use crate::{
    attachment::ImageAttachment,
    splash::{self, Protocol, Raster},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, BorderType, Clear},
    Frame,
};
use std::io::{self, Cursor, Write};

const IMAGE_ID: u32 = 43;

pub(crate) fn popup(pane: Rect) -> Rect {
    Rect::new(
        pane.x + 1,
        pane.y + 1,
        pane.width.saturating_sub(2),
        pane.height.saturating_sub(2),
    )
}

fn decoded(image: &ImageAttachment) -> Option<(Raster, image::DynamicImage)> {
    image.validate().ok()?;
    let bytes = STANDARD.decode(&image.data).ok()?;
    let pixels = image::load_from_memory(&bytes).ok()?;
    let rgba = pixels.to_rgba8();
    let raster = Raster {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    };
    Some((raster, pixels))
}

pub(crate) fn render(image: &ImageAttachment, pane: Rect, frame: &mut Frame) {
    let area = popup(pane);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", image.name));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if area.width > 0 && area.height > 0 {
        frame.buffer_mut()[(area.x + area.width - 1, area.y)].set_symbol("x");
    }
    if splash::detect() != Protocol::HalfBlocks {
        return;
    }
    let Some((raster, _)) = decoded(image) else {
        return;
    };
    let dest = splash::fit(inner, raster.width, raster.height);
    for row in 0..dest.height {
        for col in 0..dest.width {
            let top = splash::sample(
                &raster,
                u32::from(col),
                u32::from(row) * 2,
                u32::from(dest.width),
                u32::from(dest.height) * 2,
            );
            let bottom = splash::sample(
                &raster,
                u32::from(col),
                u32::from(row) * 2 + 1,
                u32::from(dest.width),
                u32::from(dest.height) * 2,
            );
            frame.buffer_mut()[(dest.x + col, dest.y + row)]
                .set_char('▀')
                .set_style(
                    Style::default()
                        .fg(Color::Rgb(top.0, top.1, top.2))
                        .bg(Color::Rgb(bottom.0, bottom.1, bottom.2)),
                );
        }
    }
}

pub(crate) struct Preview {
    protocol: Protocol,
    image: Option<ImageAttachment>,
    raster: Option<Raster>,
    png: Vec<u8>,
    dest: Option<Rect>,
}

impl Preview {
    pub(crate) fn new() -> Self {
        Self {
            protocol: splash::detect(),
            image: None,
            raster: None,
            png: Vec::new(),
            dest: None,
        }
    }

    pub(crate) fn sync<W: Write>(
        &mut self,
        out: &mut W,
        image: Option<&ImageAttachment>,
        pane: Rect,
    ) -> io::Result<()> {
        if self.image.as_ref() != image {
            if self.dest.take().is_some() && self.protocol == Protocol::Kitty {
                splash::clear_kitty_image(out, IMAGE_ID)?;
            }
            self.image = image.cloned();
            self.raster = None;
            self.png.clear();
            if let Some((raster, pixels)) = image.and_then(decoded) {
                let mut png = Cursor::new(Vec::new());
                pixels
                    .write_to(&mut png, image::ImageFormat::Png)
                    .map_err(io::Error::other)?;
                self.raster = Some(raster);
                self.png = png.into_inner();
            }
        }
        let Some(raster) = &self.raster else {
            return Ok(());
        };
        let inner = Block::bordered().inner(popup(pane));
        let dest = splash::fit(inner, raster.width, raster.height);
        if dest.width == 0 || dest.height == 0 {
            return Ok(());
        }
        match self.protocol {
            Protocol::Kitty if self.dest != Some(dest) => {
                if self.dest.is_some() {
                    splash::clear_kitty_image(out, IMAGE_ID)?;
                }
                splash::paint_kitty_image(out, dest, &self.png, IMAGE_ID)?;
                self.dest = Some(dest);
            }
            Protocol::ITerm => splash::paint_iterm_image(out, dest, &self.png)?,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_preview_transmits_actual_image_and_clears_on_close() {
        let image = ImageAttachment::from_bytes("dog.png", splash::PNG).unwrap();
        let mut preview = Preview::new();
        preview.protocol = Protocol::Kitty;
        let mut out = Vec::new();
        preview
            .sync(&mut out, Some(&image), Rect::new(0, 0, 80, 24))
            .unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(text.contains("a=T,f=100,i=43"));
        let transmitted: String = text
            .split("\x1b_G")
            .skip(1)
            .filter_map(|chunk| chunk.split_once(';'))
            .map(|(_, payload)| payload.split("\x1b\\").next().unwrap())
            .collect();
        let png = STANDARD.decode(transmitted).unwrap();
        let pixels = image::load_from_memory(&png).unwrap();
        assert_eq!(
            pixels.to_rgba8(),
            image::load_from_memory(splash::PNG).unwrap().to_rgba8()
        );
        out.clear();
        preview
            .sync(&mut out, None, Rect::new(0, 0, 80, 24))
            .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("a=d,d=i,i=43"));
    }

    #[test]
    fn iterm_preview_transmits_inline_png() {
        let image = ImageAttachment::from_bytes("dog.png", splash::PNG).unwrap();
        let mut preview = Preview::new();
        preview.protocol = Protocol::ITerm;
        let mut out = Vec::new();
        preview
            .sync(&mut out, Some(&image), Rect::new(0, 0, 80, 24))
            .unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("1337;File=inline=1;"));
    }
}
