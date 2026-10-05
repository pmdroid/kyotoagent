use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProofAction {
    Version(u64),
    Response(u64),
    Details(u64),
    File(u64, String),
    Transcript(String),
}

pub fn proof_action_at(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<ProofAction> {
    let (index, width) = pane_line_index(model, area, RightPane::Proof, column, row)?;
    entries(model, width).get(index)?.1.clone()
}

pub(super) fn entries(
    model: &ScreenModel,
    width: usize,
) -> Vec<(Line<'static>, Option<ProofAction>)> {
    let mut entries = Vec::new();
    let mut append = |text: String, action: Option<ProofAction>, style: Style| {
        for line in wrap(&text, width.max(1)) {
            entries.push((Line::from(Span::styled(line, style)), action.clone()));
        }
    };
    if let Some(status) = &model.proof_status {
        append(status.clone(), None, theme::faint());
    }
    for version in model.proof_versions.iter().rev() {
        let count = version.proof.files.len();
        let status = format!("{count} file{}", if count == 1 { "" } else { "s" });
        append(
            format!("v{} · {status}", version.version),
            None,
            theme::title(),
        );
        append(
            format!("{} · {}", version.at, version.turn_id),
            None,
            theme::faint(),
        );
        if version.response.is_some() {
            append(
                "Show response in chat".into(),
                Some(ProofAction::Response(version.version)),
                theme::body(),
            );
        }
        append(
            "File details".into(),
            Some(ProofAction::Details(version.version)),
            theme::body(),
        );
        for file in &version.proof.files {
            let action = Some(ProofAction::File(version.version, file.id.clone()));
            append(file.name.clone(), action.clone(), theme::body());
            append(
                format!("{} · {} bytes", file.media_type, file.size),
                action,
                theme::faint(),
            );
        }
        if version.proof.files.is_empty() {
            append("No attached files".into(), None, theme::faint());
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_does_not_claim_passed_checks_and_empty_proof_can_be_saved_in_layout() {
        let mut model = ScreenModel {
            right_open: true,
            right_panes: BTreeSet::from([RightPane::Proof]),
            ..Default::default()
        };
        assert_eq!(stacked_panes(&model), vec![RightPane::Proof]);
        assert!(entries(&model, 38).is_empty());
        let layout = crate::config::Layout {
            left_open: true,
            right_open: true,
            left_width: 25,
            right_width: 40,
            right_panes: model.right_panes.clone(),
        };
        let saved = serde_json::to_string(&layout).unwrap();
        assert_eq!(
            serde_json::from_str::<crate::config::Layout>(&saved).unwrap(),
            layout
        );
        model.proof_versions = serde_json::from_value(serde_json::json!([{
            "version":1,"eventId":"proof-1","turnId":"turn-1","at":"2026-10-03T00:00:00Z", "response":{"eventId":"result-1","text":"Answer"},"proof":{"text":"All checks passed", "files":[{"id":"0123456789abcdef0123456789abcdef","name":"report.md","mediaType":"text/markdown","size":123,"sha256":"hash"}]}
        }])).unwrap();
        let mut earlier = model.proof_versions[0].clone();
        earlier.version = 2;
        earlier.proof.files[0].name = "cat.png".into();
        earlier.proof.files[0].id = "abcdef0123456789abcdef0123456789".into();
        model.proof_versions.push(earlier);
        model.proof_selected = Some(2);
        let rows = entries(&model, 38);
        let text = rows
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("1 file"), "{text}");
        assert!(text.contains("report.md"), "{text}");
        assert!(text.contains("cat.png"), "{text}");
        assert!(rows
            .iter()
            .any(|(_, action)| matches!(action, Some(ProofAction::File(2, _)))));
        assert!(rows
            .iter()
            .any(|(_, action)| matches!(action, Some(ProofAction::Response(1)))));
        assert!(rows
            .iter()
            .any(|(_, action)| matches!(action, Some(ProofAction::File(1, _)))));
    }
}
