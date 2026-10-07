use super::*;

#[test]
fn visual_question_keeps_the_question_choices_and_draft_in_narrow_and_wide_frames() {
    for width in [42, 120] {
        let mut model = crate::mock::idle();
        model.left_open = false;
        model.right_open = false;
        model.overlay = Some(Overlay::VisualQuestion {
            text: "When should review happen?".into(),
            choices: vec![
                Choice {
                    label: "Before PR".into(),
                    marked: false,
                },
                Choice {
                    label: "After PR".into(),
                    marked: false,
                },
            ],
            prompt: "My draft".into(),
            visual: crate::question::QuestionVisual {
                title: "Before PR".into(),
                alt: "Implement → Review → PR".into(),
                source: None,
                image: crate::attachment::ImageAttachment::from_bytes(
                    "Before PR",
                    crate::splash::PNG,
                )
                .unwrap(),
            },
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| render(&model, frame.area(), frame))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        for expected in [
            "When should review happen?",
            "1  Before PR",
            "2  After PR",
            "My draft",
        ] {
            assert!(text.contains(expected), "{width}: {text}");
        }
    }
}

#[test]
fn answered_visual_questions_do_not_offer_an_inactive_preview_shortcut() {
    let mut card = Card::Question {
        text: "Which route?".into(),
        choices: Vec::new(),
        answer: None,
        visuals: vec![crate::question::QuestionVisual {
            title: "Review first".into(),
            alt: "Implement → Review → PR".into(),
            source: None,
            image: crate::attachment::ImageAttachment::from_bytes("route", crate::splash::PNG)
                .unwrap(),
        }],
    };
    assert!(card
        .lines(76)
        .iter()
        .any(|line| line.to_string().contains("Ctrl-V")));
    if let Card::Question { answer, .. } = &mut card {
        *answer = Some("Review first".into());
    }
    let rendered = card
        .lines(76)
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Implement → Review → PR"));
    assert!(!rendered.contains("Ctrl-V"));
}

#[test]
fn question_markdown_styles_the_card_and_overlay_without_changing_answers() {
    let text = "# Choose\n\nUse **strong** *slanted* `cargo`\n\n- first\n- second";
    let card = Card::question(text, &[("**literal answer**", false)]);
    let overlay = Overlay::Question {
        text: text.into(),
        choices: vec![Choice {
            label: "**literal answer**".into(),
            marked: false,
        }],
        prompt: "**typed answer**".into(),
    };
    for lines in [card.lines(76), overlay_lines(&overlay, 72)] {
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("# Choose"), "{rendered}");
        assert!(rendered.contains("**literal answer**"), "{rendered}");
        for (word, modifier) in [("strong", Modifier::BOLD), ("slanted", Modifier::ITALIC)] {
            assert!(lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains(word)
                    && span.style.add_modifier.contains(modifier)));
        }
        assert!(lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.content.contains("cargo") && span.style.fg == code_style().fg));
        assert!(rendered.contains("• first"), "{rendered}");
    }
    assert!(overlay_lines(&overlay, 72)
        .iter()
        .any(|line| line.to_string().contains("**typed answer**")));
}

#[test]
fn question_markdown_links_follow_the_rendered_rows() {
    let text = "# Choose\n\nRead [docs](https://example.com) first";
    let card = Card::question(text, &[("docs", false)]);
    let overlay = Overlay::Question {
        text: text.into(),
        choices: vec![Choice {
            label: "docs".into(),
            marked: false,
        }],
        prompt: "docs".into(),
    };
    for width in [24, 76] {
        let lines = card.lines(width);
        let row = lines
            .iter()
            .position(|line| line.to_string().contains("docs"))
            .unwrap();
        let col = column_of(&lines[row].to_string(), "docs");
        assert_eq!(
            link_in_card(&card, width, row, col as u16, 0).as_deref(),
            Some("https://example.com")
        );
        let lines = overlay_lines(&overlay, width);
        let row = lines
            .iter()
            .position(|line| line.to_string().contains("docs"))
            .unwrap();
        let col = column_of(&lines[row].to_string(), "docs");
        assert_eq!(
            overlay_link_at(&overlay, width, row, col).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(overlay_link_at(&overlay, width, lines.len() - 1, 3), None);
    }
}

#[test]
fn the_right_toggle_stays_visible_with_wide_and_crowded_header_text() {
    let mut model = crate::mock::idle();
    for name in [
        "模型",
        "a very long model name that fills the entire header several times over",
    ] {
        model.model = name.into();
        let backend = ratatui::backend::TestBackend::new(30, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(&model, frame.area(), frame))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(29, 0)].symbol(), PANES_GLYPH);
        assert!(panes_glyph_at(&model, Rect::new(0, 0, 30, 24), 29, 0));
        assert!(!panes_glyph_at(&model, Rect::new(0, 0, 30, 24), 28, 0));
    }
    assert_eq!(panes_glyph_column(&model, 0), None);
}

#[test]
fn each_sidebar_pane_displays_a_close_button_at_its_click_target() {
    let mut model = crate::mock::idle();
    model.right_open = true;
    model.right_panes = RightPane::ORDER.into_iter().collect();
    let area = Rect::new(0, 0, 76, 24);
    let buffer = draw_buffer(&model);
    for (pane, rect) in right_pane_rects(&model, area) {
        let x = rect.x + rect.width - 1;
        assert_eq!(buffer[(x, rect.y)].symbol(), "x");
        assert_eq!(pane_close_at(&model, area, x, rect.y), Some(pane));
        assert_eq!(pane_close_at(&model, area, x - 1, rect.y), None);
    }
    model.overlay = Some(Overlay::Pull {
        url: "https://example.com".into(),
    });
    assert_eq!(pane_close_at(&model, area, 75, 1), None);
}

#[test]
fn a_workspace_under_home_shows_a_tilde() {
    let home = Path::new("/home/u");
    assert_eq!(
        relative_path(Path::new("/home/u/work/kyotoagent"), home),
        "~/work/kyotoagent"
    );
    assert_eq!(relative_path(Path::new("/home/u"), home), "~");
    assert_eq!(
        relative_path(Path::new("/srv/kyotoagent"), home),
        "/srv/kyotoagent"
    );
}

#[test]
fn an_enhance_header_mark_and_card_dim_the_outside_and_paint_x() {
    let off = crate::mock::idle();
    let off_rows = grid(&off);
    let header: String = off_rows[0].concat();
    assert!(!header.contains("enhance"), "{header}");

    let mut on = crate::mock::idle();
    on.enhance = true;
    let on_rows = grid(&on);
    let header: String = on_rows[0].concat();
    assert!(header.contains("enhance"), "{header}");

    let mut model = crate::mock::idle();
    model.enhance = true;
    model.overlay = Some(Overlay::Enhance {
        source: "ship it".into(),
        text: "Do the thing carefully.".into(),
        error: None,
    });
    let backend = ratatui::backend::TestBackend::new(76, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    assert!(
        buffer[(0, 0)].style().add_modifier.contains(Modifier::DIM),
        "a cell outside the frame is dim"
    );
    let mut painted = false;
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let style = cell.style();
            if cell.symbol() == "x"
                && style.fg == Some(theme::ACCENT)
                && style.add_modifier.contains(Modifier::BOLD)
                && !style.add_modifier.contains(Modifier::DIM)
            {
                painted = true;
            }
        }
    }
    assert!(painted, "the card paints x");
    let text = grid(&model)
        .into_iter()
        .map(|row| row.concat())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("ship it"), "{text}");
    assert!(text.contains("Do the thing carefully."), "{text}");
}

#[test]
fn a_waiting_row_names_its_card() {
    let row = SessionRow {
        id: "91bc7a1d".to_string(),
        workspace: PathBuf::from("/home/u/work/kyotoagent"),
        title: None,
        status: Status::Waiting,
        waiting: Some(Wait::Permission),
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        hidden: false,
        worktree: false,
        archived: false,
    };
    assert_eq!(row.name(), "kyotoagent");
    assert_eq!(row.status_text(), "waiting permission");
    let titled = SessionRow {
        title: Some("Name the binary".into()),
        ..row.clone()
    };
    assert_eq!(titled.name(), "Name the binary");

    let idle = SessionRow {
        status: Status::Idle,
        ..row.clone()
    };
    assert_eq!(idle.status_text(), "idle");
    let archived = SessionRow {
        archived: true,
        ..row
    };
    assert_eq!(archived.status_text(), "archived");
}

#[test]
fn the_list_shows_four_characters_of_the_id() {
    assert_eq!(short_id("91bc7a1d"), "91bc");
    assert_eq!(short_id("91bc"), "91bc");
}

#[test]
fn a_permission_changes_colour_with_its_answer() {
    let waiting = Card::permission("Replace README.md", None, &[]);
    assert_eq!(waiting.color(), theme::attention());

    let allowed = Card::permission(
        "Replace README.md",
        Some("Allowed replace of README.md"),
        &[],
    );
    assert_eq!(allowed.color(), theme::good());

    let denied = Card::permission(
        "Replace README.md",
        Some("Denied replace of README.md"),
        &[],
    );
    assert_eq!(denied.color(), theme::bad());
}

#[test]
fn a_decided_permission_replaces_its_action_and_drops_the_diff() {
    let waiting = Card::permission("Replace README.md", None, &["+kyotoagent is the binary."]);
    let text: Vec<String> = waiting.lines(40).iter().map(Line::to_string).collect();
    assert!(text.iter().any(|line| line.contains("Replace README.md")));
    assert!(text
        .iter()
        .any(|line| line.contains("+kyotoagent is the binary.")));

    let allowed = Card::permission(
        "Replace README.md",
        Some("Allowed replace of README.md"),
        &[],
    );
    let text: Vec<String> = allowed.lines(40).iter().map(Line::to_string).collect();
    assert!(text
        .iter()
        .any(|line| line.contains("Allowed replace of README.md")));
    assert!(!text.iter().any(|line| line.contains("Replace README.md")));
}

#[test]
fn every_card_line_stays_behind_its_rail() {
    let cards = [
        Card::question(
            "Which title?",
            &[("Kyoto Agent", true), ("Kyoto Agent CLI", false)],
        ),
        Card::permission("Replace README.md", None, &["@@ -1 +1,2 @@", "+added"]),
        Card::result("Done."),
        Card::proof(
            "cargo test passed on the readme heading.",
            &[("test", ItemKind::Command, Outcome::Passed)],
        ),
        Card::answer("Kyoto Agent"),
    ];
    for card in &cards {
        let lines = card.lines(40);
        assert!(lines.len() > 1, "{:?} has only a badge", card.label());
        for line in &lines {
            let text = line.to_string();
            if card.label() == "answer" {
                assert!(
                    text.ends_with('\u{258e}') || text.ends_with('\u{2502}'),
                    "{text:?} lost its right rail"
                );
            } else {
                assert!(
                    text.starts_with(RAIL) || text.starts_with(RAIL_DIM),
                    "{:?}: {text:?} lost its rail",
                    card.label()
                );
            }
            assert!(
                text.chars().count() <= 40,
                "{:?}: {text:?} is too wide",
                card.label()
            );
        }
    }
    let ask = Card::ask("Add a readme line that names the binary.");
    let lines = ask.lines(40);
    assert!(lines.len() > 1);
    for line in &lines {
        let text = line.to_string();
        assert!(
            text.ends_with('\u{258e}') || text.ends_with('\u{2502}'),
            "{text:?} lost its right rail"
        );
        assert!(text.chars().count() <= 40, "{text:?} is too wide");
    }
}

#[test]
fn an_answer_sits_on_the_right_as_the_label() {
    let card = Card::answer("Kyoto Agent");
    let lines: Vec<String> = card.lines(42).iter().map(Line::to_string).collect();
    assert!(lines[0].contains("ANSWER"), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("Kyoto Agent")),
        "{lines:?}"
    );
    assert!(lines.iter().all(|line| !line.contains('1')), "{lines:?}");
    assert!(
        lines.iter().all(|line| !line.contains('\u{25cf}')),
        "{lines:?}"
    );
    let label = lines
        .iter()
        .find(|line| line.contains("Kyoto Agent"))
        .unwrap();
    let start = label.find("Kyoto Agent").unwrap();
    assert!(start >= 42 / 3, "{label:?}");
    let question = Card::question(
        "Which title should the heading use?",
        &[("Kyoto Agent", false), ("Kyoto Agent CLI", false)],
    );
    let asked = question.lines(42)[1].to_string();
    assert!(asked.find("Which").unwrap() < start, "{asked} / {label}");
}

#[test]
fn the_marked_choice_is_the_only_one_that_stands_out() {
    let card = Card::question(
        "Which title?",
        &[("Kyoto Agent", true), ("Kyoto Agent CLI", false)],
    );
    let lines = card.lines(40);
    let marked = lines
        .iter()
        .find(|line| line.to_string().contains("Kyoto Agent"))
        .unwrap();
    let other = lines
        .iter()
        .find(|line| line.to_string().contains("Kyoto Agent CLI"))
        .unwrap();
    assert!(marked.to_string().contains('\u{25cf}'), "{:?}", marked);
    assert!(other.to_string().contains('\u{25cb}'), "{:?}", other);
}

#[test]
fn wrapping_keeps_the_gap_between_words() {
    assert_eq!(wrap("1  Kyoto Agent", 20), vec!["1  Kyoto Agent"]);
    assert_eq!(wrap("a  b   c", 4), vec!["a  b", "c"]);
    assert_eq!(wrap("  padded  ", 10), vec!["padded"]);
    assert_eq!(wrap("one two", 0), vec!["one two"]);
}

#[test]
fn wrap_breaks_a_word_longer_than_the_width() {
    let word = "a".repeat(40);
    let lines = wrap(&word, 10);
    assert!(lines.len() > 1, "{lines:?}");
    for line in &lines {
        assert!(UnicodeWidthStr::width(line.as_str()) <= 10, "{line:?}");
    }
    assert_eq!(lines.join(""), word);
}

#[test]
fn a_card_wraps_inside_the_pane() {
    let card = Card::result("The readme now names the binary under the heading Kyoto Agent.");
    let width = 40;
    let body: Vec<String> = card
        .lines(width)
        .iter()
        .skip(1)
        .map(|line| {
            line.spans
                .iter()
                .skip(1)
                .map(|span| span.content.to_string())
                .collect()
        })
        .collect();
    assert!(body.len() > 1, "the result should wrap: {body:?}");
    for line in &body {
        assert!(
            line.chars().count() + RAIL.chars().count() <= width,
            "too wide: {line:?}"
        );
    }
    let joined: Vec<String> = body.iter().map(|line| line.trim().to_string()).collect();
    assert_eq!(
        joined.join(" "),
        "The readme now names the binary under the heading Kyoto Agent."
    );
}

fn result_body(card: &Card, width: usize) -> Vec<String> {
    card.lines(width)
        .iter()
        .skip(1)
        .map(|line| {
            line.spans
                .iter()
                .skip(1)
                .map(|span| span.content.to_string())
                .collect()
        })
        .collect()
}

#[test]
fn a_plain_result_wraps_on_word_boundaries() {
    let sentence = "The readme now names the binary under the heading Kyoto Agent.";
    let pane = 40;
    let card = Card::result(sentence);
    let width = column_width(&card, pane);
    assert_eq!(
        result_body(&card, pane),
        wrap(sentence, width - RAIL.chars().count())
    );
}

#[test]
fn a_result_draws_a_heading_and_a_list_bullet() {
    let card = Card::result("# Done\n\n- wrote README.md");
    let body = result_body(&card, 40);
    assert!(
        body.iter().any(|line| line == "Done"),
        "heading line: {body:?}"
    );
    assert!(
        body.iter()
            .any(|line| line.contains("wrote README.md") && line.contains('\u{2022}')),
        "list bullet: {body:?}"
    );
    let lines = card.lines(40);
    let heading = lines
        .iter()
        .find(|line| line.to_string().contains("Done"))
        .expect("heading");
    assert!(
        heading.spans.iter().any(|span| {
            span.content.contains("Done") && span.style.add_modifier.contains(Modifier::BOLD)
        }),
        "heading is bold: {heading:?}"
    );
}

#[test]
fn a_fenced_code_block_keeps_its_lines_inside_the_rail() {
    let card = Card::result("```rust\nfn main() {}\nlet x = 1;\n```");
    let body = result_body(&card, 30);
    assert!(
        body.iter().any(|line| line.trim() == "fn main() {}"),
        "{body:?}"
    );
    assert!(
        body.iter().any(|line| line.trim() == "let x = 1;"),
        "{body:?}"
    );
    assert!(
        body.iter().all(|line| !line.contains("```")),
        "the fence markers stay off the card: {body:?}"
    );
    for line in card.lines(30) {
        let text = line.to_string();
        assert!(
            text.starts_with(RAIL) || text.starts_with(RAIL_DIM),
            "{text:?} lost its rail"
        );
        assert!(text.chars().count() <= 30, "{text:?} is too wide");
    }
    let long = "a".repeat(80);
    let wide = Card::result(&format!("```\n{long}\n```"));
    let mut seen = String::new();
    for line in wide.lines(24) {
        let text = line.to_string();
        assert!(text.chars().count() <= 24, "{text:?} is too wide");
        seen.push_str(text.trim_start_matches(['\u{258e}', '\u{2502}', ' ']));
    }
    assert!(seen.contains(&long), "the code line was dropped");
}

#[test]
fn a_result_link_shows_the_label_and_the_url() {
    let card = Card::result("[docs](https://example.com)");
    let body = result_body(&card, 60);
    assert!(
        body.iter()
            .any(|line| line.contains("docs (https://example.com)")),
        "{body:?}"
    );
}

fn span_is_link(span: &Span<'_>, needle: &str) -> bool {
    span.content.contains(needle)
        && span.style.fg == Some(theme::accent())
        && span.style.add_modifier.contains(Modifier::UNDERLINED)
}

#[test]
fn a_result_link_underlines_its_label_and_fades_the_url() {
    let card = Card::result("[docs](https://example.com)");
    let line = card
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains("docs"))
        .expect("link line");
    assert!(
        line.spans.iter().any(|span| span_is_link(span, "docs")),
        "{line:?}"
    );
    assert!(
        line.spans.iter().any(|span| {
            span.content.contains("https://example.com")
                && span.style.fg == Some(theme::muted())
                && !span.style.add_modifier.contains(Modifier::UNDERLINED)
        }),
        "{line:?}"
    );
    let proof = Card::proof("[docs](https://example.com)", &[]);
    let proof_line = proof
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains("docs"))
        .expect("proof link");
    assert!(
        proof_line
            .spans
            .iter()
            .any(|span| span_is_link(span, "docs")),
        "{proof_line:?}"
    );
}

#[test]
fn an_autolink_draws_one_underlined_url() {
    let card = Card::result("<https://example.com>");
    let body = result_body(&card, 80);
    assert!(
        body.iter().any(|line| line.contains("https://example.com")),
        "{body:?}"
    );
    assert!(
        body.iter().all(|line| !line.contains("(https://")),
        "{body:?}"
    );
    let line = card
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains("https://example.com"))
        .expect("autolink");
    assert!(
        line.spans
            .iter()
            .any(|span| span_is_link(span, "https://example.com")),
        "{line:?}"
    );
}

#[test]
fn an_image_keeps_a_plain_label_and_url() {
    let card = Card::result("![shot](https://example.com/a.png)");
    let body = result_body(&card, 80);
    assert!(
        body.iter()
            .any(|line| line.contains("shot (https://example.com/a.png)")),
        "{body:?}"
    );
    let line = card
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains("shot"))
        .expect("image");
    assert!(
        line.spans.iter().any(|span| {
            span.content.contains("shot") && !span.style.add_modifier.contains(Modifier::UNDERLINED)
        }),
        "{line:?}"
    );
}

fn grid(model: &ScreenModel) -> Vec<Vec<String>> {
    let backend = ratatui::backend::TestBackend::new(76, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let mut rows = Vec::new();
    for y in 0..buffer.area.height {
        let mut row = Vec::new();
        for x in 0..buffer.area.width {
            row.push(buffer[(x, y)].symbol().to_string());
        }
        rows.push(row);
    }
    rows
}

fn find_phrase(rows: &[Vec<String>], phrase: &str) -> (u16, u16) {
    for (y, row) in rows.iter().enumerate() {
        let mut acc = String::new();
        let mut starts = Vec::new();
        for cell in row {
            starts.push(acc.len());
            acc.push_str(cell);
        }
        if let Some(at) = acc.find(phrase) {
            let x = starts
                .iter()
                .position(|start| *start == at)
                .expect("phrase starts on a cell");
            return (x as u16, y as u16);
        }
    }
    panic!("missing {phrase}");
}

#[test]
fn link_at_misses_a_destination_that_is_not_http() {
    let mut model = crate::mock::idle();
    model.cards = vec![Card::result(
        "See [readme](README.md) and [docs](https://example.com).",
    )];
    let area = Rect::new(0, 0, 76, 24);
    let rows = grid(&model);
    let (readme_x, readme_y) = find_phrase(&rows, "readme");
    assert_eq!(link_at(&model, area, readme_x, readme_y), None);
    let (path_x, path_y) = find_phrase(&rows, "README.md");
    assert_eq!(link_at(&model, area, path_x, path_y), None);
    let (docs_x, docs_y) = find_phrase(&rows, "docs");
    assert_eq!(
        link_at(&model, area, docs_x, docs_y).as_deref(),
        Some("https://example.com")
    );
}

#[test]
fn link_at_hits_the_label_and_the_url_on_a_result_card() {
    let model = crate::mock::result_markdown();
    let area = Rect::new(0, 0, 76, 24);
    let rows = grid(&model);
    let url = "https://example.com";
    let (label_x, label_y) = find_phrase(&rows, "docs");
    assert_eq!(
        link_at(&model, area, label_x, label_y).as_deref(),
        Some(url)
    );
    let (url_x, url_y) = find_phrase(&rows, url);
    assert_eq!(link_at(&model, area, url_x, url_y).as_deref(), Some(url));
    let (plain_x, plain_y) = find_phrase(&rows, "wrote");
    assert_eq!(link_at(&model, area, plain_x, plain_y), None);
}

#[test]
fn a_selected_range_on_the_result_markdown_is_reversed() {
    let mut model = crate::mock::result_markdown();
    let area = Rect::new(0, 0, 76, 24);
    let rows = grid(&model);
    let (x, y) = find_phrase(&rows, "Done");
    let anchor = selection_anchor(&model, area, x, y).expect("done");
    let end = selection_anchor(&model, area, x + 3, y).expect("end of done");
    model.select = Some(TextSelect {
        anchor,
        end,
        held: false,
        x,
        y,
    });
    let backend = ratatui::backend::TestBackend::new(76, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let selected = buffer[(x, y)].style();
    assert!(
        selected.add_modifier.contains(Modifier::REVERSED),
        "{selected:?}"
    );
    assert_eq!(selected.bg, Some(theme::selected_bg()));
    assert!(buffer[(x + 3, y)]
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
    let rail = buffer[(x.saturating_sub(1), y)].style();
    assert!(!rail.add_modifier.contains(Modifier::REVERSED), "{rail:?}");
    let (plain_x, plain_y) = find_phrase(&rows, "wrote");
    assert!(!buffer[(plain_x, plain_y)]
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
    let mut ask = crate::mock::chat();
    let ask_rows = grid(&ask);
    let (ask_x, ask_y) = find_phrase(&ask_rows, "Add");
    let ask_anchor = selection_anchor(&ask, area, ask_x, ask_y).expect("ask");
    let ask_end = selection_anchor(&ask, area, ask_x + 2, ask_y).expect("ask end");
    ask.select = Some(TextSelect {
        anchor: ask_anchor,
        end: ask_end,
        held: false,
        x: ask_x,
        y: ask_y,
    });
    let copied = selection_text(&ask, area, ask.select.expect("ask select"));
    assert!(copied.contains("Add"), "{copied:?}");
    assert!(!copied.contains("ASK"), "{copied:?}");
}

#[test]
fn link_at_hits_a_table_link_on_a_result_card_and_in_the_proof_overlay() {
    let url = "https://ex.co/r";
    let text = "| tool | what |\n| --- | --- |\n| [read](https://ex.co/r) | qq |\n";
    let mut model = crate::mock::idle();
    model.cards = vec![Card::result(text)];
    let area = Rect::new(0, 0, 76, 24);
    let rows = grid(&model);
    let (label_x, label_y) = find_phrase(&rows, "read");
    assert_eq!(
        link_at(&model, area, label_x, label_y).as_deref(),
        Some(url)
    );
    let drawn = rows
        .iter()
        .map(|row| row.join(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!drawn.contains(url), "{drawn}");
    let (plain_x, plain_y) = find_phrase(&rows, "qq");
    assert_eq!(link_at(&model, area, plain_x, plain_y), None);

    model.cards = vec![Card::result("The table stays on the proof.")];
    model.overlay = Some(Overlay::Proof {
        text: text.into(),
        items: vec![ItemRun {
            id: "test".into(),
            kind: ItemKind::Command,
            outcome: Outcome::Passed,
            argv: Vec::new(),
            exit: None,
            tail: String::new(),
        }],
    });
    let rows = grid(&model);
    let (label_x, label_y) = find_phrase(&rows, "read");
    assert_eq!(
        link_at(&model, area, label_x, label_y).as_deref(),
        Some(url)
    );
    let drawn = rows
        .iter()
        .map(|row| row.join(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!drawn.contains(url), "{drawn}");
}

#[test]
fn link_at_hits_a_markdown_link_in_the_proof_overlay() {
    let mut model = crate::mock::idle();
    model.overlay = Some(Overlay::Proof {
        text: "[docs](https://example.com)".into(),
        items: vec![ItemRun {
            id: "test".into(),
            kind: ItemKind::Command,
            outcome: Outcome::Passed,
            argv: Vec::new(),
            exit: None,
            tail: String::new(),
        }],
    });
    let area = Rect::new(0, 0, 76, 24);
    let rows = grid(&model);
    let url = "https://example.com";
    let (label_x, label_y) = find_phrase(&rows, "docs");
    assert_eq!(
        link_at(&model, area, label_x, label_y).as_deref(),
        Some(url)
    );
    let (url_x, url_y) = find_phrase(&rows, url);
    assert_eq!(link_at(&model, area, url_x, url_y).as_deref(), Some(url));
}

#[test]
fn emphasis_and_inline_code_are_styled_on_the_result() {
    let card = Card::result("a *slanted* **strong** `cargo` word");
    let line = card
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains("slanted"))
        .expect("styled line");
    assert!(line.to_string().contains("a slanted strong cargo word"));
    assert!(line.spans.iter().any(|span| {
        span.content.contains("slanted") && span.style.add_modifier.contains(Modifier::ITALIC)
    }));
    assert!(line.spans.iter().any(|span| {
        span.content.contains("strong") && span.style.add_modifier.contains(Modifier::BOLD)
    }));
    assert!(line
        .spans
        .iter()
        .any(|span| { span.content.contains("cargo") && span.style.fg == Some(theme::accent()) }));
}

#[test]
fn an_ordered_list_keeps_its_numbers() {
    let card = Card::result("1. first\n2. second");
    let body = result_body(&card, 40);
    assert!(
        body.iter().any(|line| line.contains("1. first")),
        "{body:?}"
    );
    assert!(
        body.iter().any(|line| line.contains("2. second")),
        "{body:?}"
    );
}

#[test]
fn an_ask_keeps_markdown_markers() {
    let card = Card::ask("# Done\n\n- wrote README.md");
    let body = result_body(&card, 40);
    let joined = body.join("\n");
    assert!(joined.contains("# Done"), "{body:?}");
    assert!(!joined.contains('\u{2022}'), "{body:?}");
}

fn plain_lines(text: &str, width: usize) -> Vec<String> {
    markdown_lines(text, width)
        .iter()
        .map(Line::to_string)
        .collect()
}

fn column_of(line: &str, needle: &str) -> usize {
    let at = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} in {line:?}"));
    cols(&line[..at])
}

#[test]
fn a_pipe_table_draws_aligned_columns_and_a_header_rule() {
    let lines = plain_lines(
        "| tool | what |\n| --- | --- |\n| read | a file |\n| grep | a pattern |\n",
        40,
    );
    assert_eq!(
        lines,
        vec![
            "tool  what     ".to_string(),
            "────  ─────────".to_string(),
            "read  a file   ".to_string(),
            "grep  a pattern".to_string(),
        ]
    );
    let card =
        Card::result("| tool | what |\n| --- | --- |\n| read | a file |\n| grep | a pattern |\n");
    let body = result_body(&card, 60);
    assert!(body
        .iter()
        .any(|line| line.contains("tool") && line.contains("what")));
    assert!(body.iter().any(|line| line.contains('─')));
    assert!(body.iter().all(|line| !line.contains('|')));
    let header = card
        .lines(60)
        .into_iter()
        .find(|line| line.to_string().contains("tool"))
        .expect("header");
    assert!(header.spans.iter().any(|span| {
        span.content.contains("tool") && span.style.add_modifier.contains(Modifier::BOLD)
    }));
    assert!(header.spans.iter().any(|span| {
        span.content.contains("what") && span.style.add_modifier.contains(Modifier::BOLD)
    }));
}

#[test]
fn a_wide_cell_wraps_inside_its_column() {
    let lines = plain_lines(
        "| tool | what |\n| --- | --- |\n| read | a long explanation |\n",
        12,
    );
    let long = lines
        .iter()
        .find(|line| line.contains("long"))
        .expect("long");
    let wrapped = lines
        .iter()
        .filter(|line| line.contains("expla") || line.contains("natio"))
        .collect::<Vec<_>>();
    assert!(wrapped.len() >= 2, "{lines:?}");
    let read_at = column_of(long, "long");
    for line in &wrapped {
        assert_eq!(
            column_of(
                line,
                if line.contains("expla") {
                    "expla"
                } else {
                    "natio"
                }
            ),
            read_at,
            "{lines:?}"
        );
        assert!(!line.contains("read"), "{line}");
    }
}

#[test]
fn a_right_aligned_column_pads_inside_the_cell() {
    let lines = plain_lines("| name | count |\n| :--- | ----: |\n| read | 7 |\n", 40);
    assert!(lines.iter().any(|line| line == "name  count"), "{lines:?}");
    assert!(lines.iter().any(|line| line == "read      7"), "{lines:?}");
}

#[test]
fn a_centered_column_pads_both_sides() {
    let lines = plain_lines("| xxx |\n| :---: |\n| y |\n", 20);
    assert!(lines.iter().any(|line| line == " y "), "{lines:?}");
}

#[test]
fn a_table_keeps_a_four_cell_floor_when_the_row_cannot_shrink_further() {
    let lines = plain_lines("| abcdefghij | klmnopqrst |\n| --- | --- |\n| x | y |\n", 6);
    assert_eq!(lines[0], "abcd  klmn");
    assert!(lines.iter().any(|line| line.contains('─')));
    assert!(lines.iter().all(|line| cols(line) >= 4), "{lines:?}");
}

#[test]
fn a_thirty_two_character_column_shrinks_before_a_short_one() {
    let long = "a".repeat(32);
    let short = "b".repeat(10);
    let lines = plain_lines(
        &format!(
            "| {long} | {short} | {short} |\n| --- | --- | --- |\n| {long} | {short} | {short} |\n"
        ),
        40,
    );
    let header = &lines[0];
    assert_eq!(header.matches(short.as_str()).count(), 2, "{header}");
    assert!(!header.contains(&long), "{header}");
    assert!(header.find(&short).expect("short") < 32, "{header}");
    assert!(lines.iter().any(|line| line.contains('─')));
}

#[test]
fn a_column_stops_at_half_the_room_before_it_wraps() {
    let long = "c".repeat(40);
    let lines = plain_lines(&format!("| {long} |\n| --- |\n| {long} |\n"), 30);
    assert!(lines.iter().all(|line| cols(line) <= 15), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.chars().all(|ch| ch == '─')),
        "{lines:?}"
    );
}

#[test]
fn a_cell_breaks_after_a_slash_and_prose_does_not() {
    let lines = plain_lines("| name |\n| --- |\n| pmdroid/barkvisor_private |\n", 20);
    assert!(
        lines.iter().any(|line| line.starts_with("pmdroid/")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("barkvisor_private") || line.contains("barkvisor")),
        "{lines:?}"
    );
    assert_eq!(
        plain_lines("pmdroid/barkvisor_private", 10),
        vec![
            "pmdroid/ba".to_string(),
            "rkvisor_pr".to_string(),
            "ivate".to_string()
        ]
    );
}

#[test]
fn a_wide_pr_table_keeps_labels_and_wraps_the_repository() {
    let lines = plain_lines(wide_pr_table(), 76);
    let joined = lines.join("\n");
    assert!(!joined.contains("https://"), "{joined}");
    assert!(
        lines.iter().any(|line| line.contains("kyotoagent")
            && line.contains("Install")
            && line.contains("#87")),
        "{joined}"
    );
    let header = lines
        .iter()
        .find(|line| line.contains("Title"))
        .expect("header");
    let validate = lines
        .iter()
        .find(|line| line.contains("Validate"))
        .expect("validate");
    assert_eq!(
        column_of(header, "Title"),
        column_of(validate, "Validate"),
        "{joined}"
    );
    let klar = lines
        .iter()
        .find(|line| line.contains("klar-magento"))
        .expect("klar");
    assert!(
        column_of(klar, "klar-magento") < column_of(header, "PR"),
        "{joined}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("pmdroid/") && !line.contains("barkvisor")),
        "{joined}"
    );
    assert!(
        lines.iter().any(|line| line.contains("barkvisor_private")),
        "{joined}"
    );
}

#[test]
fn an_arcade_pr_table_keeps_the_number_under_pr() {
    let lines = plain_lines(
        "| Repository | PR | Title | Approval | Conflicts |\n| --- | --- | --- | --- | --- |\n| ArcadeAI/monorepo | [#4858](https://github.com/ArcadeAI/monorepo/pull/4858) | Ship the parser | Required | No |\n",
        80,
    );
    let joined = lines.join("\n");
    assert!(!joined.contains("https://"), "{joined}");
    let header = lines
        .iter()
        .find(|line| line.contains("Repository"))
        .expect("header");
    let row = lines
        .iter()
        .find(|line| line.contains("#4858"))
        .expect("row");
    assert_eq!(column_of(header, "PR"), column_of(row, "#4858"), "{joined}");
    assert_eq!(
        column_of(header, "Approval"),
        column_of(row, "Required"),
        "{joined}"
    );
}

#[test]
fn a_link_in_a_cell_draws_the_label_without_the_url() {
    let text = "| tool | what |\n| --- | --- |\n| [read](https://example.com/read) | a file |\n";
    let lines = plain_lines(text, 80);
    let joined = lines.join("\n");
    assert!(joined.contains("read"), "{joined}");
    assert!(!joined.contains("https://"), "{joined}");
    assert!(!joined.contains('('), "{joined}");
}

#[test]
fn an_image_in_a_cell_draws_the_alt_text_without_the_url() {
    let lines = plain_lines(
        "| shot |\n| --- |\n| ![diagram](https://example.com/a.png) |\n",
        80,
    );
    let joined = lines.join("\n");
    assert!(joined.contains("diagram"), "{joined}");
    assert!(!joined.contains("https://"), "{joined}");
    assert!(!joined.contains('('), "{joined}");
}

fn wide_pr_table() -> &'static str {
    "\
| Repository | PR | Title | Approval | Conflicts | Draft |
| --- | --- | --- | --- | --- | --- |
| pmdroid/kyotoagent | [#87](https://github.com/pmdroid/kyotoagent/pull/87) | Install kyotoagent serve as a user service | None | No | No |
| pmdroid/barkvisor_private | [#4](https://github.com/pmdroid/barkvisor_private/pull/4) | Watch private builds | None | No | No |
| placeholder-tech/klar-magento-1 | [#1](https://github.com/placeholder-tech/klar-magento-1/pull/1) | Validate orders using response | None | No | No |
"
}

#[test]
fn a_table_is_separated_from_the_prose_around_it() {
    let lines = plain_lines("before\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\nafter\n", 40);
    assert_eq!(
        lines,
        vec![
            "before".to_string(),
            String::new(),
            "a  b".to_string(),
            "─  ─".to_string(),
            "1  2".to_string(),
            String::new(),
            "after".to_string(),
        ]
    );
}

#[test]
fn link_at_hits_a_link_inside_a_table_cell() {
    let text = "| tool | what |\n| --- | --- |\n| [read](https://example.com/read) | a file |\n";
    let lines = markdown_lines(text, 80);
    let row = lines
        .iter()
        .position(|line| line.to_string().contains("read"))
        .expect("read row");
    let line = lines[row].to_string();
    let url = "https://example.com/read";
    assert!(!line.contains(url), "{line}");
    assert_eq!(
        markdown_link_at(text, 80, row, column_of(&line, "read")).as_deref(),
        Some(url)
    );
    assert_eq!(
        markdown_link_at(text, 80, row, column_of(&line, "a file")),
        None
    );
}

#[test]
fn an_ask_keeps_pipe_table_markup() {
    let card = Card::ask("| tool | what |\n| --- | --- |\n| read | a file |");
    let joined = result_body(&card, 80).join("\n");
    assert!(joined.contains("| tool | what |"), "{joined}");
    assert!(joined.contains("| read | a file |"), "{joined}");
    assert!(!joined.contains('─'), "{joined}");
}

#[test]
fn a_quote_prefixes_each_wrapped_line_with_a_bar() {
    assert_eq!(
        plain_lines("> hello", 40),
        vec!["\u{258e} hello".to_string()]
    );
    assert_eq!(
        plain_lines("> > inner", 40),
        vec!["\u{258e} \u{258e} inner".to_string()]
    );
    assert_eq!(
        plain_lines("> alpha beta", 8),
        vec!["\u{258e} alpha".to_string(), "\u{258e} beta".to_string()]
    );
    assert_eq!(
        plain_lines("> hello\n>\n> there", 40),
        vec![
            "\u{258e} hello".to_string(),
            String::new(),
            "\u{258e} there".to_string()
        ]
    );
    assert_eq!(
        plain_lines("> - [x] ship", 40),
        vec!["\u{258e} [x] ship".to_string()]
    );
    let card = Card::result("> hello");
    let line = card
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains("hello"))
        .expect("quote");
    assert!(line.spans.iter().any(|span| {
        span.content.contains('\u{258e}') && span.style.fg == Some(theme::muted())
    }));
    let proof = Card::proof("> hello", &[]);
    let proof_body = result_body(&proof, 80).join("\n");
    assert!(proof_body.contains("\u{258e} hello"), "{proof_body}");
}

#[test]
fn strikethrough_crosses_out_the_span_and_drops_the_tildes() {
    let card = Card::result("keep ~~gone~~ here");
    let line = card
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains("gone"))
        .expect("struck");
    assert!(line.to_string().contains("keep gone here"), "{line:?}");
    assert!(!line.to_string().contains('~'), "{line:?}");
    assert!(line.spans.iter().any(|span| {
        span.content.contains("gone") && span.style.add_modifier.contains(Modifier::CROSSED_OUT)
    }));
    assert!(line.spans.iter().any(|span| {
        span.content.contains("keep") && !span.style.add_modifier.contains(Modifier::CROSSED_OUT)
    }));
}

#[test]
fn a_task_item_draws_a_checkbox_instead_of_a_bullet() {
    let lines = plain_lines("- [x] ship\n- [ ] stay", 40);
    assert_eq!(lines, vec!["[x] ship".to_string(), "[ ] stay".to_string()]);
    assert!(
        lines.iter().all(|line| !line.contains('\u{2022}')),
        "{lines:?}"
    );
}

#[test]
fn a_bare_https_url_is_a_link_hit() {
    let url = "https://example.com";
    let text = "see https://example.com now";
    let lines = markdown_lines(text, 80);
    let row = lines
        .iter()
        .position(|line| line.to_string().contains(url))
        .expect("url line");
    let line = lines[row].to_string();
    assert_eq!(line.matches(url).count(), 1, "{line}");
    assert_eq!(
        markdown_link_at(text, 80, row, column_of(&line, url)).as_deref(),
        Some(url)
    );
    assert_eq!(
        markdown_link_at(text, 80, row, column_of(&line, "see")),
        None
    );
    let card = Card::result(text);
    let drawn = card
        .lines(80)
        .into_iter()
        .find(|line| line.to_string().contains(url))
        .expect("card url");
    assert!(
        drawn.spans.iter().any(|span| span_is_link(span, url)),
        "{drawn:?}"
    );
    let quoted = "> see https://example.com";
    let quoted_lines = markdown_lines(quoted, 80);
    let quoted_row = quoted_lines
        .iter()
        .position(|line| line.to_string().contains(url))
        .expect("quoted url");
    let quoted_line = quoted_lines[quoted_row].to_string();
    assert!(quoted_line.contains("\u{258e} see"), "{quoted_line}");
    assert_eq!(
        markdown_link_at(quoted, 80, quoted_row, column_of(&quoted_line, url)).as_deref(),
        Some(url)
    );
    let dotted = "https://example.com.";
    let dotted_lines = markdown_lines(dotted, 80);
    let dotted_line = dotted_lines[0].to_string();
    assert!(dotted_line.ends_with('.'), "{dotted_line}");
    assert_eq!(
        markdown_link_at(dotted, 80, 0, column_of(&dotted_line, url)).as_deref(),
        Some(url)
    );
    assert_eq!(
        markdown_link_at(dotted, 80, 0, cols(&dotted_line) - 1),
        None
    );
    let http = "http://example.com";
    let http_text = "see http://example.com now";
    let http_lines = markdown_lines(http_text, 80);
    let http_line = http_lines[0].to_string();
    assert_eq!(
        markdown_link_at(http_text, 80, 0, column_of(&http_line, http)).as_deref(),
        Some(http)
    );
    let code = "run `https://example.com`";
    let code_lines = markdown_lines(code, 80);
    let code_row = code_lines
        .iter()
        .position(|line| line.to_string().contains(url))
        .expect("code url");
    let code_line = code_lines[code_row].to_string();
    assert_eq!(
        markdown_link_at(code, 80, code_row, column_of(&code_line, url)),
        None
    );
}

#[test]
fn link_at_hits_a_bare_url_on_a_result_card_and_in_the_proof_overlay() {
    let url = "https://example.com";
    let text = "see https://example.com now";
    let mut model = crate::mock::idle();
    model.cards = vec![Card::result(text)];
    let area = Rect::new(0, 0, 76, 24);
    let rows = grid(&model);
    let (url_x, url_y) = find_phrase(&rows, url);
    assert_eq!(link_at(&model, area, url_x, url_y).as_deref(), Some(url));
    let (plain_x, plain_y) = find_phrase(&rows, "see");
    assert_eq!(link_at(&model, area, plain_x, plain_y), None);
    model.cards = vec![Card::result("The quote stays on the proof.")];
    model.overlay = Some(Overlay::Proof {
        text: text.into(),
        items: vec![ItemRun {
            id: "test".into(),
            kind: ItemKind::Command,
            outcome: Outcome::Passed,
            argv: Vec::new(),
            exit: None,
            tail: String::new(),
        }],
    });
    let rows = grid(&model);
    let (url_x, url_y) = find_phrase(&rows, url);
    assert_eq!(link_at(&model, area, url_x, url_y).as_deref(), Some(url));
}

#[test]
fn an_ask_keeps_a_quote_marker() {
    let card = Card::ask("> quote");
    let joined = result_body(&card, 40).join("\n");
    assert!(joined.contains("> quote"), "{joined}");
    assert!(!joined.contains("\u{258e} quote"), "{joined}");
}

#[test]
fn a_diff_reads_green_for_an_addition_and_red_for_a_removal() {
    assert_eq!(
        diff_style("+kyotoagent is the binary.").fg,
        Some(theme::good())
    );
    assert_eq!(
        diff_style("-kyotoagent is the binary.").fg,
        Some(theme::bad())
    );
    assert_eq!(diff_style("@@ -1 +1,2 @@").fg, Some(theme::muted()));
}

#[test]
fn an_ask_keeps_the_right_two_thirds_and_a_result_the_left() {
    let pane = 44;
    let ask = Card::ask("Add a readme line that names the binary.");
    for line in ask.lines(pane) {
        let text = line.to_string();
        let rail = text.rfind('\u{258e}').expect("rail");
        assert!(
            rail + 1 == pane,
            "ask rail sits on the right edge: {text:?}"
        );
        let ink = text.find(|ch: char| !ch.is_whitespace()).expect("ink");
        assert!(
            ink >= pane / 3,
            "ask text stays out of the left third: {text:?}"
        );
    }
    let result = Card::result("The readme now names the binary under the heading Kyoto Agent.");
    for line in result.lines(pane) {
        let text = line.to_string();
        assert!(
            text.starts_with(RAIL) || text.starts_with(RAIL_DIM),
            "result stays left: {text:?}"
        );
        assert!(
            text.chars().count() <= pane * 2 / 3,
            "result stays in the left two thirds: {text:?}"
        );
    }
    let permission = Card::permission("Replace README.md", None, &["+kyotoagent is the binary."]);
    let headline = permission.lines(pane)[1].to_string();
    assert!(headline.starts_with(RAIL_DIM));
    assert!(headline.contains("Replace README.md"));
    assert!(headline.chars().count() <= pane);
}

#[test]
fn a_card_kind_is_labelled_in_caps_without_a_block_of_its_own() {
    let card = Card::ask("Something.");
    let badge = &card.lines(40)[0];
    let text = badge.to_string();
    assert!(text.ends_with("ASK \u{258e}"), "{text:?}");
    let style = badge
        .spans
        .iter()
        .find(|span| span.content == "ASK")
        .expect("badge")
        .style;
    assert_eq!(style.fg, Some(theme::ask()));
    assert_eq!(style.bg, None, "the badge must not fill a block");
}

#[test]
fn each_card_kind_has_its_own_colour() {
    let colors = [
        Card::ask("a").color(),
        Card::question("q", &[]).color(),
        Card::result("r").color(),
        Card::proof("", &[]).color(),
    ];
    for (index, color) in colors.iter().enumerate() {
        assert!(
            !colors[..index].contains(color),
            "two card kinds share the colour {color:?}"
        );
    }
}

#[test]
fn a_turn_that_ran_no_items_lists_none_rather_than_hiding_the_list() {
    let text: Vec<String> = Card::proof("Wrote README.md", &[])
        .lines(40)
        .iter()
        .map(Line::to_string)
        .collect();
    assert_eq!(
        text.len(),
        2,
        "the badge and the one line it names: {text:?}"
    );
    assert!(!text.iter().any(|line| line.contains("passed")));
}

#[test]
fn a_proof_renders_carried_checks_on_the_card_and_overlay() {
    let outcome = Outcome::from_label("passed_earlier").unwrap();
    let card = Card::proof("checked", &[("docs", ItemKind::Command, outcome)]);
    assert!(card
        .lines(60)
        .iter()
        .any(|line| line.to_string().contains("✓ docs (earlier)")));
    let Card::Proof { items, .. } = card else {
        panic!("expected proof")
    };
    let lines = overlay_lines(
        &Overlay::Proof {
            text: "checked".into(),
            items,
        },
        60,
    );
    assert!(lines
        .iter()
        .any(|line| line.to_string().contains("✓ docs (earlier)")));
}

#[test]
fn a_proof_lists_every_item_that_ran_with_its_kind_and_outcome() {
    let card = Card::proof(
        "cargo test passed.",
        &[
            ("test", ItemKind::Command, Outcome::Passed),
            ("visual", ItemKind::Visual, Outcome::Failed),
        ],
    );
    let text: Vec<String> = card.lines(40).iter().map(Line::to_string).collect();
    assert_eq!(
        text,
        vec![
            "\u{258e} PROOF".to_string(),
            "\u{2502} cargo test passed.".to_string(),
        ]
    );
    let Card::Proof { items, .. } = &card else {
        panic!("a proof card");
    };
    let overlay: Vec<String> = overlay_lines(
        &Overlay::Proof {
            text: "cargo test passed.".into(),
            items: items.clone(),
        },
        40,
    )
    .iter()
    .map(Line::to_string)
    .collect();
    assert!(overlay
        .iter()
        .any(|line| line.contains("cargo test passed.")));
    assert!(!overlay.iter().any(|line| line.contains("visual")));
}

/// The kind on a line is the spelling the file uses, because the id the
/// model passes to `run_closeout` and the kind the file stored have to
/// read the same.
#[test]
fn every_item_kind_is_named_the_way_the_file_names_it() {
    let kinds = [
        (ItemKind::Command, "command"),
        (ItemKind::Cucumber, "cucumber"),
        (ItemKind::Visual, "visual"),
        (ItemKind::Receipt, "receipt"),
        (ItemKind::Review, "review"),
        (ItemKind::Ci, "ci"),
        (ItemKind::ReviewThreads, "reviewThreads"),
    ];
    for (kind, label) in kinds {
        assert_eq!(kind.label(), label);
        assert_eq!(ItemKind::from_label(label), Some(kind));
    }
    assert_eq!(Outcome::from_label("passed"), Some(Outcome::Passed));
    assert_eq!(Outcome::from_label("failed"), Some(Outcome::Failed));
}

#[test]
fn padding_fits_exactly() {
    assert_eq!(pad("ab", 5), "ab   ");
    assert_eq!(pad("abcdef", 3), "abc");
    // Padded by characters, not bytes: the marker and the name together
    // have to fill the row exactly.
    assert_eq!(pad("\u{25b6} kyotoagent", 12).chars().count(), 12);
    assert_eq!(pad("\u{25b6} kyotoagent", 12), "\u{25b6} kyotoagent");
}

fn draw_buffer(model: &ScreenModel) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(76, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(model, frame.area(), frame))
        .expect("the screen draws");
    terminal.backend().buffer().clone()
}

fn draw(model: &ScreenModel) -> String {
    let buffer = draw_buffer(model);
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[test]
fn a_titled_row_draws_that_title_in_the_list() {
    let mut model = crate::mock::idle();
    model.sessions[0].title = Some("Name the binary".into());
    let text = draw(&model);
    assert!(text.contains("Name the binary"), "{text}");
    assert!(text.contains("acpbot"), "{text}");
    assert!(text.contains("notes"), "{text}");
}

#[test]
fn the_header_shows_yolo_when_it_is_on() {
    let mut model = crate::mock::waiting();
    assert!(!draw(&model).contains("yolo"));
    model.yolo = true;
    let text = draw(&model);
    assert!(text.contains("yolo"), "{text}");
    assert!(text.contains("+kyotoagent is the binary."), "{text}");
}

/// The `yolo` word is a rainbow, and the tick shifts it. Read the header
/// row's colours straight off the buffer, because the text is the same on
/// every tick: only the paint animates.
#[test]
fn the_yolo_word_stays_in_one_color_across_ticks() {
    fn header_colors(tick: usize) -> Vec<Color> {
        let mut model = crate::mock::waiting();
        model.yolo = true;
        model.tick = tick;
        let backend = ratatui::backend::TestBackend::new(76, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
        terminal
            .draw(|frame| render(&model, frame.area(), frame))
            .expect("the screen draws");
        let buffer = terminal.backend().buffer();
        (0..76)
            .map(|x| buffer[(x, 0)].style().fg.unwrap_or(Color::Reset))
            .collect()
    }
    let first = header_colors(0);
    let second = header_colors(1);
    // Columns 1..7 carry "kyotoagent" in the single accent colour.
    let name_at = 3;
    let name_end = name_at + "Kyoto Agent".chars().count();
    assert!(
        first[name_at..name_end]
            .iter()
            .all(|color| *color == theme::accent()),
        "{first:?}"
    );
    // Columns 9..13 are the letters of "yolo", painted from the rainbow
    // rather than one flat colour.
    let yolo_at = name_end + 2;
    let letters: Vec<Color> = first[yolo_at..yolo_at + 4].to_vec();
    assert!(
        letters.iter().all(|color| *color == YOLO_RAINBOW[1]),
        "yolo status color: {letters:?}"
    );
    // One tick shifts the pattern by one column, so the letters move.
    assert_eq!(first, second, "yolo status should stay still");
    // A tick advances the pattern, so each column takes the colour its
    // right neighbour had one tick ago.
    for column in yolo_at..yolo_at + 4 {
        assert_eq!(second[column], first[column]);
    }
}

#[test]
fn the_header_shows_a_faint_percent_and_a_click_hits_that_span() {
    let mut model = crate::mock::idle();
    let plain = draw(&model);
    assert!(plain.contains(" Kyoto Agent "), "{plain}");
    assert!(plain.contains(LIST_GLYPH), "{plain}");
    assert!(plain.contains(PANES_GLYPH), "{plain}");
    assert!(!plain.contains('%'), "{plain}");
    model.context_percent = Some(12);
    let text = draw(&model);
    assert!(text.contains(" Kyoto Agent  12%"), "{text}");
    let area = Rect::new(0, 0, 76, 24);
    assert!(context_at(&model, area, 16, 0));
    assert!(!context_at(&model, area, 3, 0));
    model.yolo = true;
    let yolo = draw(&model);
    assert!(yolo.contains("Kyoto Agent"), "{yolo}");
    assert!(yolo.contains("yolo"), "{yolo}");
    assert!(yolo.contains("12%"), "{yolo}");
    assert!(context_at(&model, area, 22, 0));
    assert!(!context_at(&model, area, 16, 0));
    let backend = ratatui::backend::TestBackend::new(76, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(22, 0)].style().fg, Some(theme::muted()));
}

#[test]
fn a_context_overlay_lists_the_five_buckets() {
    let mut model = crate::mock::idle();
    let cards = model.cards.clone();
    model.context_percent = Some(12);
    model.overlay = Some(Overlay::Context {
        percent: 12,
        used: 31_200,
        reported_prompt_tokens: Some(30_000),
        window: 256_000,
        buckets: vec![
            ContextLine {
                id: "system".into(),
                tokens: Some(2_100),
            },
            ContextLine {
                id: "tools".into(),
                tokens: Some(4_800),
            },
            ContextLine {
                id: "skills".into(),
                tokens: Some(400),
            },
            ContextLine {
                id: "messages".into(),
                tokens: Some(23_900),
            },
            ContextLine {
                id: "free".into(),
                tokens: Some(224_800),
            },
        ],
    });
    let text = draw(&model);
    assert!(text.contains("CONTEXT  12%"), "{text}");
    assert!(text.contains("Estimated current request"), "{text}");
    assert!(text.contains("Last request  30,000"), "{text}");
    assert!(text.contains("31,200 / 256,000"), "{text}");
    for id in ["system", "tools", "skills", "messages", "free"] {
        assert!(text.contains(id), "{id} missing: {text}");
    }
    assert_eq!(model.cards, cards);
}

#[test]
fn the_header_shows_the_selected_model_and_effort() {
    let mut model = crate::mock::waiting();
    model.model = "grok-4.6".into();
    let text = draw(&model);
    assert!(text.contains(" Kyoto Agent "), "{text}");
    assert!(text.contains(LIST_GLYPH), "{text}");
    assert!(text.contains("grok-4.6 \u{00b7} 3 sessions"), "{text}");
    model.effort = Some("high".into());
    let text = draw(&model);
    assert!(text.contains("grok-4.6 high \u{00b7} 3 sessions"), "{text}");
    model.compacting = true;
    let text = draw(&model);
    assert!(text.contains("compact"), "{text}");
    assert!(
        text.contains("compacting \u{00b7} grok-4.6 high \u{00b7} 3 sessions"),
        "{text}"
    );
}

#[test]
fn the_header_shows_the_profile_name_only_when_one_is_set() {
    let mut model = crate::mock::waiting();
    model.model = "grok-4.6".into();
    let text = draw(&model);
    assert!(text.contains(" kyotoagent "), "{text}");
    assert!(!text.contains("review"), "{text}");
    model.sessions[0].profile = Some("review".into());
    let text = draw(&model);
    assert!(text.contains(" kyotoagent "), "{text}");
    assert!(text.contains("review \u{00b7} grok-4.6"), "{text}");
    model.sessions[0].profile = None;
    let text = draw(&model);
    assert!(!text.contains("review"), "{text}");
}

#[test]
fn a_model_overlay_lists_the_highlighted_row() {
    let mut model = crate::mock::idle();
    model.overlay = Some(Overlay::Model {
        rows: vec!["grok-4.6".into(), "grok-4.5".into()],
        highlight: 1,
    });
    let text = draw(&model);
    assert!(text.contains("grok-4.6"), "{text}");
    assert!(text.contains("grok-4.5"), "{text}");
}

#[test]
fn a_permission_overlay_sits_on_the_quiet_pane() {
    let mut model = crate::mock::waiting();
    model.overlay = Some(Overlay::Permission {
        action: "Replace README.md".into(),
        diff: vec!["@@ -1 +1,2 @@".into(), "+kyotoagent is the binary.".into()],
        argv: Vec::new(),
    });
    let text = draw(&model);
    assert!(text.contains("Replace README.md"), "{text}");
    assert!(text.contains("+kyotoagent is the binary."), "{text}");
    assert!(text.contains("a once"), "{text}");
    assert!(text.contains("ASK"), "{text}");
}

fn live_run_word() -> String {
    "w".repeat(80)
}

fn live_run_model(open: bool) -> ScreenModel {
    let word = live_run_word();
    let mut model = crate::mock::closeout_run();
    model.cards = vec![
        Card::ask("Add a readme line that names the binary."),
        Card::command("Run sh", None, &["sh", "-c", &word]),
    ];
    if open {
        model.overlay = Some(Overlay::Permission {
            action: "Run sh".into(),
            diff: Vec::new(),
            argv: vec!["sh".into(), "-c".into(), word],
        });
    }
    model
}

#[test]
fn a_live_command_permission_draws() {
    let text = draw(&live_run_model(false));
    assert!(text.contains("PERMISSION"), "{text}");
    assert!(text.contains("Run sh"), "{text}");
}

#[test]
fn a_live_command_permission_overlay_draws_inside_the_frame() {
    let model = live_run_model(true);
    let text = draw(&model);
    assert!(text.contains("PERMISSION"), "{text}");
    assert!(text.contains("Run sh"), "{text}");
    let frame = Rect::new(0, 0, 76, 24);
    let pane = split_of(&model, frame).session;
    let overlay = model.overlay.as_ref().expect("overlay");
    let lines = overlay_lines(overlay, overlay_inner_width(pane));
    let area = overlay_rect(pane, lines.len());
    assert!(area.x >= frame.x);
    assert!(area.y >= frame.y);
    assert!(area.x + area.width <= frame.x + frame.width);
    assert!(area.y + area.height <= frame.y + frame.height);
    assert!(area.x >= pane.x);
    assert!(area.y >= pane.y);
    assert!(area.x + area.width <= pane.x + pane.width);
    assert!(area.y + area.height <= pane.y + pane.height);
}

#[test]
fn a_narrow_permission_overlay_draws() {
    let overlay = Overlay::Permission {
        action: "Run sh".into(),
        diff: Vec::new(),
        argv: vec!["sh".into(), "-c".into(), live_run_word()],
    };
    let pane = Rect::new(0, 0, 10, 8);
    let lines = overlay_lines(&overlay, overlay_inner_width(pane));
    let area = overlay_rect(pane, lines.len());
    assert!(area.x + area.width <= pane.x + pane.width);
    assert!(area.y + area.height <= pane.y + pane.height);
    let backend = ratatui::backend::TestBackend::new(10, 8);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    let model = ScreenModel {
        overlay: Some(overlay),
        ..Default::default()
    };
    terminal
        .draw(|frame| render_overlay(&model, pane, pane, frame))
        .expect("the overlay stays inside a narrow pane");
}

#[test]
fn a_command_permission_joins_argv() {
    let card = Card::command("Run sh", None, &["sh", "-c", "cargo test"]);
    let text: Vec<String> = card.lines(40).iter().map(Line::to_string).collect();
    assert!(
        text.iter().any(|line| line.contains("sh -c cargo test")),
        "{text:?}"
    );
}

#[test]
fn a_question_overlay_shows_choices_and_the_prompt() {
    let mut model = crate::mock::closeout_asked();
    model.overlay = Some(Overlay::Question {
        text: "Which way?".into(),
        choices: vec![
            Choice {
                label: "continue".into(),
                marked: false,
            },
            Choice {
                label: "stop".into(),
                marked: false,
            },
        ],
        prompt: "hello".into(),
    });
    let text = draw(&model);
    assert!(text.contains("Which way?"), "{text}");
    assert!(text.contains("1  continue"), "{text}");
    assert!(text.contains("2  stop"), "{text}");
    assert!(text.contains("hello"), "{text}");
}

#[test]
fn a_click_hits_a_waiting_permission_and_misses_an_answered_one() {
    let area = Rect::new(0, 0, 76, 24);
    let waiting = crate::mock::waiting();
    let mut hit = false;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if waiting_card_at(&waiting, area, x, y) {
                hit = true;
            }
        }
    }
    assert!(hit, "the waiting permission is clickable");
    let idle = crate::mock::idle();
    for y in 0..24u16 {
        for x in 0..76u16 {
            assert!(
                !waiting_card_at(&idle, area, x, y),
                "an answered card does not open"
            );
        }
    }
}

#[test]
fn a_waiting_question_is_clickable() {
    let area = Rect::new(0, 0, 76, 24);
    let asked = crate::mock::closeout_asked();
    let mut hit = false;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if waiting_card_at(&asked, area, x, y) {
                hit = true;
            }
        }
    }
    assert!(hit, "the waiting question is clickable");
}

#[test]
fn a_proof_overlay_shows_the_sentence_and_not_the_checks() {
    let mut model = crate::mock::idle();
    model.overlay = Some(Overlay::Proof {
        text: "cargo test passed on the readme heading.".into(),
        items: vec![
            ItemRun {
                id: "test".into(),
                kind: ItemKind::Command,
                outcome: Outcome::Passed,
                argv: Vec::new(),
                exit: None,
                tail: String::new(),
            },
            ItemRun {
                id: "lint".into(),
                kind: ItemKind::Command,
                outcome: Outcome::Failed,
                argv: vec!["cargo".into(), "clippy".into()],
                exit: Some(1),
                tail: "error: unused".into(),
            },
        ],
    });
    let text = draw(&model);
    assert!(text.contains("cargo test passed"), "{text}");
    assert!(!text.contains("error: unused"), "{text}");
    assert!(!text.contains("cargo clippy"), "{text}");
}

#[test]
fn a_proof_with_text_is_clickable_and_a_blank_one_is_not() {
    let area = Rect::new(0, 0, 76, 24);
    let idle = crate::mock::idle();
    let mut hit = false;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if overlay_card_at(&idle, area, x, y) {
                hit = true;
            }
        }
    }
    assert!(hit, "a proof with text opens");
    let mut blank = crate::mock::idle();
    blank.cards = vec![Card::proof(
        "",
        &[("test", ItemKind::Command, Outcome::Passed)],
    )];
    for y in 0..24u16 {
        for x in 0..76u16 {
            assert!(
                !overlay_card_at(&blank, area, x, y),
                "a proof with no text stays on the pane"
            );
        }
    }
}

fn idle_with_pull() -> ScreenModel {
    let mut model = crate::mock::idle();
    model.left_width = 20;
    let url = format!(
        "{}/pmdroid/kyotoagent/pull/14",
        crate::session::github_origin()
    );
    model.sessions[0].pull_url = Some(url);
    model
}

#[test]
fn a_session_with_a_pull_url_shows_the_pr_mark_and_one_without_hides_it() {
    let with = draw(&idle_with_pull());
    assert!(with.contains("pr 14"), "{with}");
    assert!(with.contains("Opened"), "{with}");
    assert!(with.contains("pull/14"), "{with}");
    let waiting = draw(&crate::mock::waiting());
    assert!(!waiting.contains("pr 14"), "{waiting}");
    assert!(!waiting.contains("Opened"), "{waiting}");
    let working = draw(&crate::mock::working());
    assert!(!working.contains("pr 14"), "{working}");
    assert!(!working.contains("Opened"), "{working}");
    let idle = draw(&crate::mock::idle());
    assert!(!idle.contains("pr 14"), "{idle}");
    assert!(!idle.contains("Opened"), "{idle}");
}

#[test]
fn a_pull_overlay_shows_the_full_url() {
    let mut model = idle_with_pull();
    let url = model.sessions[0].pull_url.clone().expect("url");
    model.overlay = Some(Overlay::Pull { url: url.clone() });
    let text = draw(&model);
    assert!(text.contains(&url), "{text}");
    assert!(text.contains("pr 14"), "{text}");
}

#[test]
fn a_click_on_the_title_or_opened_line_hits_the_pull_target() {
    let area = Rect::new(0, 0, 76, 24);
    let model = idle_with_pull();
    let mut hit = false;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if pull_target_at(&model, area, x, y) {
                hit = true;
            }
        }
    }
    assert!(hit, "the title or opened line is clickable");
    let idle = crate::mock::idle();
    for y in 0..24u16 {
        for x in 0..76u16 {
            assert!(
                !pull_target_at(&idle, area, x, y),
                "a session with no pull request has no mark"
            );
        }
    }
}

#[test]
fn a_skill_picker_lists_the_matching_names_above_the_prompt() {
    let mut model = crate::mock::idle();
    model.bottom = "/pre".to_string();
    model.skill_picker = Some(SkillPicker {
        rows: vec![
            SkillPickerRow {
                name: "preflight".into(),
                description: "Ship checks".into(),
            },
            SkillPickerRow {
                name: "preview".into(),
                description: "Preview a change".into(),
            },
        ],
        selected: 0,
    });
    let text = draw(&model);
    assert!(text.contains("/preflight"), "{text}");
    assert!(text.contains("Ship checks"), "{text}");
    assert!(text.contains("/preview"), "{text}");
    assert!(text.contains("Preview a change"), "{text}");
    assert!(text.contains("/pre"), "{text}");
    let waiting = draw(&crate::mock::waiting());
    assert!(!waiting.contains("/preflight"), "{waiting}");
    let idle = draw(&crate::mock::idle());
    assert!(!idle.contains("/preflight"), "{idle}");
}

#[test]
fn the_prompt_row_uses_the_selected_wash() {
    let area = Rect::new(0, 0, 76, 24);
    let mut prompt = crate::mock::idle();
    prompt.bottom.clear();
    let split = split_of(&prompt, area);
    let buffer = draw_buffer(&prompt);
    for x in 0..buffer.area.width {
        assert_eq!(
            buffer[(x, split.input.y + 1)].style().bg,
            Some(theme::selected_bg()),
            "column {x}"
        );
    }
    let row = (0..buffer.area.width)
        .map(|x| buffer[(x, split.input.y + 1)].symbol())
        .collect::<String>();
    assert!(row.contains('\u{203a}'), "{row}");
    assert!(row.contains('\u{2588}'), "{row}");

    prompt.bottom = "hello".into();
    let typed = draw_buffer(&prompt);
    let hello = (0..typed.area.width)
        .find(|x| typed[(*x, split.input.y + 1)].symbol() == "h")
        .expect("typed text");
    let cell = typed[(hello, split.input.y + 1)].style();
    assert_eq!(cell.fg, Some(theme::text()));
    assert_eq!(cell.bg, Some(theme::selected_bg()));

    let keys = crate::mock::waiting();
    let keys_split = split_of(&keys, area);
    let keys_buffer = draw_buffer(&keys);
    for x in 0..keys_buffer.area.width {
        assert_eq!(
            keys_buffer[(x, keys_split.input.y)].style().bg,
            Some(theme::selected_bg())
        );
    }

    let blank = crate::mock::empty();
    let blank_split = split_of(&blank, area);
    let blank_buffer = draw_buffer(&blank);
    for x in 0..blank_buffer.area.width {
        assert_ne!(
            blank_buffer[(x, blank_split.input.y)].style().bg,
            Some(theme::selected_bg())
        );
    }
}

#[test]
fn header_glyphs_are_accent_when_open_and_faint_when_closed() {
    let area = Rect::new(0, 0, 76, 24);
    let mut model = crate::mock::idle();
    model.right_open = true;
    model.right_panes.insert(RightPane::Todos);
    let open = draw_buffer(&model);
    let open_style = open[(1, 0)].style();
    assert_eq!(open[(1, 0)].symbol(), LIST_GLYPH);
    assert_eq!(open_style.fg, Some(theme::accent()));
    assert!(open_style.add_modifier.contains(Modifier::BOLD));
    let panes = panes_glyph_column(&model, area.width).expect("panes");
    let panes_style = open[(panes, 0)].style();
    assert_eq!(open[(panes, 0)].symbol(), PANES_GLYPH);
    assert_eq!(panes_style.fg, Some(theme::accent()));
    assert!(panes_style.add_modifier.contains(Modifier::BOLD));

    model.left_open = false;
    model.right_open = false;
    let closed = draw_buffer(&model);
    assert_eq!(closed[(1, 0)].symbol(), LIST_GLYPH);
    assert_eq!(closed[(1, 0)].style().fg, Some(theme::muted()));
    let panes = panes_glyph_column(&model, area.width).expect("panes stay");
    assert_eq!(closed[(panes, 0)].symbol(), PANES_GLYPH);
    assert_eq!(closed[(panes, 0)].style().fg, Some(theme::muted()));
}

#[test]
fn a_long_session_list_scrolls_and_keeps_clicks_on_the_visible_row() {
    let mut model = crate::mock::idle();
    model.sessions = (0..12)
        .map(|index| {
            let mut row = model.sessions[0].clone();
            row.id = format!("id{index:02}");
            row.title = Some(format!("session {index:02}"));
            row
        })
        .collect();
    model.selected = "id00".into();
    let area = Rect::new(0, 0, 76, 24);
    assert!(list_scroll_max(&model, area) > 0);
    let top = draw(&model);
    assert!(top.contains("session 00"), "{top}");
    assert!(!top.contains("session 11"), "{top}");
    model.list_scroll = list_scroll_max(&model, area);
    let bottom = draw(&model);
    assert!(!bottom.contains("session 00"), "{bottom}");
    assert!(bottom.contains("session 11"), "{bottom}");
    let list = split_of(&model, area).list;
    let inner = Block::bordered().inner(list);
    let session_row = inner.y + u16::from(inner.height > 1);
    let top_row = list_at(&model, area, inner.x + 2, session_row);
    assert_ne!(top_row, Some(ListHit::Session("id00".into())));
    assert!(matches!(top_row, Some(ListHit::Session(_))));
    model.list_scroll = 0;
    assert_eq!(
        list_at(&model, area, inner.x + 2, session_row),
        Some(ListHit::Session("id00".into()))
    );
    model.selected = "id11".into();
    model.list_scroll = list_scroll_for(&model, area, 0);
    let followed = draw(&model);
    assert!(followed.contains("session 11"), "{followed}");
    assert!(!followed.contains("session 00"), "{followed}");
}

#[test]
fn an_empty_todo_list_keeps_two_columns() {
    let area = Rect::new(0, 0, 76, 24);
    let split = split_of(&crate::mock::waiting(), area);
    assert_eq!(split.list.width, LIST_WIDTH);
    assert_eq!(split.todos.width, 0);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    assert_eq!(split.session.width, 46);
}

#[test]
fn an_open_todo_pane_takes_the_right_and_shows_progress() {
    let area = Rect::new(0, 0, 76, 24);
    let model = crate::mock::todos();
    let split = split_of(&model, area);
    assert_eq!(split.list.width, LIST_WIDTH);
    assert_eq!(split.todos.width, TODOS_WIDTH);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    let text = draw(&model);
    assert!(text.contains("Write the todo tool"), "{text}");
    assert!(text.contains("Read the crate"), "{text}");
    assert!(text.contains("Draw the right pane"), "{text}");
    assert!(text.contains("1/3"), "{text}");
    assert!(text.contains("2 left"), "{text}");
    assert!(text.contains('\u{2588}'), "{text}");
    assert!(text.contains('\u{2591}'), "{text}");
    assert!(!text.contains("Replace content with title"), "{text}");
    assert!(!text.contains("src/events.rs"), "{text}");
}

#[test]
fn an_expanded_todo_lists_the_plan_files_and_links() {
    let model = crate::mock::todos_open();
    assert!(model.overlay.is_none());
    let area = Rect::new(0, 0, 76, 24);
    let split = split_of(&model, area);
    assert_eq!(split.session.width, CARD_MIN);
    let text = draw(&model);
    assert!(text.contains("Write the todo"), "{text}");
    assert!(text.contains("in_progress"), "{text}");
    assert!(text.contains("Replace content"), "{text}");
    assert!(text.contains("src/events.rs"), "{text}");
    assert!(text.contains("src/turn.rs"), "{text}");
    assert!(text.contains("docs.rs"), "{text}");
    let closed = draw(&crate::mock::todos());
    assert!(closed.contains("Write the todo tool"), "{closed}");
    assert!(!closed.contains("Replace content with title"), "{closed}");
    assert!(!closed.contains("src/events.rs"), "{closed}");
    assert!(!closed.contains("docs.rs/serde"), "{closed}");
}

#[test]
fn an_idle_session_with_an_empty_ask_draws_the_prompt() {
    let mut model = crate::mock::idle();
    model.bottom.clear();
    model.bottom_kind = Bottom::Prompt;
    let text = draw(&model);
    assert!(
        text.contains(" \u{203a} \u{2588}"),
        "empty idle prompt: {text}"
    );
    let working = draw(&crate::mock::working());
    assert!(
        working.contains(" \u{203a} \u{2588}"),
        "working draws the empty prompt: {working}"
    );
    model.bottom = "hello".into();
    let mut queued = crate::mock::working();
    queued.queue = 1;
    let queued_text = draw(&queued);
    assert!(
        queued_text.contains("queued 1"),
        "the footer names the queue: {queued_text}"
    );
    let mut thinking = crate::mock::thinking();
    thinking.queue = 2;
    let thinking_text = draw(&thinking);
    assert!(
        thinking_text.contains("Thinking"),
        "thinking stays the footer word: {thinking_text}"
    );
    assert!(
        thinking_text.contains("queued 2"),
        "thinking names the queue: {thinking_text}"
    );
    let typed = draw(&model);
    assert!(
        typed.contains(" \u{203a} hello\u{2588}"),
        "typed idle prompt: {typed}"
    );
}

fn docs_rs_serde() -> String {
    let mut url = String::from("https:");
    url.push('/');
    url.push('/');
    url.push_str("docs.rs/serde");
    url
}

fn todo_file_overlay() -> ScreenModel {
    crate::mock::todos_open()
}

fn hit_path(
    model: &ScreenModel,
    area: Rect,
    want: &str,
    finder: fn(&ScreenModel, Rect, u16, u16) -> Option<String>,
) -> (u16, u16) {
    for y in 0..area.height {
        for x in 0..area.width {
            if finder(model, area, x, y).as_deref() == Some(want) {
                return (x, y);
            }
        }
    }
    panic!("no hit for {want}");
}

#[test]
fn file_at_hits_an_expanded_todo_file() {
    let model = todo_file_overlay();
    let area = Rect::new(0, 0, 76, 24);
    let (x, y) = hit_path(&model, area, "src/events.rs", file_at);
    assert_eq!(
        file_at(&model, area, x, y).as_deref(),
        Some("src/events.rs")
    );
    let (x, y) = hit_path(&model, area, "src/turn.rs", file_at);
    assert_eq!(file_at(&model, area, x, y).as_deref(), Some("src/turn.rs"));
    let title = split_of(&model, area).session;
    assert_eq!(file_at(&model, area, title.x + 2, title.y), None);
}

#[test]
fn link_at_hits_an_expanded_todo_link() {
    let model = todo_file_overlay();
    let area = Rect::new(0, 0, 76, 24);
    let url = docs_rs_serde();
    let (x, y) = hit_path(&model, area, &url, link_at);
    assert_eq!(link_at(&model, area, x, y).as_deref(), Some(url.as_str()));
    assert_eq!(file_at(&model, area, x, y), None);
}

#[test]
fn file_at_hits_a_proof_wrote_line() {
    let mut model = crate::mock::idle();
    model.cards = vec![Card::proof("Wrote README.md", &[])];
    let area = Rect::new(0, 0, 76, 24);
    let (x, y) = hit_path(&model, area, "README.md", file_at);
    assert_eq!(file_at(&model, area, x, y).as_deref(), Some("README.md"));
}

#[test]
fn link_at_hits_a_pull_overlay_url() {
    let mut model = crate::mock::idle();
    let url = format!(
        "{}/pmdroid/kyotoagent/pull/14",
        crate::session::github_origin()
    );
    model.sessions[0].pull_url = Some(url.clone());
    model.overlay = Some(Overlay::Pull { url: url.clone() });
    let area = Rect::new(0, 0, 76, 24);
    let (x, y) = hit_path(&model, area, &url, link_at);
    assert_eq!(link_at(&model, area, x, y).as_deref(), Some(url.as_str()));
    assert_eq!(file_at(&model, area, x, y), None);
}

fn two_checks(tail: &str) -> ScreenModel {
    let mut model = crate::mock::idle();
    model.closeout = vec![
        CloseoutCheck {
            runs: Vec::new(),
            id: "test".into(),
            kind: "command".into(),
            required: true,
            status: CloseoutMark::Passed,
            exit: Some(0),
            attempt: Some(1),
            tail: "ok".into(),
        },
        CloseoutCheck {
            runs: Vec::new(),
            id: "lint".into(),
            kind: "command".into(),
            required: true,
            status: CloseoutMark::Failed,
            exit: Some(1),
            attempt: Some(1),
            tail: tail.into(),
        },
    ];
    model
}

#[test]
fn a_closeout_list_does_not_sit_above_the_input() {
    let area = Rect::new(0, 0, 76, 24);
    let idle = crate::mock::idle();
    assert_eq!(
        split_of(&idle, area).input.y,
        split_of(&idle, area).session.y + split_of(&idle, area).session.height
    );
    assert!(!draw(&idle).contains("\u{2713} test"));
    let model = two_checks("ok");
    let split = split_of(&model, area);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    assert_eq!(split.todos.width, 0);
    assert_eq!(split.session.x + split.session.width, area.width);
    assert_eq!(split.session.width, split_of(&idle, area).session.width);
    let buffer = draw_buffer(&model);
    assert_eq!(buffer[(area.width - 1, split.session.y)].symbol(), "╮");
    let closed = draw(&model);
    assert!(!closed.contains("\u{2713} test"), "{closed}");
    let mut open = model.clone();
    open.right_open = true;
    open.right_panes.insert(RightPane::Closeout);
    let text = draw(&open);
    assert!(text.contains("\u{2713} test"), "{text}");
    assert!(text.contains("\u{2717} lint"), "{text}");
    let rows = split_of(&open, area);
    assert_eq!(rows.input.y, rows.session.y + rows.session.height);
}

#[test]
fn the_closeout_pane_sits_in_the_right_column() {
    let mut model = two_checks("---\n\u{2514}\u{2500}\u{2500} fail");
    model.right_open = true;
    model.right_width = 40;
    model.right_panes.insert(RightPane::Closeout);
    model.open_check = Some("lint".into());
    let short = Rect::new(0, 0, 76, 24);
    let pane = closeout_pane_rect(&model, short);
    let chat = session_inner_of(&model, short);
    let session = split_of(&model, short).session;
    assert!(pane.height >= 3, "{pane:?}");
    assert!(chat.height >= 1, "{chat:?}");
    assert!(pane.x >= session.x + session.width);
    assert_eq!(pane.y, session.y);
    let text = draw(&model);
    assert!(text.contains("\u{2717} lint  command  failed"), "{text}");
    assert!(text.contains("exit 1"), "{text}");
    assert!(text.contains("---"), "{text}");
    assert!(text.contains("\u{2514}\u{2500}\u{2500} fail"), "{text}");
    assert!(!text.contains("[31m"), "{text}");
}

#[test]
fn required_and_untouched_closeout_checks_have_clear_labels() {
    let mut model = two_checks("");
    model.right_open = true;
    model.right_width = 40;
    model.right_panes.insert(RightPane::Closeout);
    model.closeout[0].status = CloseoutMark::Missing;
    model.closeout[0].exit = None;
    model.closeout[1].status = CloseoutMark::NotRequired;
    model.closeout[1].required = false;
    model.closeout[1].exit = None;
    let text = draw(&model);
    assert!(text.contains("test  command  missing"), "{text}");
    assert!(text.contains("Required for this turn"), "{text}");
    assert_eq!(text.matches("missing").count(), 1, "{text}");
    assert!(text.contains("Not required; won't run"), "{text}");
}

#[test]
fn untouched_closeout_checks_hide_the_pane_and_its_column() {
    let mut model = two_checks("");
    model.right_open = true;
    model.right_panes.clear();
    model.right_panes.insert(RightPane::Closeout);
    for check in &mut model.closeout {
        check.required = false;
        check.status = CloseoutMark::NotRequired;
    }
    assert!(!right_column_visible(&model));
    assert!(stacked_panes(&model).is_empty());
    assert!(!draw(&model).contains("Not required"));
    model.closeout[0].required = true;
    assert!(right_column_visible(&model));
    assert_eq!(stacked_panes(&model), [RightPane::Closeout]);
    assert!(closeout_content_lines(&model, 40)
        .iter()
        .any(|(line, _)| line.to_string() == "Closeout checks are required"));
}

#[test]
fn a_running_check_changes_its_spinner_with_the_tick() {
    let mut model = two_checks("");
    model.right_open = true;
    model.right_panes.insert(RightPane::Closeout);
    model.closeout[0].status = CloseoutMark::Running;
    model.closeout[0].exit = None;
    model.tick = 0;
    let first = draw(&model);
    model.tick = 1;
    let second = draw(&model);
    assert!(first.contains("test"), "{first}");
    assert_ne!(first, second);
    assert!(first.contains('\u{280b}') || second.contains('\u{280b}'));
}

#[test]
fn an_expanded_tail_keeps_ansi_width_and_scrolls_inside_the_pane() {
    let mut lines = vec!["\u{1b}[31mred\u{1b}[0m".to_string(), "---".to_string()];
    for index in 0..40 {
        lines.push(format!("row{index}"));
    }
    let mut model = two_checks(&lines.join("\n"));
    model.right_open = true;
    model.right_width = 40;
    model.right_panes.insert(RightPane::Closeout);
    model.open_check = Some("lint".into());
    let area = Rect::new(0, 0, 76, 24);
    let text = draw(&model);
    assert!(text.contains("red"), "{text}");
    assert!(!text.contains("[31m"), "{text}");
    assert!(text.contains("row0"), "{text}");
    let max = closeout_scroll_max(&model, area);
    assert!(max > 0, "the tail is taller than the pane");
    model.closeout_scroll = max;
    let scrolled = draw(&model);
    assert!(!scrolled.contains("row0"), "{scrolled}");
    assert!(scrolled.contains("row39"), "{scrolled}");
    let (x, y) = hit_path(&model, area, "lint", closeout_row_at);
    assert_eq!(closeout_row_at(&model, area, x, y).as_deref(), Some("lint"));
    let pane = closeout_pane_rect(&model, area);
    assert!(closeout_pane_at(&model, area, pane.x, pane.y));
    assert_eq!(closeout_row_at(&model, area, pane.x, pane.y), None);
    model.open_check = Some("test".into());
    let passed = draw(&model);
    assert!(
        passed.contains("\u{2713} test  command  passed"),
        "{passed}"
    );
    assert!(passed.contains("ok"), "{passed}");
}

#[test]
fn a_proof_overlay_does_not_list_closeout_checks() {
    let mut model = two_checks("---\nfail");
    model.overlay = Some(Overlay::Proof {
        text: "cargo test passed.".into(),
        items: vec![ItemRun {
            id: "lint".into(),
            kind: ItemKind::Command,
            outcome: Outcome::Failed,
            argv: vec!["cargo".into(), "clippy".into()],
            exit: Some(1),
            tail: "---\nfail".into(),
        }],
    });
    let text = draw(&model);
    assert!(text.contains("cargo test passed."), "{text}");
    assert!(!text.contains("\u{2717} lint  command  failed"), "{text}");
    assert!(!text.contains("cargo clippy"), "{text}");
}

fn filled_stack() -> ScreenModel {
    let mut model = two_checks("ok");
    model.todos = crate::mock::todos().todos;
    model.tasks = vec![TaskLine {
        id: "ab12cd34".into(),
        argv: "sleep 30".into(),
        state: "running".into(),
    }];
    model.schedules = vec![ScheduleLine {
        id: "sched-1".into(),
        note: "Check gh comments".into(),
        remaining_min: 10,
    }];
    model.right_open = true;
    model.right_width = TODOS_WIDTH;
    model.right_panes.extend(RightPane::ORDER);
    model.open_task = Some(OpenTask {
        id: "ab12cd34".into(),
        argv: "sleep 30".into(),
        state: "running".into(),
        tail: "still going".into(),
    });
    model
}

#[test]
fn stacked_panes_share_the_right_column() {
    let mut model = filled_stack();
    model.right_panes.remove(&RightPane::Proof);
    model.task_scroll = 1;
    let area = Rect::new(0, 0, 76, 24);
    let column = split_of(&model, area).todos;
    assert!(column.width > RAIL_WIDTH);
    let rects = right_pane_rects(&model, area);
    assert_eq!(
        rects.iter().map(|(pane, _)| *pane).collect::<Vec<_>>(),
        RightPane::ORDER
            .into_iter()
            .filter(|pane| *pane != RightPane::Proof)
            .collect::<Vec<_>>()
    );
    let mut bottom = column.y;
    for (pane, rect) in &rects {
        assert!(rect.height >= 3, "{pane:?} {rect:?}");
        assert_eq!(rect.x, column.x);
        assert_eq!(rect.width, column.width);
        assert_eq!(rect.y, bottom);
        bottom = rect.y + rect.height;
        assert!(pane_title_at(&model, area, rect.x + 2, rect.y));
    }
    assert_eq!(bottom, column.y + column.height);
    let todos = rects[0].1;
    let tasks = rects[2].1;
    assert!(todos.y < tasks.y);
    let text = draw(&model);
    assert!(text.contains(" todos "), "{text}");
    assert!(text.contains(" closeout "), "{text}");
    assert!(text.contains(" tasks "), "{text}");
    assert!(text.contains(" schedules "), "{text}");
    assert!(text.contains("still going"), "{text}");
    assert!(text.contains("In 10 min"), "{text}");
    assert!(text.contains("Check gh"), "{text}");
    assert_eq!(task_row_at(&model, area, tasks.x + 2, tasks.y), None);
}

#[test]
fn a_closed_or_empty_right_column_takes_no_width() {
    let area = Rect::new(0, 0, 76, 24);
    let mut closed = crate::mock::todos();
    closed.right_open = false;
    let split = split_of(&closed, area);
    assert_eq!(split.todos.width, 0);
    assert_eq!(split.session.x + split.session.width, area.width);
    let buffer = draw_buffer(&closed);
    assert_eq!(buffer[(area.width - 1, split.session.y)].symbol(), "╮");

    let mut empty = closed.clone();
    empty.right_open = true;
    empty.right_panes.clear();
    assert_eq!(split_of(&empty, area).todos.width, 0);

    let mut open = closed.clone();
    open.right_open = true;
    open.right_panes.insert(RightPane::Todos);
    let wide = split_of(&open, area);
    assert!(wide.todos.width > 1);
    assert_eq!(split.session.width, wide.session.width + wide.todos.width);
}

#[test]
fn an_empty_session_has_no_right_column_or_splash() {
    let model = crate::mock::empty();
    let area = Rect::new(0, 0, 76, 24);
    let split = split_of(&model, area);
    assert_eq!(split.todos.width, 0);
    assert!(!right_column_visible(&model));
    let backend = ratatui::backend::TestBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let session = session_inner_of(&model, area);
    let mut dog = false;
    for y in session.y..session.y + session.height {
        for x in session.x..session.x + session.width {
            if buffer[(x, y)].symbol().contains('\u{2580}') {
                dog = true;
            }
        }
    }
    assert!(!dog, "the empty session pane stays clear");
}

#[test]
fn input_wraps_long_words_and_keeps_the_cursor() {
    let model = ScreenModel {
        bottom: "x".repeat(40),
        ..ScreenModel::default()
    };
    let area = Rect::new(0, 0, 20, 12);
    let split = split_of(&model, area);
    assert_eq!(split.input.height, 3);
    let lines = input_lines(&model, area.width);
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| line.width() <= 20));
    assert!(lines.last().unwrap().to_string().ends_with("█"));
}
#[test]
fn input_wraps_explicit_newlines_and_wide_unicode() {
    let model = ScreenModel {
        bottom: "界\nend".into(),
        ..ScreenModel::default()
    };
    let lines = input_lines(&model, 8);
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line.width() <= 8));
    assert_eq!(lines.last().unwrap().to_string(), "end█");
}
#[test]
fn a_tall_input_keeps_its_tail_visible_and_leaves_session_room() {
    let model = ScreenModel {
        bottom: "x".repeat(300),
        ..ScreenModel::default()
    };
    let area = Rect::new(0, 0, 20, 10);
    let split = split_of(&model, area);
    assert!(split.session.height >= 3);
    let backend = ratatui::backend::TestBackend::new(20, 10);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(&model, area, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!((0..20).any(|x| buffer[(x, 9)].symbol() == "█"));
}

#[test]
fn tool_failure_is_visible_during_model_thinking() {
    let mut model = crate::mock::working();
    model.phase = Some(Phase::Thinking);
    model.action = Some("attach_artifact failed (2 attempts): missing.jpg".into());
    let text = draw(&model);
    assert!(
        text.contains("attach_artifact failed (2 attempts)"),
        "{text}"
    );
    assert!(!model.thinking.contains("missing.jpg"));
}

#[test]
fn provider_retry_status_is_a_footer_not_reasoning() {
    let mut model = crate::mock::working();
    model.retry_status = Some("Provider busy. Retrying in 10s".into());
    let text = draw(&model);
    assert!(text.contains("Provider busy. Retrying in 10s"), "{text}");
    assert!(!model.thinking.contains("Provider busy"));
}

#[test]
fn markdown_file_links_and_selection_match_rendered_lines() {
    let path = "docs/README.md";
    let text = "# Guide\n\nOpen [docs](https://example.com/docs).";
    let overlay = Overlay::File {
        path: path.into(),
        text: text.into(),
        truncated: false,
    };
    let lines = overlay_lines(&overlay, 40);
    let row = lines
        .iter()
        .position(|line| line.to_string().contains("Open docs"))
        .expect("rendered label");
    let column = lines[row].to_string().find("docs").unwrap();
    assert_eq!(
        overlay_link_at(&overlay, 40, row, column),
        Some("https://example.com/docs".into())
    );
    assert_eq!(overlay_link_at(&overlay, 40, 0, 0), None);
}

#[test]
fn formatted_file_selection_copies_visible_code_and_markdown() {
    for (path, text, needle) in [
        ("src/main.rs", "    let value = 1;", "    let value = 1;"),
        ("README.md", "**bold** text", "bold text"),
    ] {
        let model = ScreenModel {
            overlay: Some(Overlay::File {
                path: path.into(),
                text: text.into(),
                truncated: false,
            }),
            ..ScreenModel::default()
        };
        let area = Rect::new(0, 0, 100, 30);
        let width = overlay_inner_width(split_of(&model, area).session);
        let lines = overlay_lines(model.overlay.as_ref().unwrap(), width);
        let row = lines
            .iter()
            .position(|line| line.to_string() == needle)
            .expect("visible file text");
        let anchor = SelectPoint {
            line: row,
            column: 0,
            place: SelectPlace::Overlay,
        };
        let end = SelectPoint {
            column: needle.len() - 1,
            ..anchor
        };
        let sel = TextSelect {
            anchor,
            end,
            held: false,
            x: 0,
            y: 0,
        };
        assert_eq!(selection_text(&model, area, sel), needle);
    }
}

#[test]
fn scrolled_file_links_and_selection_use_visible_row_offsets() {
    let text = format!(
        "{}\n[visible](https://example.com/scrolled)",
        (0..30).map(|i| format!("row {i}\n\n")).collect::<String>()
    );
    let mut model = ScreenModel {
        overlay: Some(Overlay::File {
            path: "README.md".into(),
            text,
            truncated: false,
        }),
        ..ScreenModel::default()
    };
    let area = Rect::new(0, 0, 100, 24);
    model.file_scroll = file_scroll_max(&model, area);
    let pane = split_of(&model, area).session;
    let lines = overlay_lines(model.overlay.as_ref().unwrap(), overlay_inner_width(pane));
    let rect = overlay_rect(pane, lines.len());
    let inner = Block::bordered().inner(rect);
    let row = lines
        .iter()
        .position(|line| line.to_string().contains("visible"))
        .unwrap();
    let y = inner.y + (row - model.file_scroll) as u16;
    let backend = ratatui::backend::TestBackend::new(100, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(&model, area, frame)).unwrap();
    assert_eq!(terminal.backend().buffer()[(inner.x, y)].symbol(), "v");
    assert_eq!(
        link_at(&model, area, inner.x, y),
        Some("https://example.com/scrolled".into())
    );
    let anchor = selection_anchor(&model, area, inner.x, y).unwrap();
    assert_eq!(anchor.line, row);
    let end = SelectPoint {
        column: 6,
        ..anchor
    };
    let sel = TextSelect {
        anchor,
        end,
        held: false,
        x: inner.x,
        y,
    };
    assert_eq!(selection_text(&model, area, sel), "visible");
}

#[test]
fn terminal_light_and_dark_palettes_keep_input_and_selected_rows_readable() {
    fn luminance(color: Color) -> f64 {
        let (r, g, b) = match color {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Gray => (192, 192, 192),
            _ => panic!("expected known text color"),
        };
        let linear = |v: u8| {
            let c = f64::from(v) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.072 * linear(b)
    }
    for mode in [theme::Appearance::Dark, theme::Appearance::Light] {
        theme::with_appearance(mode, || {
            let mut model = crate::mock::idle();
            model.bottom = "Readable draft".into();
            let area = Rect::new(0, 0, 76, 24);
            let split = split_of(&model, area);
            let buffer = draw_buffer(&model);
            let cell = (0..76)
                .map(|x| &buffer[(x, split.input.y + 1)])
                .find(|cell| cell.symbol() == "R")
                .unwrap();
            assert_eq!(cell.fg, theme::text());
            assert_eq!(cell.bg, theme::selected_bg());
            let a = luminance(cell.fg);
            let b = luminance(cell.bg);
            assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5);
            assert!((0..split.list.height).any(|y| (0..split.list.width)
                .any(|x| buffer[(x, y + split.list.y)].bg == theme::selected_bg())));
            assert_eq!(theme::selected_row().bg, Some(theme::selected_bg()));
        });
    }
}
#[test]
fn syntax_highlighting_follows_terminal_appearance() {
    let dark = theme::with_appearance(theme::Appearance::Dark, || {
        file::file_lines("main.rs", "fn main() {}", 80)
    });
    let light = theme::with_appearance(theme::Appearance::Light, || {
        file::file_lines("main.rs", "fn main() {}", 80)
    });
    assert_eq!(
        dark.iter().map(Line::to_string).collect::<String>(),
        light.iter().map(Line::to_string).collect::<String>()
    );
    assert_ne!(dark, light);
}

#[test]
fn large_result_and_question_previews_preserve_content_and_choices() {
    let text = (0..80).map(|i| format!("line {i}\n")).collect::<String>();
    let result = Card::result(&text);
    assert!(result.lines(60).len() <= 10);
    assert_eq!(result.preview_text(), Some(text.as_str()));
    let question = Card::question(&text, &[("continue", false)]);
    let rendered = question
        .lines(60)
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Show complete text"));
    assert!(rendered.contains("continue"));
    assert!(!rendered.contains("line 79"));
    let mut model = crate::mock::idle();
    model.overlay = Some(Overlay::Text { text: text.clone() });
    let area = Rect::new(0, 0, 76, 24);
    model.file_scroll = file_scroll_max(&model, area);
    let rendered = grid(&model)
        .into_iter()
        .map(|row| row.concat())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("line 79"), "{rendered}");
}

#[test]
fn idle_panes_have_no_image_and_working_labels_keep_their_row() {
    for width in [40, 76, 120] {
        let mut model = crate::mock::empty();
        let backend = ratatui::backend::TestBackend::new(width, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(&model, frame.area(), frame))
            .unwrap();
        let pane = session_inner_of(&model, Rect::new(0, 0, width, 24));
        for y in pane.y..pane.y + pane.height {
            for x in pane.x..pane.x + pane.width {
                assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), " ");
            }
        }
        model = crate::mock::working();
        model.phase = Some(Phase::Tool);
        for action in ["Searching", "Reading", "Editing", "Running", "Verifying"] {
            model.action = Some(action.into());
            let pane = session_inner_of(&model, Rect::new(0, 0, width, 24));
            for (tick, glyph) in FLUX_SPINNER.iter().enumerate() {
                model.tick = tick;
                terminal
                    .draw(|frame| render(&model, frame.area(), frame))
                    .unwrap();
                let row = pane.y + pane.height - 1;
                let line: String = (pane.x..pane.x + pane.width)
                    .map(|x| terminal.backend().buffer()[(x, row)].symbol())
                    .collect();
                assert!(line.starts_with(glyph), "{line}");
                assert!(line.contains(action), "{line}");
            }
        }
    }
}

#[test]
fn compaction_overrides_stale_thinking_and_retry_status() {
    let mut model = crate::mock::working();
    model.compacting = true;
    model.phase = Some(Phase::Thinking);
    model.retry_status = Some("Provider busy. Retrying in 10s".into());
    model.sessions[0].compacting = true;
    let text = draw(&model);
    assert!(text.contains("Compacting context"), "{text}");
    assert!(!text.contains("Provider busy"), "{text}");
    assert_eq!(model.sessions[0].status_text(), "compacting");
    let area = Rect::new(0, 0, 100, 30);
    assert!(!(0..30).any(|y| (0..100).any(|x| thinking_at(&model, area, x, y))));
    model.compacting = false;
    model.sessions[0].compacting = false;
    assert!(!draw(&model).contains("Compacting context"));
}
