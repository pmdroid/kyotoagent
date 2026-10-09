use super::*;

pub fn question_preview_area(pane: Rect, overlay: &Overlay) -> Option<Rect> {
    let Overlay::VisualQuestion {
        text,
        choices,
        visual,
        ..
    } = overlay
    else {
        return None;
    };
    let frame = crate::image_preview::popup(pane);
    let inner = Block::bordered().inner(frame);
    let width = usize::from(inner.width).max(1);
    let header = markdown_lines(text, width).len() + wrap(&visual.title, width).len();
    let footer = choices
        .iter()
        .map(|c| wrap(&c.label, width.saturating_sub(3).max(1)).len())
        .sum::<usize>()
        + 3;
    if crate::splash::detect() == crate::splash::Protocol::HalfBlocks {
        return None;
    }
    let height = inner
        .height
        .saturating_sub(u16::try_from(header + footer).unwrap_or(u16::MAX));
    (height >= 5).then(|| Rect::new(frame.x, inner.y + header as u16, frame.width, height))
}

pub(super) fn render_visual_question(overlay: &Overlay, pane: Rect, frame: &mut Frame) {
    let Overlay::VisualQuestion {
        text,
        choices,
        prompt,
        visual,
    } = overlay
    else {
        return;
    };
    let area = crate::image_preview::popup(pane);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::border_focused()),
        area,
    );
    let inner = Block::bordered().inner(area);
    let width = usize::from(inner.width).max(1);
    let mut lines = markdown_lines(text, width);
    lines.extend(wrapped(&visual.title, width, theme::body()));
    if let Some(image_area) = question_preview_area(pane, overlay) {
        let image_pane = Rect::new(
            image_area.x.saturating_sub(1),
            image_area.y.saturating_sub(1),
            image_area.width + 2,
            image_area.height + 2,
        );
        crate::image_preview::render(&visual.image, image_pane, frame);
        lines.extend((0..image_area.height).map(|_| Line::from("")));
    } else {
        lines.extend(wrapped(&visual.alt, width, theme::faint()));
    }
    for (index, choice) in choices.iter().enumerate() {
        lines.extend(wrapped(
            &format!("{}  {}", index + 1, choice.label),
            width,
            theme::body(),
        ));
    }
    lines.push(key_hint_line("Ctrl-V next diagram · Esc back"));
    lines.push(Line::from(format!(" › {prompt}█")));
    for (index, line) in lines
        .into_iter()
        .take(usize::from(inner.height))
        .enumerate()
    {
        if !line.spans.is_empty() && line.width() > 0 {
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(inner.x, inner.y + index as u16, inner.width, 1),
            );
        }
    }
    super::overlay::paint_close(frame, area);
}
