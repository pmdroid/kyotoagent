use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectPlace {
    Session,
    Overlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectPoint {
    pub line: usize,
    pub column: usize,
    pub place: SelectPlace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelect {
    pub anchor: SelectPoint,
    pub end: SelectPoint,
    pub held: bool,
    pub x: u16,
    pub y: u16,
}

impl TextSelect {
    pub fn begin(point: SelectPoint, x: u16, y: u16) -> TextSelect {
        TextSelect {
            anchor: point,
            end: point,
            held: true,
            x,
            y,
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor.line == self.end.line && self.anchor.column == self.end.column
    }

    fn ordered(self) -> (SelectPoint, SelectPoint) {
        if (self.anchor.line, self.anchor.column) <= (self.end.line, self.end.column) {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }
}

pub fn selection_anchor(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<SelectPoint> {
    if model.overlay.is_some() {
        if let Some((index, _, col)) = overlay_cell(model, area, column, row) {
            return Some(SelectPoint {
                line: index,
                column: col,
                place: SelectPlace::Overlay,
            });
        }
        if overlay_at(model, area, column, row) {
            return None;
        }
    }
    if thinking_at(model, area, column, row) {
        return None;
    }
    let inner = session_inner_of(model, area);
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    if model.cards.is_empty() {
        return None;
    }
    let line = column_index(model, inner, row)?;
    Some(SelectPoint {
        line,
        column: usize::from(column.saturating_sub(inner.x)),
        place: SelectPlace::Session,
    })
}

pub fn selected_text(model: &ScreenModel, area: Rect) -> String {
    model
        .select
        .map(|sel| selection_text(model, area, sel))
        .unwrap_or_default()
}

pub fn selection_text(model: &ScreenModel, area: Rect, sel: TextSelect) -> String {
    if sel.is_empty() {
        return String::new();
    }
    let lines = match sel.anchor.place {
        SelectPlace::Session => {
            let inner = session_inner_of(model, area);
            session_column(model, usize::from(inner.width))
        }
        SelectPlace::Overlay => {
            let Some(overlay) = model.overlay.as_ref() else {
                return String::new();
            };
            let pane = split_of(model, area).session;
            overlay_lines(overlay, overlay_inner_width(pane))
        }
    };
    let (from, to) = sel.ordered();
    let mut rows = Vec::new();
    for index in from.line..=to.line {
        let Some(line) = lines.get(index) else {
            break;
        };
        let start = if index == from.line { from.column } else { 0 };
        let end = if index == to.line {
            to.column.saturating_add(1)
        } else {
            usize::MAX
        };
        let mut row = String::new();
        let mut col = 0usize;
        for span in &line.spans {
            if span_is_chrome(span.content.as_ref()) {
                col += UnicodeWidthStr::width(span.content.as_ref());
                continue;
            }
            for ch in span.content.chars() {
                let width = UnicodeWidthChar::width(ch).unwrap_or(0);
                let hit = if width == 0 {
                    col >= start && col < end
                } else {
                    col < end && col + width > start
                };
                if hit {
                    row.push(ch);
                }
                col += width;
            }
        }
        rows.push(row.trim_end().to_string());
    }
    rows.join("\n")
}

pub(super) fn paint_selection(
    select: Option<TextSelect>,
    place: SelectPlace,
    origin: usize,
    lines: &mut [Line<'static>],
) {
    let Some(sel) = select else {
        return;
    };
    if sel.anchor.place != place || sel.is_empty() {
        return;
    }
    for (offset, line) in lines.iter_mut().enumerate() {
        let painted = paint_line(std::mem::take(line), origin + offset, sel);
        *line = painted;
    }
}

pub(super) fn paint_line(line: Line<'static>, index: usize, sel: TextSelect) -> Line<'static> {
    let (from, to) = sel.ordered();
    if index < from.line || index > to.line {
        return line;
    }
    let start = if index == from.line { from.column } else { 0 };
    let end = if index == to.line {
        to.column.saturating_add(1)
    } else {
        usize::MAX
    };
    let mut col = 0usize;
    let mut spans = Vec::new();
    for span in line.spans {
        if span_is_chrome(span.content.as_ref()) {
            col += UnicodeWidthStr::width(span.content.as_ref());
            spans.push(span);
            continue;
        }
        let content = span.content.clone();
        let mut pre = String::new();
        let mut mid = String::new();
        let mut post = String::new();
        for ch in content.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            let bucket = if width == 0 {
                if col >= end {
                    2
                } else if col < start {
                    0
                } else {
                    1
                }
            } else if col + width <= start {
                0
            } else if col >= end {
                2
            } else {
                1
            };
            match bucket {
                0 => pre.push(ch),
                1 => mid.push(ch),
                _ => post.push(ch),
            }
            col += width;
        }
        if !pre.is_empty() {
            spans.push(Span::styled(pre, span.style));
        }
        if !mid.is_empty() {
            spans.push(Span::styled(
                mid,
                span.style
                    .bg(theme::selected_bg())
                    .add_modifier(Modifier::REVERSED),
            ));
        }
        if !post.is_empty() {
            spans.push(Span::styled(post, span.style));
        }
    }
    Line::from(spans)
}

pub(super) fn span_is_chrome(text: &str) -> bool {
    text == RAIL
        || text == RAIL_DIM
        || text == RAIL_RIGHT
        || text == RIGHT_RAIL
        || text == RIGHT_RAIL_DIM
        || matches!(
            text,
            "ASK" | "RESULT" | "PROOF" | "ARTIFACT" | "QUESTION" | "ANSWER" | "PERMISSION"
        )
}

pub(super) fn column_index(model: &ScreenModel, inner: Rect, row: u16) -> Option<usize> {
    if row < inner.y {
        return None;
    }
    let offset = usize::from(row - inner.y);
    if offset >= usize::from(inner.height) {
        return None;
    }
    Some(session_origin(model, inner) + offset)
}

pub(super) fn card_at_point(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<&Card> {
    let inner = session_inner_of(model, area);
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let index = column_index(model, inner, row)?;
    let width = usize::from(inner.width);
    let mut cursor = 1usize;
    if let Some(url) = model
        .selected_session()
        .and_then(|session| session.pull_url.as_deref())
    {
        cursor = cursor
            .saturating_add(wrap(&crate::session::opened_line(url), width).len())
            .saturating_add(1);
    }
    for (card_index, card) in model.cards.iter().enumerate() {
        let height = card.lines(width).len();
        if index >= cursor && index < cursor + height {
            return Some(card);
        }
        cursor = cursor.saturating_add(height);
        if card_index + 1 < model.cards.len() {
            cursor = cursor.saturating_add(1);
        }
    }
    None
}
