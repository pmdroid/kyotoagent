use ratatui::style::{Color, Modifier, Style};

/// Text that carries meaning, over the terminal background.
pub const TEXT: Color = Color::Gray;
/// Text that is deliberately quiet: paths, hints, timestamps.
pub const MUTED: Color = Color::DarkGray;
/// A pane border that is not in focus.
pub const BORDER: Color = Color::Rgb(0x33, 0x3a, 0x4a);
/// The pane border that is in focus. Bright enough to find, dim enough to
/// sit behind the text.
pub const BORDER_FOCUSED: Color = Color::Rgb(0x4a, 0x6a, 0x7a);
/// The app's own colour: the header rule, the cursor, the working spinner.
pub const ACCENT: Color = Color::Cyan;
/// The selected session's fill. A dark wash rather than a solid block, so
/// the text on it stays readable.
pub const SELECTED_BG: Color = Color::Rgb(0x1c, 0x2c, 0x38);
/// An addition, and a result that answers the ask.
pub const GOOD: Color = Color::Green;
/// A removal, and a denied permission.
pub const BAD: Color = Color::Red;
/// Something the user has to answer.
pub const ATTENTION: Color = Color::Yellow;
/// A proof block.
pub const PROOF: Color = Color::Blue;
/// A question card.
pub const QUESTION: Color = Color::Magenta;
/// What the user asked.
pub const ASK: Color = Color::LightBlue;

/// The header bar. Only the app name is lit; the counts stay quiet so the
/// bar does not shout.
pub fn bar() -> Style {
    Style::default().fg(accent()).add_modifier(Modifier::BOLD)
}

/// A card's kind. A small caps word in the card's own colour, with no
/// background: the rail already says which card this is.
pub fn badge(color: Color) -> Style {
    Style::default()
        .fg(color)
        .add_modifier(Modifier::BOLD | Modifier::DIM)
}

/// The rail down the first line of a card.
pub fn rail(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// The rail down a card's other lines, one step quieter than the first so
/// the badge stays the strongest mark on the card.
pub fn rail_rest(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::DIM)
}

/// The selected session's rows.
pub fn selected_row() -> Style {
    Style::default().bg(selected_bg())
}

/// The card body text.
pub fn body() -> Style {
    Style::default().fg(text())
}

/// A muted line inside a card, such as a diff hunk header.
pub fn faint() -> Style {
    Style::default().fg(muted())
}

pub fn link() -> Style {
    Style::default()
        .fg(accent())
        .add_modifier(Modifier::UNDERLINED)
}

/// The pane border that is not in focus.
pub fn border() -> Style {
    Style::default().fg(border_color())
}

/// The border of the pane that has focus.
pub fn border_focused() -> Style {
    Style::default().fg(focused_color())
}

/// A pane title.
pub fn title() -> Style {
    Style::default().fg(muted()).add_modifier(Modifier::BOLD)
}

/// The input field, which is the one row that takes typing.
pub fn input() -> Style {
    Style::default().fg(text()).bg(selected_bg())
}

/// The blinking cursor drawn at the end of the input.
pub fn cursor() -> Style {
    Style::default()
        .fg(accent())
        .add_modifier(Modifier::SLOW_BLINK)
}

/// A key in a hint. It keeps its plain width so the hint still reads as
/// text, and carries the accent instead of a background.
pub fn key() -> Style {
    Style::default().fg(accent()).add_modifier(Modifier::BOLD)
}

/// A key in a hint that nothing happens to, such as the list controls.
pub fn quiet_key() -> Style {
    Style::default().fg(text()).add_modifier(Modifier::BOLD)
}

/// The word after a key in a hint.
pub fn hint() -> Style {
    Style::default().fg(muted())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}
static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
thread_local! { static OVERRIDE: std::cell::Cell<Option<Appearance>> = const { std::cell::Cell::new(None) }; }
pub fn appearance() -> Appearance {
    OVERRIDE.with(|value| value.get()).unwrap_or_else(|| {
        if LIGHT.load(std::sync::atomic::Ordering::Relaxed) {
            Appearance::Light
        } else {
            Appearance::Dark
        }
    })
}
pub fn detect() {
    use std::io::IsTerminal;
    let detected = if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let mut options = terminal_colorsaurus::QueryOptions::default();
        options.timeout = std::time::Duration::from_millis(250);
        terminal_colorsaurus::theme_mode(options)
            .ok()
            .map(|mode| match mode {
                terminal_colorsaurus::ThemeMode::Light => Appearance::Light,
                terminal_colorsaurus::ThemeMode::Dark => Appearance::Dark,
            })
    } else {
        None
    };
    let fallback = std::env::var("COLORFGBG").ok();
    let mode = detected
        .or_else(|| fallback.as_deref().and_then(environment_appearance))
        .unwrap_or(Appearance::Dark);
    LIGHT.store(
        mode == Appearance::Light,
        std::sync::atomic::Ordering::Relaxed,
    );
}
fn environment_appearance(value: &str) -> Option<Appearance> {
    let bg = value.rsplit(';').next()?.parse::<u8>().ok()?;
    match bg {
        0..=6 | 8 => Some(Appearance::Dark),
        7 | 9..=15 => Some(Appearance::Light),
        _ => None,
    }
}
fn pick(dark: Color, light: Color) -> Color {
    if appearance() == Appearance::Light {
        light
    } else {
        dark
    }
}
pub fn text() -> Color {
    pick(TEXT, Color::Rgb(0x20, 0x29, 0x35))
}
pub fn muted() -> Color {
    pick(MUTED, Color::Rgb(0x56, 0x63, 0x70))
}
pub fn border_color() -> Color {
    pick(BORDER, Color::Rgb(0xb8, 0xc2, 0xcc))
}
pub fn focused_color() -> Color {
    pick(BORDER_FOCUSED, Color::Rgb(0x59, 0x78, 0x88))
}
pub fn accent() -> Color {
    pick(ACCENT, Color::Rgb(0x00, 0x65, 0x80))
}
pub fn selected_bg() -> Color {
    pick(SELECTED_BG, Color::Rgb(0xde, 0xeb, 0xf3))
}
pub fn good() -> Color {
    pick(GOOD, Color::Rgb(0x18, 0x70, 0x36))
}
pub fn bad() -> Color {
    pick(BAD, Color::Rgb(0xb4, 0x22, 0x22))
}
pub fn attention() -> Color {
    pick(ATTENTION, Color::Rgb(0x85, 0x5a, 0x00))
}
pub fn proof() -> Color {
    pick(PROOF, Color::Rgb(0x24, 0x54, 0x9a))
}
pub fn question() -> Color {
    pick(QUESTION, Color::Rgb(0x85, 0x36, 0x8b))
}
pub fn ask() -> Color {
    pick(ASK, Color::Rgb(0x25, 0x5b, 0x92))
}
#[cfg(test)]
pub fn with_appearance<T>(mode: Appearance, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Appearance>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|value| value.set(self.0));
        }
    }
    let restore = Restore(OVERRIDE.with(|value| value.replace(Some(mode))));
    let result = f();
    drop(restore);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_theme_environment_fallback_is_bounded() {
        assert_eq!(environment_appearance("0;15"), Some(Appearance::Light));
        assert_eq!(environment_appearance("15;0"), Some(Appearance::Dark));
        assert_eq!(environment_appearance("bad"), None);
        assert_eq!(environment_appearance("0;200"), None);
    }
}
