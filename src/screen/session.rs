use super::*;
use std::cell::RefCell;
use std::rc::Rc;

struct CardLayout {
    card: Card,
    lines: Vec<Line<'static>>,
}

struct SessionLayout {
    selected: String,
    width: usize,
    appearance: theme::Appearance,
    pull: Option<String>,
    cards: Vec<CardLayout>,
    lines: Rc<Vec<Line<'static>>>,
}

thread_local! {
    static SESSION_LAYOUT: RefCell<Option<SessionLayout>> = const { RefCell::new(None) };
}

fn session_lines(model: &ScreenModel, width: usize) -> Rc<Vec<Line<'static>>> {
    let appearance = theme::appearance();
    let pull = model
        .selected_session()
        .and_then(|row| row.pull_url.clone());
    SESSION_LAYOUT.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.as_ref().is_none_or(|layout| {
            layout.width != width
                || layout.appearance != appearance
                || layout.selected != model.selected
        }) {
            *cache = Some(SessionLayout {
                selected: model.selected.clone(),
                width,
                appearance,
                pull: None,
                cards: Vec::new(),
                lines: Rc::new(Vec::new()),
            });
        }
        let layout = cache.as_mut().unwrap();
        let mut changed = layout.pull != pull || layout.cards.len() != model.cards.len();
        for (index, card) in model.cards.iter().enumerate() {
            if layout
                .cards
                .get(index)
                .is_some_and(|cached| cached.card == *card)
            {
                continue;
            }
            let card = CardLayout {
                card: card.clone(),
                lines: card.lines(width),
            };
            if index < layout.cards.len() {
                layout.cards[index] = card;
            } else {
                layout.cards.push(card);
            }
            changed = true;
        }
        layout.cards.truncate(model.cards.len());
        if changed || layout.lines.is_empty() {
            let mut lines = vec![Line::from("")];
            if let Some(url) = &pull {
                lines.extend(wrapped(
                    &crate::session::opened_line(url),
                    width,
                    theme::faint(),
                ));
                lines.push(Line::from(""));
            }
            for (index, card) in layout.cards.iter().enumerate() {
                lines.extend(card.lines.iter().cloned());
                if index + 1 < layout.cards.len() {
                    lines.push(Line::from(""));
                }
            }
            layout.lines = Rc::new(lines);
            layout.pull = pull;
        }
        Rc::clone(&layout.lines)
    })
}

pub(super) fn session_action(model: &ScreenModel) -> Option<&str> {
    if model.compacting {
        return Some("Compacting context");
    }
    let selected = model.selected_session();
    let working = selected.is_some_and(|row| row.status == Status::Working);
    model.retry_status.as_deref().or(
        if selected.is_some_and(|row| row.status == Status::Waiting) {
            Some("Waiting")
        } else {
            match model.phase {
                Some(Phase::Thinking) => model.action.as_deref().or(Some("Thinking")),
                Some(Phase::Tool) if working => model.action.as_deref().or(Some("Using tool")),
                None if working => Some("Working"),
                _ => None,
            }
        },
    )
}

pub(super) fn session_has_footer(model: &ScreenModel) -> bool {
    session_action(model).is_some()
}

pub(super) fn session_room(model: &ScreenModel, height: usize) -> usize {
    if session_has_footer(model) {
        height.saturating_sub(1)
    } else {
        height
    }
}

pub fn session_column(model: &ScreenModel, width: usize) -> Vec<Line<'static>> {
    session_lines(model, width).as_ref().clone()
}

pub fn card_scroll(model: &ScreenModel, area: Rect, index: usize) -> usize {
    let inner = session_inner_of(model, area);
    let width = usize::from(inner.width);
    let prefix = model
        .selected_session()
        .and_then(|row| row.pull_url.as_deref())
        .map(|url| wrap(&crate::session::opened_line(url), width).len() + 1)
        .unwrap_or(0);
    let offset = 1
        + prefix
        + model
            .cards
            .iter()
            .take(index)
            .map(|card| card.lines(width).len() + 1)
            .sum::<usize>();
    offset.min(session_tail(model, inner))
}

pub fn session_tail(model: &ScreenModel, inner: Rect) -> usize {
    let count = session_lines(model, usize::from(inner.width)).len();
    let room = session_room(model, usize::from(inner.height));
    count.saturating_sub(room)
}

pub(super) fn session_origin(model: &ScreenModel, inner: Rect) -> usize {
    model.scroll.min(session_tail(model, inner))
}

pub(super) fn render_session(model: &ScreenModel, area: Rect, frame: &mut Frame) {
    let selected = model.selected_session();
    // The short id and the path share one title, so they read as a single
    // heading instead of two titles butted together.
    let title = match selected {
        Some(row) => {
            let path = relative_path(&row.workspace, &model.home);
            let heading = match row.pull_mark() {
                Some(mark) => format!(" {path}  {mark} "),
                None => format!(" {path} "),
            };
            Line::from(Span::styled(heading, theme::faint()))
        }
        None => Line::from(Span::styled(format!(" {PRODUCT} "), theme::title())),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border_focused())
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let origin = session_origin(model, inner);
    let room = session_room(model, usize::from(inner.height));
    let mut lines: Vec<Line<'static>> = session_lines(model, usize::from(inner.width))
        .iter()
        .skip(origin)
        .take(room)
        .cloned()
        .collect();
    paint_selection(model.select, SelectPlace::Session, origin, &mut lines);
    let footer = session_action(model);
    if footer.is_some() && inner.height > 0 {
        let mut spans = vec![
            Span::styled(
                format!(
                    "{} ",
                    if selected.is_some_and(|row| row.status == Status::Waiting) {
                        "·"
                    } else {
                        FLUX_SPINNER[model.tick % FLUX_SPINNER.len()]
                    }
                ),
                Style::default().fg(theme::accent()),
            ),
            Span::styled(
                footer.unwrap_or("Working").to_string(),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if model.queue > 0 {
            spans.push(Span::styled(
                format!(" queued {}", model.queue),
                theme::faint(),
            ));
        }
        let indicator = Line::from(spans);
        // The indicator sits on the last row of the pane, so the cards are
        // padded to the row above it rather than the row itself.
        let last = usize::from(inner.height).saturating_sub(1);
        lines.truncate(last);
        while lines.len() < last {
            lines.push(Line::from(""));
        }
        lines.push(indicator);
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn on_input(style: Style) -> Style {
    style.bg(theme::selected_bg())
}

pub(super) fn input_lines(model: &ScreenModel, width: u16) -> Vec<Line<'static>> {
    let empty_prompt = model.bottom_kind == Bottom::Prompt
        && model.selected_session().is_some_and(|row| {
            row.status == Status::Idle
                || row.status == Status::Working
                || (row.status == Status::Waiting && row.waiting == Some(Wait::Question))
        });
    if width == 0 || (model.bottom.is_empty() && !empty_prompt) {
        return Vec::new();
    }
    let mut spans = vec![Span::styled(
        " \u{203a} ",
        on_input(
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
    )];
    if model.bottom_kind == Bottom::Keys {
        // A card is answered with keys. The key is lit and the word after it is
        // quiet, but the pair keeps its single space so the row still reads as
        // plain `a once`, `s session`, `d deny`, or `1 continue`, `2 stop`.
        for (index, part) in model.bottom.split("   ").enumerate() {
            if index > 0 {
                spans.push(Span::styled("  ", on_input(Style::default())));
            }
            match part.split_once(' ') {
                Some((key, word)) => {
                    spans.push(Span::styled(key.to_string(), on_input(theme::key())));
                    spans.push(Span::styled(" ", on_input(Style::default())));
                    spans.push(Span::styled(word.to_string(), on_input(theme::hint())));
                }
                None => spans.push(Span::styled(part.to_string(), on_input(Style::default()))),
            }
        }
    } else {
        spans.push(Span::styled(model.bottom.clone(), on_input(theme::input())));
    }
    spans.push(Span::styled("\u{2588}", on_input(theme::cursor())));
    let mut lines = Vec::new();
    let mut line = Line::default();
    let mut used = 0;
    for span in spans {
        for ch in span.content.chars() {
            if ch == '\n' {
                lines.push(std::mem::take(&mut line));
                used = 0;
                continue;
            }
            let cells = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used > 0 && used + cells > usize::from(width) {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.spans.push(Span::styled(ch.to_string(), span.style));
            used += cells;
        }
    }
    lines.push(line);
    if model.bottom_kind == Bottom::Prompt {
        lines = super::input::prompt_lines(model, width);
    }
    for image in &model.pending_images {
        let name = image_chip_name(image, width);
        lines.push(Line::from(vec![
            Span::styled(format!(" ▧ {name}"), on_input(theme::key())),
            Span::styled("  × ", on_input(theme::hint())),
        ]));
    }
    lines
}
pub(super) fn render_input(model: &ScreenModel, area: Rect, frame: &mut Frame) {
    let framed = composer_framed(model, frame.area().height);
    let content = if framed {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .style(on_input(Style::default()))
            .border_style(theme::border_focused())
            .title(" Message ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    } else {
        area
    };
    let lines = input_lines(model, content.width);
    if lines.is_empty() {
        return;
    }
    let start = super::input::input_start(model, content, lines.len());
    frame.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>())
            .style(on_input(Style::default())),
        content,
    );
}

pub(super) fn render_picker(picker: &SkillPicker, area: Rect, frame: &mut Frame) {
    if area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, row) in picker.rows.iter().enumerate() {
        let selected = index == picker.selected;
        let name_style = if selected {
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD)
        } else {
            theme::body()
        };
        let label = format!(" /{}  ", row.name);
        let used = label.chars().count();
        let rest = width.saturating_sub(used);
        let desc = if rest == 0 || row.description.is_empty() {
            String::new()
        } else {
            row.description.chars().take(rest).collect()
        };
        let mut line = Line::from(vec![
            Span::styled(label, name_style),
            Span::styled(desc, theme::faint()),
        ]);
        let used = line.width();
        if used < width {
            line.spans
                .push(Span::styled(" ".repeat(width - used), Style::default()));
        }
        if selected {
            for span in line.spans.iter_mut() {
                span.style = theme::selected_row().patch(span.style);
            }
        }
        lines.push(line);
    }
    frame.render_widget(Paragraph::new(lines), area);
}
