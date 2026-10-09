use super::*;
use pulldown_cmark::Alignment as ColumnAlign;

const TABLE_GAP: usize = 2;
const TABLE_COLUMN_FLOOR: usize = 4;
const TABLE_SHORT_COLUMN: usize = 12;

impl Card {
    pub fn ask(text: &str) -> Card {
        Card::Ask {
            images: Vec::new(),
            text: text.to_string(),
        }
    }

    pub fn question(text: &str, choices: &[(&str, bool)]) -> Card {
        Card::Question {
            text: text.to_string(),
            choices: choices
                .iter()
                .map(|(label, marked)| Choice {
                    label: (*label).to_string(),
                    marked: *marked,
                })
                .collect(),
            answer: None,
            visuals: Vec::new(),
        }
    }

    pub fn answer(text: &str) -> Card {
        Card::Answer {
            text: text.to_string(),
        }
    }

    pub fn permission(action: &str, decision: Option<&str>, diff: &[&str]) -> Card {
        Card::Permission {
            action: action.to_string(),
            decision: decision.map(str::to_string),
            diff: diff.iter().map(|line| (*line).to_string()).collect(),
            argv: Vec::new(),
        }
    }

    /// A permission on a command rather than on a write. The model never types
    /// the command itself: the harness builds it, so the card shows the argv it
    /// is about to run under `sh -c`.
    pub fn command(action: &str, decision: Option<&str>, argv: &[&str]) -> Card {
        Card::Permission {
            action: action.to_string(),
            decision: decision.map(str::to_string),
            diff: Vec::new(),
            argv: argv.iter().map(|line| (*line).to_string()).collect(),
        }
    }

    pub fn result(text: &str) -> Card {
        Card::Result {
            text: text.to_string(),
        }
    }

    pub fn proof(text: &str, items: &[(&str, ItemKind, Outcome)]) -> Card {
        Card::Proof {
            text: text.to_string(),
            items: items
                .iter()
                .map(|(id, kind, outcome)| ItemRun {
                    id: (*id).to_string(),
                    kind: *kind,
                    outcome: *outcome,
                    argv: Vec::new(),
                    exit: None,
                    tail: String::new(),
                })
                .collect(),
        }
    }

    /// The card's kind, shown as a badge above the card.
    pub fn label(&self) -> &'static str {
        match self {
            Card::Ask { .. } => "ask",
            Card::Question { .. } => "question",
            Card::Answer { .. } => "answer",
            Card::Permission { .. } => "permission",
            Card::Result { .. } => "result",
            Card::Proof { .. } => "proof",
            Card::Artifact { .. } => "artifact",
            Card::Enhance { .. } => "enhance",
        }
    }

    /// The card's rail colour. A permission takes the colour of its answer, so
    /// an allowed write reads differently from a denied one.
    pub fn color(&self) -> Color {
        match self {
            Card::Ask { .. } | Card::Answer { .. } => theme::ask(),
            Card::Question { .. } => theme::question(),
            Card::Result { .. } => theme::good(),
            Card::Proof { .. } | Card::Artifact { .. } => theme::proof(),
            Card::Enhance { .. } => theme::attention(),
            Card::Permission { decision, .. } => match decision.as_deref() {
                Some(text) if text.starts_with("Allowed") => theme::good(),
                Some(_) => theme::bad(),
                None => theme::attention(),
            },
        }
    }

    pub fn is_waiting(&self) -> bool {
        match self {
            Card::Question {
                choices, answer, ..
            } => answer.is_none() && choices.iter().all(|choice| !choice.marked),
            Card::Permission { decision, .. } => decision.is_none(),
            Card::Enhance { .. } => true,
            _ => false,
        }
    }

    /// The card's lines, each one behind the rail.
    ///
    /// `width` is the whole card including the rail. The first line carries the
    /// lit rail and the badge; the rest carry a dim rail.
    fn clip_preview(&self, lines: &mut Vec<Line<'static>>) {
        if self.preview_text().is_some() {
            lines.truncate(8);
            lines.push(Line::from(Span::styled("Show complete text", theme::key())));
        }
    }

    pub fn preview_text(&self) -> Option<&str> {
        let text = match self {
            Card::Result { text } | Card::Proof { text, .. } | Card::Question { text, .. } => text,
            _ => return None,
        };
        (text.chars().take(2049).count() > 2048 || text.lines().take(13).count() > 12)
            .then_some(text.as_str())
    }

    pub fn lines(&self, pane_width: usize) -> Vec<Line<'static>> {
        let pane_width = pane_width.max(1);
        let width = column_width(self, pane_width);
        if matches!(self, Card::Answer { .. }) {
            return answer_lines(self, pane_width);
        }
        let color = self.color();
        let text_width = width.saturating_sub(RAIL.chars().count());
        let mut out = vec![Line::from(vec![
            Span::styled(RAIL, theme::rail(color)),
            Span::styled(self.label().to_uppercase(), theme::badge(color)),
        ])];
        let mut body: Vec<Line<'static>> = Vec::new();
        match self {
            Card::Ask { text, images } => {
                let mut lines = ask_on_the_right(text, pane_width - width, text_width, color);
                for image in images {
                    lines.push(rail_right(
                        pane_width - width,
                        text_width,
                        Span::styled(
                            format!(
                                "▧ {}",
                                truncate_cols(&image.name, text_width.saturating_sub(2))
                            ),
                            theme::key(),
                        ),
                        Span::styled(RAIL_RIGHT, theme::rail_rest(color)),
                    ));
                }
                return lines;
            }
            Card::Result { text } | Card::Proof { text, .. } => {
                body.extend(markdown_lines_between(
                    preview_source(text),
                    text_width,
                    pane_width.saturating_sub(RAIL.chars().count()),
                ));
                self.clip_preview(&mut body);
                if let Card::Proof { items, .. } = self {
                    for item in items
                        .iter()
                        .filter(|item| item.outcome == Outcome::PassedEarlier)
                    {
                        body.extend(wrapped(
                            &format!("✓ {} (earlier)", item.id),
                            text_width,
                            Style::default().fg(theme::good()),
                        ));
                    }
                }
            }
            Card::Artifact {
                file,
                caption,
                focused,
            } => {
                body.extend(wrapped(
                    caption.as_deref().unwrap_or(&file.name),
                    text_width,
                    if *focused {
                        theme::key()
                    } else {
                        theme::body()
                    },
                ));
                if caption.is_some() {
                    body.extend(wrapped(&file.name, text_width, theme::body()));
                }
                body.extend(wrapped(
                    &format!("{} · {} bytes", file.media_type, file.size),
                    text_width,
                    theme::faint(),
                ));
                body.extend(wrapped(
                    if *focused { "[Open]" } else { "Open" },
                    text_width,
                    theme::key(),
                ));
            }
            Card::Answer { .. } => {}
            Card::Permission {
                decision,
                action,
                diff,
                argv,
            } => {
                let headline = decision.as_deref().unwrap_or(action);
                body.extend(wrapped(headline, text_width, style_for(self, headline)));
                for line in diff {
                    body.extend(wrapped(line, text_width, diff_style(line)));
                }
                body.extend(argv_lines(argv, text_width));
            }
            Card::Enhance {
                source,
                text,
                error,
                ..
            } => {
                body.extend(wrapped(source, text_width, theme::faint()));
                if !text.is_empty() {
                    body.extend(wrapped(text, text_width, theme::body()));
                }
                if let Some(error) = error {
                    body.extend(wrapped(error, text_width, theme::faint()));
                }
            }
            Card::Question {
                text,
                choices,
                answer,
                visuals,
            } => {
                body.extend(markdown_lines(preview_source(text), text_width));
                self.clip_preview(&mut body);
                for (index, visual) in visuals.iter().enumerate() {
                    body.extend(wrapped(&visual.title, text_width, theme::body()));
                    body.extend(wrapped(&visual.alt, text_width, theme::faint()));
                    if answer.is_none() {
                        body.extend(wrapped(
                            if index == 0 {
                                "Ctrl-V · View diagram"
                            } else {
                                "Ctrl-V · Next diagram"
                            },
                            text_width,
                            theme::key(),
                        ));
                    }
                }
                if answer.is_none() {
                    for (index, choice) in choices.iter().enumerate() {
                        let marker = if choice.marked {
                            "\u{25cf}"
                        } else {
                            "\u{25cb}"
                        };
                        let style = if choice.marked {
                            Style::default()
                                .fg(theme::good())
                                .add_modifier(Modifier::BOLD)
                        } else {
                            theme::faint()
                        };
                        body.push(Line::from(Span::styled(
                            format!("{marker} {}  {}", index + 1, choice.label),
                            style,
                        )));
                    }
                }
            }
        }
        for line in body {
            let mut spans = vec![Span::styled(RAIL_DIM, theme::rail_rest(color))];
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
        out
    }
}

fn preview_source(text: &str) -> &str {
    let end = text
        .char_indices()
        .nth(2048)
        .map_or(text.len(), |(index, _)| index);
    let text = &text[..end];
    let end = text.split_inclusive('\n').take(12).map(str::len).sum();
    &text[..end]
}

pub(super) fn column_width(card: &Card, pane_width: usize) -> usize {
    if pane_width == 0 {
        return 0;
    }
    match card {
        Card::Ask { .. } | Card::Result { .. } | Card::Proof { .. } | Card::Artifact { .. } => {
            (pane_width * 2 / 3).max(1)
        }
        _ => pane_width,
    }
}

pub(super) fn ask_on_the_right(
    text: &str,
    offset: usize,
    text_width: usize,
    color: Color,
) -> Vec<Line<'static>> {
    let mut out = vec![rail_right(
        offset,
        text_width,
        Span::styled("ASK", theme::badge(color)),
        Span::styled(RAIL_RIGHT, theme::rail(color)),
    )];
    for line in wrapped(text, text_width, theme::body()) {
        let body = line
            .spans
            .into_iter()
            .next()
            .unwrap_or_else(|| Span::styled(String::new(), theme::body()));
        out.push(rail_right(
            offset,
            text_width,
            body,
            Span::styled(RAIL_RIGHT, theme::rail_rest(color)),
        ));
    }
    out
}

pub(super) fn rail_right(
    offset: usize,
    text_width: usize,
    text: Span<'static>,
    rail: Span<'static>,
) -> Line<'static> {
    let used = UnicodeWidthStr::width(text.content.as_ref());
    let lead = offset + text_width.saturating_sub(used);
    let mut spans = Vec::new();
    if lead > 0 {
        spans.push(Span::raw(" ".repeat(lead)));
    }
    spans.push(text);
    spans.push(rail);
    Line::from(spans)
}

/// The colour of a permission's headline, which follows its answer.
pub(super) fn style_for(card: &Card, headline: &str) -> Style {
    match card {
        Card::Permission { decision, .. } if decision.is_some() => {
            if headline.starts_with("Allowed") {
                Style::default()
                    .fg(theme::good())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::bad())
                    .add_modifier(Modifier::BOLD)
            }
        }
        _ => theme::body(),
    }
}

/// A diff line: an addition or a removal takes its colour, the hunk header is
/// quiet, and a context line is body text.
pub(super) fn diff_style(line: &str) -> Style {
    if line.starts_with('+') {
        Style::default().fg(theme::good())
    } else if line.starts_with('-') {
        Style::default().fg(theme::bad())
    } else if line.starts_with('@') {
        theme::faint()
    } else {
        theme::body()
    }
}

/// The command a permission is about to run. It is quoted here because the
/// model never typed it: the harness built it, so the user is reading it for
/// the first time.
pub(super) fn argv_style() -> Style {
    Style::default()
        .fg(theme::text())
        .add_modifier(Modifier::ITALIC)
}

fn markdown_events(text: &str) -> Parser<'_> {
    Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    )
}

pub(super) fn markdown_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    markdown_lines_between(text, width, width)
}

pub(super) fn markdown_lines_between(text: &str, prose: usize, table: usize) -> Vec<Line<'static>> {
    let mut paint = Paint::new(prose);
    paint.table_width = table;
    for event in markdown_events(text) {
        paint.event(event);
    }
    paint.finish()
}

pub(super) struct Paint {
    width: usize,
    table_width: usize,
    lines: Vec<Line<'static>>,
    inline: Vec<Piece>,
    bits: Face,
    heading: bool,
    lists: Vec<ListCursor>,
    items: Vec<OpenItem>,
    code: Option<String>,
    links: Vec<PendingLink>,
    labels: Vec<Vec<Piece>>,
    hits: Vec<Vec<Hit>>,
    table: Option<OpenTable>,
    quotes: u32,
}

struct OpenTable {
    align: Vec<ColumnAlign>,
    rows: Vec<TableRow>,
    cell: Vec<Piece>,
    in_cell: bool,
}

struct TableRow {
    cells: Vec<Vec<Piece>>,
    header: bool,
}

pub(super) struct Face {
    bold: u32,
    italic: u32,
    code: u32,
    strike: u32,
}

pub(super) struct ListCursor {
    next: Option<u64>,
}

pub(super) struct OpenItem {
    marker: Option<String>,
    used: bool,
    task: Option<bool>,
}

pub(super) enum Piece {
    Text {
        text: String,
        style: Style,
        url: Option<String>,
    },
    Break,
}

pub(super) struct Hit {
    start: usize,
    end: usize,
    url: String,
}

pub(super) struct PendingLink {
    url: String,
    image: bool,
}

impl Paint {
    fn new(width: usize) -> Paint {
        Paint {
            width,
            table_width: width,
            lines: Vec::new(),
            inline: Vec::new(),
            bits: Face {
                bold: 0,
                italic: 0,
                code: 0,
                strike: 0,
            },
            heading: false,
            lists: Vec::new(),
            items: Vec::new(),
            code: None,
            links: Vec::new(),
            labels: Vec::new(),
            hits: Vec::new(),
            table: None,
            quotes: 0,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some(buf) = self.code.as_mut() {
                    buf.push_str(&text);
                } else {
                    self.push_text(&text);
                }
            }
            Event::Code(text) => {
                self.bits.code += 1;
                self.push_text(&text);
                self.bits.code -= 1;
            }
            Event::Html(text) | Event::InlineHtml(text) => self.push_text(&text),
            Event::SoftBreak => {
                if self.code.is_none() {
                    self.push_text(" ");
                }
            }
            Event::HardBreak => {
                if self.code.is_none() {
                    self.push(Piece::Break);
                }
            }
            Event::Rule => self.rule(),
            Event::TaskListMarker(checked) => self.task_marker(checked),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { .. } => self.heading = true,
            Tag::List(next) => {
                if !self.inline.is_empty() {
                    self.emit_prose();
                }
                if self.items.is_empty() {
                    self.separate();
                }
                self.lists.push(ListCursor { next });
            }
            Tag::Item => self.items.push(OpenItem {
                marker: None,
                used: false,
                task: None,
            }),
            Tag::CodeBlock(_) => {
                if !self.inline.is_empty() {
                    self.emit_prose();
                }
                self.code = Some(String::new());
            }
            Tag::Link { dest_url, .. } => {
                self.links.push(PendingLink {
                    url: dest_url.to_string(),
                    image: false,
                });
                self.labels.push(Vec::new());
            }
            Tag::Image { dest_url, .. } => {
                self.links.push(PendingLink {
                    url: dest_url.to_string(),
                    image: true,
                });
                self.labels.push(Vec::new());
            }
            Tag::Strong => self.bits.bold += 1,
            Tag::Emphasis => self.bits.italic += 1,
            Tag::Strikethrough => self.bits.strike += 1,
            Tag::BlockQuote(_) => self.quotes = self.quotes.saturating_add(1),
            Tag::Table(align) => self.open_table(align),
            Tag::TableHead => self.open_table_row(true),
            Tag::TableRow => self.open_table_row(false),
            Tag::TableCell => self.open_table_cell(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                if self.table.is_some() {
                    return;
                }
                let heading = matches!(tag, TagEnd::Heading(_));
                self.emit_prose();
                if heading {
                    self.heading = false;
                }
            }
            TagEnd::Item => {
                if !self.inline.is_empty() {
                    self.emit_prose();
                }
                if self.items.last().is_some_and(|item| !item.used) {
                    let prefix = self.item_prefix();
                    self.push_row(Line::from(Span::styled(prefix, theme::body())), Vec::new());
                }
                self.items.pop();
            }
            TagEnd::List(_) => {
                self.lists.pop();
            }
            TagEnd::CodeBlock => {
                let raw = self.code.take().unwrap_or_default();
                self.emit_code(raw);
            }
            TagEnd::Link | TagEnd::Image => self.end_link(),
            TagEnd::Strong => self.bits.bold = self.bits.bold.saturating_sub(1),
            TagEnd::Emphasis => self.bits.italic = self.bits.italic.saturating_sub(1),
            TagEnd::Strikethrough => self.bits.strike = self.bits.strike.saturating_sub(1),
            TagEnd::BlockQuote(_) => self.quotes = self.quotes.saturating_sub(1),
            TagEnd::TableCell => self.close_table_cell(),
            TagEnd::TableHead => self.close_table_head(),
            TagEnd::Table => self.emit_table(),
            _ => {}
        }
    }

    fn close(&mut self) {
        if self.table.is_some() {
            self.emit_table();
        }
        if let Some(raw) = self.code.take() {
            self.emit_code(raw);
        }
        if !self.inline.is_empty() {
            self.emit_prose();
        }
        if self.lines.is_empty() {
            self.push_row(Line::from(""), Vec::new());
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.close();
        self.lines
    }

    fn link_on(mut self, line_index: usize, column: usize) -> Option<String> {
        self.close();
        self.hits.get(line_index)?.iter().find_map(|hit| {
            if column >= hit.start && column < hit.end {
                Some(hit.url.clone())
            } else {
                None
            }
        })
    }

    fn push_row(&mut self, mut line: Line<'static>, mut hits: Vec<Hit>) {
        if self.quotes > 0 && line_has_ink(&line) {
            let mark = self.quote_mark();
            let shift = cols(&mark);
            for hit in &mut hits {
                hit.start += shift;
                hit.end += shift;
            }
            let mut spans = vec![Span::styled(mark, theme::faint())];
            spans.append(&mut line.spans);
            line.spans = spans;
        }
        self.lines.push(line);
        self.hits.push(hits);
    }

    fn quote_mark(&self) -> String {
        "\u{258e} ".repeat(self.quotes as usize)
    }

    fn content_width(&self) -> usize {
        self.width.saturating_sub(cols(&self.quote_mark()))
    }

    fn table_room(&self) -> usize {
        self.table_width.saturating_sub(cols(&self.quote_mark()))
    }

    fn face(&self) -> Style {
        let mut style = if self.bits.code > 0 {
            code_style()
        } else {
            theme::body()
        };
        if self.bits.bold > 0 || self.heading {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.bits.italic > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.bits.strike > 0 {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.labels.is_empty() || self.bits.code > 0 {
            self.push_face(text);
            return;
        }
        let mut rest = text;
        while let Some((start, end)) = bare_url_span(rest) {
            if start > 0 {
                self.push_face(&rest[..start]);
            }
            let url = &rest[start..end];
            self.push(Piece::Text {
                text: url.to_string(),
                style: linked_style(self.face()),
                url: http_url(url),
            });
            rest = &rest[end..];
        }
        if !rest.is_empty() {
            self.push_face(rest);
        }
    }

    fn push_face(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.push(Piece::Text {
            text: text.to_string(),
            style: self.face(),
            url: None,
        });
    }

    fn piece_dest(&mut self) -> &mut Vec<Piece> {
        if !self.labels.is_empty() {
            let last = self.labels.len() - 1;
            return &mut self.labels[last];
        }
        if let Some(table) = self.table.as_mut() {
            if table.in_cell {
                return &mut table.cell;
            }
        }
        &mut self.inline
    }

    fn push(&mut self, piece: Piece) {
        let dest = self.piece_dest();
        match (dest.last_mut(), &piece) {
            (
                Some(Piece::Text { text, style, url }),
                Piece::Text {
                    text: more,
                    style: next,
                    url: next_url,
                },
            ) if *style == *next && *url == *next_url => {
                text.push_str(more);
            }
            _ => dest.push(piece),
        }
    }

    fn end_link(&mut self) {
        let label = self.labels.pop().unwrap_or_default();
        let pending = self.links.pop().unwrap_or(PendingLink {
            url: String::new(),
            image: false,
        });
        let label_text: String = label
            .iter()
            .map(|piece| match piece {
                Piece::Text { text, .. } => text.as_str(),
                Piece::Break => " ",
            })
            .collect();
        if pending.image {
            self.push_image(label, pending.url, &label_text);
            return;
        }
        let hit = http_url(&pending.url)
            .or_else(|| local_file_link(&pending.url).map(|_| pending.url.clone()));
        if label_text.is_empty() || label_text == pending.url {
            self.push(Piece::Text {
                text: pending.url,
                style: theme::link(),
                url: hit,
            });
            return;
        }
        for piece in label {
            match piece {
                Piece::Text { text, style, url } if url.is_some() => {
                    self.push(Piece::Text { text, style, url });
                }
                Piece::Text { text, style, .. } => self.push(Piece::Text {
                    text,
                    style: linked_style(style),
                    url: hit.clone(),
                }),
                Piece::Break => self.push(Piece::Break),
            }
        }
        if self.in_table_cell() {
            return;
        }
        self.push(Piece::Text {
            text: format!(" ({})", pending.url),
            style: theme::faint(),
            url: hit,
        });
    }

    fn in_table_cell(&self) -> bool {
        self.table.as_ref().is_some_and(|table| table.in_cell)
    }

    fn push_image(&mut self, label: Vec<Piece>, url: String, label_text: &str) {
        if label_text.is_empty() || label_text == url {
            let style = label
                .iter()
                .find_map(|piece| match piece {
                    Piece::Text { style, .. } => Some(*style),
                    Piece::Break => None,
                })
                .unwrap_or_else(theme::faint);
            self.push(Piece::Text {
                text: url,
                style,
                url: None,
            });
            return;
        }
        for piece in label {
            self.push(piece);
        }
        if self.in_table_cell() {
            return;
        }
        self.push(Piece::Text {
            text: format!(" ({url})"),
            style: theme::faint(),
            url: None,
        });
    }

    fn emit_prose(&mut self) {
        let pieces = std::mem::take(&mut self.inline);
        let prefix = self.item_prefix();
        if prefix.is_empty() {
            self.separate();
        }
        let width = self.content_width().saturating_sub(cols(&prefix));
        let shift = cols(&prefix);
        let indent = " ".repeat(shift);
        for (index, (mut line, mut hits)) in wrap_pieces(&pieces, width).into_iter().enumerate() {
            if shift > 0 {
                for hit in &mut hits {
                    hit.start += shift;
                    hit.end += shift;
                }
                let lead = if index == 0 {
                    prefix.clone()
                } else {
                    indent.clone()
                };
                let mut spans = vec![Span::styled(lead, theme::body())];
                spans.append(&mut line.spans);
                line.spans = spans;
            }
            self.push_row(line, hits);
        }
    }

    fn emit_code(&mut self, raw: String) {
        let prefix = self.item_prefix();
        if prefix.is_empty() {
            self.separate();
        }
        let width = self.content_width().saturating_sub(cols(&prefix));
        let mut lines = Vec::new();
        for line in code_lines(&raw) {
            for part in split_cols(&line, width) {
                lines.push(Line::from(Span::styled(part, code_style())));
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(String::new(), code_style())));
        }
        stamp(&mut lines, &prefix);
        for line in lines {
            self.push_row(line, Vec::new());
        }
    }

    fn rule(&mut self) {
        if self.items.is_empty() {
            self.separate();
        }
        let width = self.content_width().max(1);
        self.push_row(
            Line::from(Span::styled("─".repeat(width), theme::faint())),
            Vec::new(),
        );
    }

    fn separate(&mut self) {
        let nonempty = self.lines.last().is_some_and(|line| {
            line.spans
                .iter()
                .any(|span| span.content.chars().any(|ch| !ch.is_whitespace()))
        });
        if nonempty {
            self.push_row(Line::from(""), Vec::new());
        }
    }

    fn item_prefix(&mut self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let index = self.items.len() - 1;
        if self.items[index].marker.is_none() {
            let marker = self.make_marker();
            self.items[index].marker = Some(marker);
        }
        let marker = self.items[index].marker.clone().unwrap_or_default();
        if self.items[index].used {
            " ".repeat(cols(&marker))
        } else {
            self.items[index].used = true;
            marker
        }
    }

    fn make_marker(&mut self) -> String {
        let depth = self.lists.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let task = self.items.last().and_then(|item| item.task);
        let Some(list) = self.lists.last_mut() else {
            return marker_text(&indent, task);
        };
        if let Some(checked) = task {
            if let Some(number) = list.next.as_mut() {
                *number += 1;
            }
            return marker_text(&indent, Some(checked));
        }
        if let Some(number) = list.next.as_mut() {
            let marker = format!("{indent}{number}. ");
            *number += 1;
            marker
        } else {
            format!("{indent}\u{2022} ")
        }
    }

    fn task_marker(&mut self, checked: bool) {
        let Some(item) = self.items.last_mut() else {
            return;
        };
        item.task = Some(checked);
        if !item.used {
            item.marker = None;
        }
    }

    fn open_table(&mut self, align: Vec<ColumnAlign>) {
        if self.table.is_some() {
            self.emit_table();
        }
        if !self.inline.is_empty() {
            self.emit_prose();
        }
        if self.items.is_empty() {
            self.separate();
        }
        self.table = Some(OpenTable {
            align,
            rows: Vec::new(),
            cell: Vec::new(),
            in_cell: false,
        });
    }

    fn open_table_row(&mut self, header: bool) {
        if let Some(table) = self.table.as_mut() {
            table.rows.push(TableRow {
                cells: Vec::new(),
                header,
            });
        } else {
            return;
        }
        if header {
            self.heading = true;
        }
    }

    fn open_table_cell(&mut self) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        table.in_cell = true;
        table.cell.clear();
    }

    fn close_table_cell(&mut self) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        table.in_cell = false;
        let cell = std::mem::take(&mut table.cell);
        if let Some(row) = table.rows.last_mut() {
            row.cells.push(cell);
        }
    }

    fn close_table_head(&mut self) {
        if self.table.is_some() {
            self.heading = false;
        }
    }

    fn emit_table(&mut self) {
        let Some(mut table) = self.table.take() else {
            return;
        };
        if table.in_cell {
            let cell = std::mem::take(&mut table.cell);
            if let Some(row) = table.rows.last_mut() {
                row.cells.push(cell);
            }
        }
        let count = table
            .rows
            .iter()
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(0)
            .max(table.align.len());
        if count == 0 {
            return;
        }
        let mut widths = vec![0usize; count];
        for row in &table.rows {
            for (index, cell) in row.cells.iter().enumerate() {
                widths[index] = widths[index].max(piece_width(cell));
            }
        }
        fit_table(&mut widths, self.table_room());
        for row in &table.rows {
            let wrapped: Vec<Vec<Vec<Ink>>> = (0..count)
                .map(|index| {
                    let pieces = row.cells.get(index).map(Vec::as_slice).unwrap_or(&[]);
                    cell_lines(pieces, widths[index])
                })
                .collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for visual in 0..height {
                let mut chars = Vec::new();
                for (index, width) in widths.iter().enumerate() {
                    if index > 0 {
                        chars.extend(ink_spaces(TABLE_GAP));
                    }
                    let align = table.align.get(index).copied().unwrap_or(ColumnAlign::None);
                    let line = wrapped
                        .get(index)
                        .and_then(|cell| cell.get(visual))
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    chars.extend(align_cell(line, *width, align));
                }
                let (line, hits) = painted(&chars);
                self.push_row(line, hits);
            }
            if row.header {
                self.push_row(table_rule(&widths), Vec::new());
            }
        }
    }
}

fn line_has_ink(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .any(|span| span.content.chars().any(|ch| !ch.is_whitespace()))
}

fn marker_text(indent: &str, task: Option<bool>) -> String {
    match task {
        Some(true) => format!("{indent}[x] "),
        Some(false) => format!("{indent}[ ] "),
        None => format!("{indent}\u{2022} "),
    }
}

fn bare_url_span(text: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while from < text.len() {
        let rest = &text[from..];
        let (rel, scheme_len) = earlier_scheme(rest)?;
        let at = from + rel;
        if !bare_boundary(text, at) {
            from = at + 1;
            continue;
        }
        let tail = &text[at..];
        if !tail[scheme_len..].starts_with("//") {
            from = at + scheme_len;
            continue;
        }
        let url_len = bare_url_len(tail);
        if url_len <= scheme_len + 2 {
            from = at + scheme_len;
            continue;
        }
        return Some((at, at + url_len));
    }
    None
}

fn earlier_scheme(text: &str) -> Option<(usize, usize)> {
    let https = text.find("https:").map(|index| (index, 6));
    let http = text.find("http:").map(|index| (index, 5));
    match (https, http) {
        (Some((https_at, https_len)), Some((http_at, http_len))) => {
            if https_at <= http_at {
                Some((https_at, https_len))
            } else {
                Some((http_at, http_len))
            }
        }
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn bare_boundary(text: &str, at: usize) -> bool {
    let Some(prev) = text[..at].chars().next_back() else {
        return true;
    };
    !prev.is_ascii_alphanumeric() && prev != '_' && prev != '*' && prev != '~'
}

fn bare_url_len(text: &str) -> usize {
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if ch.is_whitespace() || ch == '<' || ch == '>' {
            break;
        }
        end = index + ch.len_utf8();
    }
    let mut url = &text[..end];
    while let Some(last) = url.chars().next_back() {
        let trim = matches!(
            last,
            '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_' | '~' | '\'' | '"'
        ) || (last == ')' && url.matches(')').count() > url.matches('(').count())
            || (last == ']' && url.matches(']').count() > url.matches('[').count());
        if !trim {
            break;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
    url.len()
}

pub(super) fn code_style() -> Style {
    Style::default().fg(theme::accent())
}

pub(super) fn linked_style(style: Style) -> Style {
    let mut next = theme::link();
    if style.add_modifier.contains(Modifier::BOLD) {
        next = next.add_modifier(Modifier::BOLD);
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        next = next.add_modifier(Modifier::ITALIC);
    }
    if style.add_modifier.contains(Modifier::CROSSED_OUT) {
        next = next.add_modifier(Modifier::CROSSED_OUT);
    }
    next
}

pub(super) fn markdown_link_at(
    text: &str,
    text_width: usize,
    line_index: usize,
    column: usize,
) -> Option<String> {
    markdown_link_between(text, text_width, text_width, line_index, column)
}

pub(super) fn markdown_link_between(
    text: &str,
    prose: usize,
    table: usize,
    line_index: usize,
    column: usize,
) -> Option<String> {
    let mut paint = Paint::new(prose);
    paint.table_width = table;
    for event in markdown_events(text) {
        paint.event(event);
    }
    paint.link_on(line_index, column)
}

pub(super) fn stamp(lines: &mut [Line<'static>], prefix: &str) {
    if prefix.is_empty() {
        return;
    }
    let indent = " ".repeat(cols(prefix));
    for (index, line) in lines.iter_mut().enumerate() {
        let lead = if index == 0 {
            prefix.to_string()
        } else {
            indent.clone()
        };
        let mut spans = vec![Span::styled(lead, theme::body())];
        spans.append(&mut line.spans);
        line.spans = spans;
    }
}

pub(super) type Ink = (char, Style, Option<String>);

pub(super) fn wrap_pieces(pieces: &[Piece], width: usize) -> Vec<(Line<'static>, Vec<Hit>)> {
    let mut segments: Vec<Vec<Ink>> = vec![Vec::new()];
    for piece in pieces {
        match piece {
            Piece::Break => segments.push(Vec::new()),
            Piece::Text { text, style, url } => {
                let segment = segments.last_mut().expect("a segment");
                for ch in text.chars() {
                    segment.push((ch, *style, url.clone()));
                }
            }
        }
    }
    let mut lines = Vec::new();
    for segment in segments {
        lines.extend(wrap_chars(&segment, width));
    }
    if lines.is_empty() {
        lines.push((Line::from(""), Vec::new()));
    }
    lines
}

pub(super) struct RichWord {
    gap: Vec<Ink>,
    chars: Vec<Ink>,
}

pub(super) fn wrap_chars(chars: &[Ink], width: usize) -> Vec<(Line<'static>, Vec<Hit>)> {
    broken_lines(chars, width)
        .into_iter()
        .map(|chars| painted(&chars))
        .collect()
}

fn broken_lines(chars: &[Ink], width: usize) -> Vec<Vec<Ink>> {
    layout_lines(chars, width, rich_words(chars))
}

fn cell_broken_lines(chars: &[Ink], width: usize) -> Vec<Vec<Ink>> {
    layout_lines(chars, width, soft_words(chars))
}

fn layout_lines(chars: &[Ink], width: usize, words: Vec<RichWord>) -> Vec<Vec<Ink>> {
    if width == 0 {
        return vec![chars.to_vec()];
    }
    if chars.iter().all(|(ch, _, _)| ch.is_whitespace()) {
        return vec![Vec::new()];
    }
    let mut lines: Vec<Vec<Ink>> = Vec::new();
    let mut line: Vec<Ink> = Vec::new();
    for word in words {
        if line.is_empty() {
            push_rich_word(&mut lines, &mut line, &word.chars, width);
        } else if cols_of(&line) + cols_of(&word.gap) + cols_of(&word.chars) <= width {
            line.extend(word.gap);
            line.extend(word.chars);
        } else {
            lines.push(std::mem::take(&mut line));
            push_rich_word(&mut lines, &mut line, &word.chars, width);
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn soft_words(chars: &[Ink]) -> Vec<RichWord> {
    let mut out = Vec::new();
    for word in rich_words(chars) {
        let mut parts = soft_parts(&word.chars);
        if parts.is_empty() {
            continue;
        }
        let first = parts.remove(0);
        out.push(RichWord {
            gap: word.gap,
            chars: first,
        });
        for part in parts {
            out.push(RichWord {
                gap: Vec::new(),
                chars: part,
            });
        }
    }
    out
}

fn soft_parts(word: &[Ink]) -> Vec<Vec<Ink>> {
    let mut parts = Vec::new();
    let mut cur = Vec::new();
    for ink in word {
        cur.push(ink.clone());
        if ink.0 == '/' || ink.0 == '-' {
            parts.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn cell_lines(pieces: &[Piece], width: usize) -> Vec<Vec<Ink>> {
    let mut segments: Vec<Vec<Ink>> = vec![Vec::new()];
    for piece in pieces {
        match piece {
            Piece::Break => segments.push(Vec::new()),
            Piece::Text { text, style, url } => {
                let segment = segments.last_mut().expect("a segment");
                for ch in text.chars() {
                    segment.push((ch, *style, url.clone()));
                }
            }
        }
    }
    let mut lines = Vec::new();
    for segment in segments {
        lines.extend(cell_broken_lines(&segment, width));
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn piece_width(pieces: &[Piece]) -> usize {
    let mut max = 0usize;
    let mut current = 0usize;
    for piece in pieces {
        match piece {
            Piece::Break => {
                max = max.max(current);
                current = 0;
            }
            Piece::Text { text, .. } => current += cols(text),
        }
    }
    max.max(current)
}

fn fit_table(widths: &mut [usize], available: usize) {
    if widths.is_empty() {
        return;
    }
    let natural = widths.to_vec();
    let gaps = TABLE_GAP * widths.len().saturating_sub(1);
    let room = available.saturating_sub(gaps);
    let half = room / 2;
    for (index, width) in widths.iter_mut().enumerate() {
        let cap = half.max(column_floor(natural[index]));
        if *width > cap {
            *width = cap;
        }
    }
    let mut total = widths.iter().sum::<usize>().saturating_add(gaps);
    while total > available {
        let Some(index) = widest_shrink(widths, &natural) else {
            break;
        };
        widths[index] -= 1;
        total -= 1;
    }
}

fn column_floor(natural: usize) -> usize {
    TABLE_COLUMN_FLOOR.min(natural)
}

fn widest_shrink(widths: &[usize], natural: &[usize]) -> Option<usize> {
    let long_above_floor = widths.iter().enumerate().any(|(index, width)| {
        natural[index] > TABLE_SHORT_COLUMN && *width > column_floor(natural[index])
    });
    let mut best = None;
    for (index, width) in widths.iter().enumerate() {
        if *width <= column_floor(natural[index]) {
            continue;
        }
        if long_above_floor && natural[index] <= TABLE_SHORT_COLUMN {
            continue;
        }
        if best.is_none_or(|chosen| *width > widths[chosen]) {
            best = Some(index);
        }
    }
    best
}

fn align_cell(chars: &[Ink], width: usize, align: ColumnAlign) -> Vec<Ink> {
    let pad = width.saturating_sub(cols_of(chars));
    let (left, right) = match align {
        ColumnAlign::Right => (pad, 0),
        ColumnAlign::Center => (pad / 2, pad - pad / 2),
        ColumnAlign::Left | ColumnAlign::None => (0, pad),
    };
    let mut out = ink_spaces(left);
    out.extend_from_slice(chars);
    out.extend(ink_spaces(right));
    out
}

fn ink_spaces(count: usize) -> Vec<Ink> {
    std::iter::repeat_n((' ', theme::body(), None), count).collect()
}

fn table_rule(widths: &[usize]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(TABLE_GAP)));
        }
        spans.push(Span::styled("─".repeat(*width), theme::faint()));
    }
    Line::from(spans)
}

pub(super) fn rich_words(chars: &[Ink]) -> Vec<RichWord> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let start = index;
        while index < chars.len() && chars[index].0.is_whitespace() {
            index += 1;
        }
        if index == chars.len() {
            break;
        }
        let gap = if out.is_empty() {
            Vec::new()
        } else {
            chars[start..index]
                .iter()
                .flat_map(|(ch, style, url)| {
                    let count = ch.len_utf8();
                    std::iter::repeat_n((' ', *style, url.clone()), count)
                })
                .collect()
        };
        let word_at = index;
        while index < chars.len() && !chars[index].0.is_whitespace() {
            index += 1;
        }
        out.push(RichWord {
            gap,
            chars: chars[word_at..index].to_vec(),
        });
    }
    out
}

pub(super) fn push_rich_word(
    lines: &mut Vec<Vec<Ink>>,
    line: &mut Vec<Ink>,
    word: &[Ink],
    width: usize,
) {
    if cols_of(word) <= width {
        line.extend_from_slice(word);
        return;
    }
    for (ch, style, url) in word {
        let w = UnicodeWidthChar::width(*ch).unwrap_or(0);
        if !line.is_empty() && cols_of(line) + w > width {
            lines.push(std::mem::take(line));
        }
        line.push((*ch, *style, url.clone()));
    }
}

pub(super) fn cols_of(chars: &[Ink]) -> usize {
    chars
        .iter()
        .map(|(ch, _, _)| UnicodeWidthChar::width(*ch).unwrap_or(0))
        .sum()
}

pub(super) fn painted(chars: &[Ink]) -> (Line<'static>, Vec<Hit>) {
    (line_from(chars), link_hits(chars))
}

pub(super) fn link_hits(chars: &[Ink]) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut col = 0usize;
    let mut open: Option<(usize, String)> = None;
    for (ch, _, url) in chars {
        let width = UnicodeWidthChar::width(*ch).unwrap_or(0);
        let same = match (&open, url) {
            (Some((_, current)), Some(next)) => current == next,
            _ => false,
        };
        if !same {
            if let Some((start, current)) = open.take() {
                if col > start {
                    hits.push(Hit {
                        start,
                        end: col,
                        url: current,
                    });
                }
            }
            if let Some(url) = url.clone() {
                open = Some((col, url));
            }
        }
        col += width;
    }
    if let Some((start, url)) = open {
        if col > start {
            hits.push(Hit {
                start,
                end: col,
                url,
            });
        }
    }
    hits
}

pub(super) fn line_from(chars: &[Ink]) -> Line<'static> {
    if chars.is_empty() {
        return Line::from("");
    }
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut style = chars[0].1;
    for (ch, next, _) in chars {
        if *next == style {
            buf.push(*ch);
        } else {
            spans.push(Span::styled(std::mem::take(&mut buf), style));
            style = *next;
            buf.push(*ch);
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    Line::from(spans)
}

pub(super) fn code_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = raw.split('\n').map(str::to_string).collect();
    if raw.ends_with('\n') {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(super) fn split_cols(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() || width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if !line.is_empty() && used + w > width {
            out.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push(ch);
        used += w;
    }
    out.push(line);
    out
}

/// `text` broken to `width`, each line already styled.
pub(super) fn answer_lines(card: &Card, width: usize) -> Vec<Line<'static>> {
    let Card::Answer { text } = card else {
        return Vec::new();
    };
    let color = card.color();
    let rail_width = cols(RIGHT_RAIL);
    let column = (width * 2 / 3)
        .max(rail_width.saturating_add(1))
        .min(width.max(1));
    let text_width = column.saturating_sub(rail_width).max(1);
    let pad = width.saturating_sub(column);
    let mut out = vec![right_row(
        pad,
        column,
        &card.label().to_uppercase(),
        theme::badge(color),
        RIGHT_RAIL,
        theme::rail(color),
    )];
    for line in wrap(text, text_width) {
        out.push(right_row(
            pad,
            column,
            &line,
            theme::body(),
            RIGHT_RAIL,
            theme::rail_rest(color),
        ));
    }
    out
}

pub(super) fn right_row(
    pad: usize,
    column: usize,
    content: &str,
    content_style: Style,
    rail: &str,
    rail_style: Style,
) -> Line<'static> {
    let inner = column.saturating_sub(cols(rail));
    let gap = inner.saturating_sub(cols(content));
    let mut spans = Vec::new();
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.push(Span::styled(content.to_string(), content_style));
    spans.push(Span::styled(rail.to_string(), rail_style));
    Line::from(spans)
}

pub(super) fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap(text, width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

pub(super) fn argv_lines(argv: &[String], width: usize) -> Vec<Line<'static>> {
    if argv.is_empty() {
        Vec::new()
    } else {
        wrapped(&argv.join(" "), width, argv_style())
    }
}
