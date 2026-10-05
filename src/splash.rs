use std::io::{self, Write};
use std::sync::OnceLock;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use png::{ColorType, Decoder};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

pub const PNG: &[u8] = include_bytes!("../assets/kyoto-dog.png");

const CELL_W: u32 = 1;
const CELL_H: u32 = 2;
const KITTY_ID: u32 = 42;
const HALF: char = '\u{2580}';

pub(crate) struct Raster {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

fn raster() -> &'static Raster {
    static RASTER: OnceLock<Raster> = OnceLock::new();
    RASTER.get_or_init(load)
}

fn load() -> Raster {
    let decoder = Decoder::new(std::io::Cursor::new(PNG));
    let mut reader = decoder.read_info().expect("the splash png header reads");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("the splash png decodes");
    let pixels = info.buffer_size();
    let rgba = match info.color_type {
        ColorType::Rgba => buf[..pixels].to_vec(),
        ColorType::Rgb => {
            let mut out = Vec::with_capacity((pixels / 3) * 4);
            for chunk in buf[..pixels].chunks(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            out
        }
        other => panic!("the splash png is {other:?}, want rgba"),
    };
    Raster {
        width: info.width,
        height: info.height,
        rgba,
    }
}

pub fn size() -> (u32, u32) {
    let img = raster();
    (img.width, img.height)
}

pub fn shown(cards: usize) -> bool {
    cards == 0
}

pub fn rect_in(pane: Rect) -> Rect {
    let img = raster();
    fit(pane, img.width, img.height)
}

pub(crate) fn fit(pane: Rect, img_w: u32, img_h: u32) -> Rect {
    if pane.width == 0 || pane.height == 0 || img_w == 0 || img_h == 0 {
        return Rect::new(pane.x, pane.y, 0, 0);
    }
    let pane_w = u32::from(pane.width);
    let pane_h = u32::from(pane.height);
    let (w, h) = if pane_w * img_h * CELL_W > pane_h * img_w * CELL_H {
        let h = pane_h;
        let w = (h * img_w * CELL_H / (img_h * CELL_W)).max(1).min(pane_w);
        (w, h)
    } else {
        let w = pane_w;
        let h = (w * img_h * CELL_W / (img_w * CELL_H)).max(1).min(pane_h);
        (w, h)
    };
    let w = w as u16;
    let h = h as u16;
    Rect {
        x: pane.x + (pane.width.saturating_sub(w)) / 2,
        y: pane.y + (pane.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

pub fn render(area: Rect, buf: &mut Buffer) {
    let dest = rect_in(area).intersection(area).intersection(buf.area);
    if dest.width == 0 || dest.height == 0 {
        return;
    }
    let img = raster();
    let px_w = u32::from(dest.width);
    let px_h = u32::from(dest.height) * 2;
    for row in 0..dest.height {
        for col in 0..dest.width {
            let top = sample(img, u32::from(col), u32::from(row) * 2, px_w, px_h);
            let bot = sample(img, u32::from(col), u32::from(row) * 2 + 1, px_w, px_h);
            if dark(top) && dark(bot) {
                continue;
            }
            let cell = &mut buf[(dest.x + col, dest.y + row)];
            cell.set_char(HALF);
            cell.set_fg(Color::Rgb(top.0, top.1, top.2));
            cell.set_bg(Color::Rgb(bot.0, bot.1, bot.2));
        }
    }
}

pub(crate) fn sample(img: &Raster, x: u32, y: u32, dest_w: u32, dest_h: u32) -> (u8, u8, u8) {
    let sx = (x * img.width / dest_w.max(1)).min(img.width - 1);
    let sy = (y * img.height / dest_h.max(1)).min(img.height - 1);
    let i = ((sy * img.width + sx) * 4) as usize;
    premul(
        img.rgba[i],
        img.rgba[i + 1],
        img.rgba[i + 2],
        img.rgba[i + 3],
    )
}

fn premul(r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8) {
    match a {
        255 => (r, g, b),
        0 => (0, 0, 0),
        a => {
            let a = u16::from(a);
            (
                (u16::from(r) * a / 255) as u8,
                (u16::from(g) * a / 255) as u8,
                (u16::from(b) * a / 255) as u8,
            )
        }
    }
}

fn dark(c: (u8, u8, u8)) -> bool {
    u16::from(c.0) + u16::from(c.1) + u16::from(c.2) < 24
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    ITerm,
    HalfBlocks,
}

pub fn detect() -> Protocol {
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("kitty") || term.contains("ghostty") {
        return Protocol::Kitty;
    }
    if std::env::var("KITTY_WINDOW_ID").is_ok() {
        return Protocol::Kitty;
    }
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if program == "ghostty" {
        return Protocol::Kitty;
    }
    if program == "iTerm.app" || program == "WezTerm" || program == "WarpTerminal" {
        return Protocol::ITerm;
    }
    Protocol::HalfBlocks
}

pub struct Splash {
    protocol: Protocol,
    dest: Option<Rect>,
}

impl Splash {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            dest: None,
        }
    }

    pub fn detect() -> Self {
        Self::new(detect())
    }

    pub fn uses_graphics(&self) -> bool {
        matches!(self.protocol, Protocol::Kitty | Protocol::ITerm)
    }

    pub fn sync<W: Write>(&mut self, out: &mut W, pane: Rect, show: bool) -> io::Result<()> {
        let dest = rect_in(pane);
        let show = show && dest.width > 0 && dest.height > 0;
        match self.protocol {
            Protocol::Kitty => self.sync_kitty(out, dest, show),
            Protocol::ITerm => {
                if show {
                    paint_iterm(out, dest)
                } else {
                    Ok(())
                }
            }
            Protocol::HalfBlocks => Ok(()),
        }
    }

    fn sync_kitty<W: Write>(&mut self, out: &mut W, dest: Rect, show: bool) -> io::Result<()> {
        if show {
            if self.dest == Some(dest) {
                return Ok(());
            }
            if self.dest.is_some() {
                clear_kitty(out)?;
            }
            paint_kitty(out, dest)?;
            self.dest = Some(dest);
            Ok(())
        } else if self.dest.take().is_some() {
            clear_kitty(out)
        } else {
            Ok(())
        }
    }
}

pub fn blank(area: Rect, buf: &mut Buffer) {
    let dest = rect_in(area).intersection(area).intersection(buf.area);
    if dest.width == 0 || dest.height == 0 {
        return;
    }
    for row in 0..dest.height {
        for col in 0..dest.width {
            buf[(dest.x + col, dest.y + row)].reset();
        }
    }
}

fn paint_kitty<W: Write>(out: &mut W, dest: Rect) -> io::Result<()> {
    if dest.width == 0 || dest.height == 0 {
        return Ok(());
    }
    paint_kitty_image(out, dest, PNG, KITTY_ID)
}

pub(crate) fn paint_kitty_image<W: Write>(
    out: &mut W,
    dest: Rect,
    png: &[u8],
    id: u32,
) -> io::Result<()> {
    queue_move(out, dest)?;
    let payload = STANDARD.encode(png);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(3840).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = i + 1 < chunks.len();
        let m = u8::from(more);
        if i == 0 {
            write!(
                out,
                "\x1b_Ga=T,f=100,i={id},q=2,c={},r={},C=1,m={m};",
                dest.width, dest.height
            )?;
        } else {
            write!(out, "\x1b_Gm={m};")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    out.flush()
}

fn clear_kitty<W: Write>(out: &mut W) -> io::Result<()> {
    clear_kitty_image(out, KITTY_ID)
}

pub(crate) fn clear_kitty_image<W: Write>(out: &mut W, id: u32) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=i,i={id},q=2\x1b\\")?;
    out.flush()
}

fn paint_iterm<W: Write>(out: &mut W, dest: Rect) -> io::Result<()> {
    if dest.width == 0 || dest.height == 0 {
        return Ok(());
    }
    paint_iterm_image(out, dest, PNG)
}

pub(crate) fn paint_iterm_image<W: Write>(out: &mut W, dest: Rect, png: &[u8]) -> io::Result<()> {
    queue_move(out, dest)?;
    let payload = STANDARD.encode(png);
    write!(
        out,
        "\x1b]1337;File=inline=1;width={};height={};preserveAspectRatio=1;doNotMoveCursor=1:{payload}",
        dest.width, dest.height
    )?;
    out.write_all(&[0x07])?;
    out.flush()
}

fn queue_move<W: Write>(out: &mut W, dest: Rect) -> io::Result<()> {
    write!(out, "\x1b[{};{}H", dest.y + 1, dest.x + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;
    use crate::screen::{session_inner, Card};

    #[test]
    fn the_png_is_the_retriever_head() {
        let (w, h) = size();
        assert_eq!(w, 1254);
        assert_eq!(h, 1254);
        let img = raster();
        let corner = sample(img, 0, 0, w, h);
        assert!(dark(corner), "the field is black: {corner:?}");
        let probes = [
            sample(img, w / 2, h / 3, w, h),
            sample(img, w / 3, h / 2, w, h),
            sample(img, (w * 2) / 3, h / 2, w, h),
        ];
        assert!(
            probes.iter().any(|c| !dark(*c)),
            "the head is in the drawing: {probes:?}"
        );
    }

    #[test]
    fn the_splash_rectangle_is_centred_in_the_right_pane() {
        let screen = Rect::new(0, 0, 76, 24);
        let pane = session_inner(screen);
        assert!(
            pane.x >= 30,
            "the image sits in the session pane, not the list"
        );
        let splash = rect_in(pane);
        assert!(splash.width > 0);
        assert!(splash.height > 0);
        assert!(splash.x >= pane.x);
        assert!(splash.y >= pane.y);
        assert!(splash.x + splash.width <= pane.x + pane.width);
        assert!(splash.y + splash.height <= pane.y + pane.height);
        let left = i32::from(splash.x.saturating_sub(pane.x));
        let right = i32::from((pane.x + pane.width).saturating_sub(splash.x + splash.width));
        assert!((left - right).abs() <= 1, "left {left} right {right}");
        let top = i32::from(splash.y.saturating_sub(pane.y));
        let bottom = i32::from((pane.y + pane.height).saturating_sub(splash.y + splash.height));
        assert!((top - bottom).abs() <= 1, "top {top} bottom {bottom}");
        assert_eq!(
            u32::from(splash.width) * 1254 * CELL_W,
            u32::from(splash.height) * 1254 * CELL_H
        );
    }

    #[test]
    fn a_view_with_one_ask_has_no_splash() {
        assert!(shown(mock::empty().cards.len()));
        assert!(!shown(mock::working().cards.len()));
        let mut fresh = mock::empty();
        fresh.sessions = mock::working().sessions.clone();
        fresh.selected = mock::working().selected.clone();
        fresh.cards = Vec::new();
        assert!(shown(fresh.cards.len()));
        fresh.cards = vec![Card::ask("hello")];
        assert!(!shown(fresh.cards.len()));
    }

    #[test]
    fn kitty_sends_the_png() {
        let mut out = Vec::new();
        paint_kitty(&mut out, Rect::new(10, 4, 20, 20)).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("a=T"));
        assert!(text.contains("f=100"));
        assert!(text.contains("c=20"));
        assert!(text.contains("r=20"));
        assert!(text.contains(&STANDARD.encode(PNG)[..32]));
    }

    fn kitty_bytes(splash: &mut Splash, pane: Rect, show: bool) -> String {
        let mut out = Vec::new();
        splash.sync(&mut out, pane, show).unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    fn uploads(text: &str) -> usize {
        text.matches("\u{1b}_Ga=T").count()
    }

    fn deletes(text: &str) -> usize {
        text.matches("\u{1b}_Ga=d").count()
    }

    #[test]
    fn a_second_kitty_sync_on_the_same_pane_is_silent() {
        let pane = session_inner(Rect::new(0, 0, 76, 24));
        let mut splash = Splash::new(Protocol::Kitty);
        let first = kitty_bytes(&mut splash, pane, true);
        assert_eq!(uploads(&first), 1);
        assert_eq!(deletes(&first), 0);
        let second = kitty_bytes(&mut splash, pane, true);
        assert_eq!(uploads(&second), 0);
        assert_eq!(deletes(&second), 0);
        assert!(second.is_empty());
    }

    #[test]
    fn hiding_the_splash_deletes_the_kitty_image() {
        let pane = session_inner(Rect::new(0, 0, 76, 24));
        let mut splash = Splash::new(Protocol::Kitty);
        let _ = kitty_bytes(&mut splash, pane, true);
        let hide = kitty_bytes(&mut splash, pane, false);
        assert_eq!(deletes(&hide), 1);
        assert_eq!(uploads(&hide), 0);
        let again = kitty_bytes(&mut splash, pane, false);
        assert!(again.is_empty());
    }

    #[test]
    fn a_resized_pane_replaces_the_kitty_image() {
        let small = session_inner(Rect::new(0, 0, 76, 24));
        let large = session_inner(Rect::new(0, 0, 120, 40));
        let mut splash = Splash::new(Protocol::Kitty);
        let first = kitty_bytes(&mut splash, small, true);
        let dest = rect_in(large);
        assert_ne!(rect_in(small), dest);
        let resized = kitty_bytes(&mut splash, large, true);
        assert_eq!(deletes(&resized), 1);
        assert_eq!(uploads(&resized), 1);
        assert!(resized.contains(&format!("c={},r={}", dest.width, dest.height)));
        assert_eq!(uploads(&first), 1);
        assert_eq!(deletes(&first), 0);
    }

    #[test]
    fn a_live_graphics_image_does_not_keep_half_blocks() {
        let area = Rect::new(0, 0, 76, 24);
        let pane = session_inner(area);
        let mut buf = Buffer::empty(area);
        render(pane, &mut buf);
        assert!(
            (pane.x..pane.x + pane.width).any(|x| {
                (pane.y..pane.y + pane.height).any(|y| buf[(x, y)].symbol() == HALF.to_string())
            }),
            "the empty pane draws the dog"
        );
        blank(pane, &mut buf);
        assert!(
            (pane.x..pane.x + pane.width).all(|x| {
                (pane.y..pane.y + pane.height).all(|y| buf[(x, y)].symbol() != HALF.to_string())
            }),
            "a wiped frame is empty until the protocol paints"
        );
    }
}
