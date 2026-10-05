use super::*;
use std::cell::RefCell;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
pub(super) fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}
pub(super) fn file_lines(path: &str, text: &str, width: usize) -> Vec<Line<'static>> {
    struct CachedFile {
        path: String,
        text: String,
        width: usize,
        appearance: theme::Appearance,
        lines: Vec<Line<'static>>,
    }
    thread_local! {
        static FILE_LINES: RefCell<Option<CachedFile>> = const { RefCell::new(None) };
    }
    FILE_LINES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let appearance = theme::appearance();
        if cache.as_ref().is_none_or(|cached| {
            cached.path != path
                || cached.text != text
                || cached.width != width
                || cached.appearance != appearance
        }) {
            *cache = Some(CachedFile {
                path: path.to_string(),
                text: text.to_string(),
                width,
                appearance,
                lines: highlight_file(path, text, width),
            });
        }
        cache.as_ref().unwrap().lines.clone()
    })
}

fn highlight_file(path: &str, text: &str, width: usize) -> Vec<Line<'static>> {
    if is_markdown(path) {
        return markdown_lines(&safe_text(text), width);
    }
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let syntaxes = SYNTAXES.get_or_init(two_face::syntax::extra_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let syntax = syntaxes
        .find_syntax_by_extension(&extension)
        .or_else(|| syntaxes.find_syntax_by_first_line(text.lines().next().unwrap_or("")))
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let theme_name = if theme::appearance() == theme::Appearance::Light {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    };
    let mut highlighter = HighlightLines::new(syntax, &themes.themes[theme_name]);
    let mut output = Vec::new();
    for raw in code_lines(text) {
        let clean = safe_text(&raw);
        let source = format!("{clean}\n");
        let spans = highlighter.highlight_line(&source, syntaxes).ok();
        let mut inks = Vec::new();
        match spans {
            Some(spans) => {
                for (ink, text) in spans {
                    let mut style = Style::default().fg(Color::Rgb(
                        ink.foreground.r,
                        ink.foreground.g,
                        ink.foreground.b,
                    ));
                    if ink.font_style.contains(FontStyle::BOLD) {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if ink.font_style.contains(FontStyle::ITALIC) {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if ink.font_style.contains(FontStyle::UNDERLINE) {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    inks.extend(
                        text.chars()
                            .filter(|ch| *ch != '\n')
                            .map(|ch| (ch, style, None)),
                    );
                }
            }
            None => inks.extend(clean.chars().map(|ch| (ch, theme::body(), None))),
        }
        output.extend(code_rows(&inks, width.max(1)));
    }
    output
}
pub(super) fn safe_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .map(|ch| {
            if ch == '\t' {
                "    ".to_string()
            } else {
                ch.to_string()
            }
        })
        .collect()
}
fn code_rows(inks: &[Ink], width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut line = Vec::new();
    let mut used = 0;
    for ink in inks {
        let cells = UnicodeWidthChar::width(ink.0).unwrap_or(0);
        if !line.is_empty() && used + cells > width {
            rows.push(painted(&line).0);
            line.clear();
            used = 0;
        }
        line.push(ink.clone());
        used += cells;
    }
    rows.push(painted(&line).0);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_code_keeps_spacing_and_colors_tokens() {
        let text = "fn main() {\n    let value = \"hello\";\n}\n";
        let lines = file_lines("MAIN.RS", text, 80);
        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
            code_lines(text)
        );
        let colors: std::collections::HashSet<_> = lines
            .iter()
            .flat_map(|line| line.spans.iter().filter_map(|span| span.style.fg))
            .collect();
        assert!(colors.len() > 1);
    }
    #[test]
    fn file_code_wraps_unicode_without_dropping_spaces() {
        let text = "  let value = \"界\";";
        let lines = file_lines("test.rs", text, 10);
        assert!(lines.iter().all(|line| line.width() <= 10));
        assert_eq!(lines.iter().map(Line::to_string).collect::<String>(), text);
    }
    #[test]
    fn file_unknown_type_is_plain_and_keeps_indentation() {
        let text = "  text  here\n\nnext";
        let lines = file_lines("file.unknown", text, 80);
        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
            code_lines(text)
        );
    }
    #[test]
    fn file_markdown_uses_the_shared_renderer() {
        let text = "# Heading\n\n**bold** and *italic*\n\n- item\n\n| Name | Value |\n| --- | --- |\n| a | b |";
        let lines = file_lines("README.MD", text, 60);
        assert_eq!(lines, markdown_lines(text, 60));
        let raw = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!raw.contains("# Heading"));
        assert!(!raw.contains("**bold**"));
        assert!(raw.contains("Heading"));
        assert!(raw.contains("item"));
    }
    #[test]
    fn file_control_codes_are_not_emitted() {
        let lines = file_lines("x.rs", "\u{1b}[31mcode\tmore", 80);
        let text = lines.iter().map(Line::to_string).collect::<String>();
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains("code    more"));
    }
    #[test]
    fn file_common_languages_have_grammars() {
        let syntaxes = two_face::syntax::extra_newlines();
        for ext in ["rs", "py", "js", "json", "yaml", "sh", "ts", "toml"] {
            assert!(
                syntaxes.find_syntax_by_extension(ext).is_some(),
                "missing {ext}"
            );
        }
    }
}
