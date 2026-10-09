use crate::{attachment::ImageAttachment, tools::Tools};
use resvg::{tiny_skia, usvg};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionVisual {
    pub title: String,
    pub alt: String,
    pub image: ImageAttachment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

pub fn prepare(tools: &Tools, turn: &str, args: &Value) -> Result<Vec<QuestionVisual>, String> {
    let Some(value) = args.get("visuals") else {
        return Ok(Vec::new());
    };
    let visuals = value.as_array().ok_or("visuals must be an array")?;
    if visuals.len() > 2 {
        return Err("Use at most two visuals per question.".into());
    }
    visuals
        .iter()
        .map(|value| prepare_one(tools, turn, value))
        .collect()
}

fn prepare_one(tools: &Tools, turn: &str, value: &Value) -> Result<QuestionVisual, String> {
    let title = bounded_text(value, "title", 80)?;
    let alt = bounded_text(value, "alt", 600)?;
    let (image, source) = match (value.get("mermaid"), value.get("path")) {
        (Some(source), None) => {
            let source = source.as_str().ok_or("mermaid must be a string")?;
            let svg = mermaid_svg(source)?;
            (svg_image(&svg)?, Some(source.to_string()))
        }
        (None, Some(path)) => {
            let path = path.as_str().ok_or("path must be a string")?;
            let read = tools
                .read_file(turn, path, None, None, None)
                .map_err(|e| e.to_string())?;
            if read.denied {
                return Err(
                    "Visual read was denied. Ask without it or choose an allowed file.".into(),
                );
            }
            if let Some(image) = read.images.first() {
                if read.images.len() != 1 {
                    return Err("Choose one image, SVG, or Mermaid file per visual.".into());
                }
                (image.clone(), None)
            } else {
                if read.next_line.is_some() || read.next_offset.is_some() {
                    return Err(
                        "Diagram source is too large. Use a smaller focused diagram.".into(),
                    );
                }
                let source = read
                    .text
                    .lines()
                    .map(|line| line.split_once('→').map_or(line, |(_, text)| text))
                    .collect::<Vec<_>>()
                    .join("\n");
                let svg = if source.trim_start().starts_with('<') {
                    source.clone()
                } else {
                    mermaid_svg(&source)?
                };
                (svg_image(&svg)?, Some(source))
            }
        }
        _ => return Err("Each visual needs exactly one of mermaid or path.".into()),
    };
    let image = ImageAttachment {
        name: title.clone(),
        ..image
    };
    Ok(QuestionVisual {
        title,
        alt,
        image,
        source,
    })
}

fn bounded_text(value: &Value, key: &str, limit: usize) -> Result<String, String> {
    let text = value.get(key).and_then(Value::as_str).unwrap_or("").trim();
    if text.is_empty()
        || text.chars().count() > limit
        || text.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(format!("{key} must contain 1–{limit} readable characters."));
    }
    Ok(text.to_string())
}

fn mermaid_svg(source: &str) -> Result<String, String> {
    if source.len() > 8192 {
        return Err("Mermaid source must be at most 8 KiB.".into());
    }
    let parsed = mermaid_rs_renderer::parse_mermaid_strict(source).map_err(|e| e.to_string())?;
    if parsed.graph.nodes.len() > 40
        || parsed.graph.edges.len() > 80
        || parsed.graph.subgraphs.len() > 10
    {
        return Err("Use a focused diagram with at most 40 nodes, 80 edges, and 10 groups.".into());
    }
    let theme = mermaid_rs_renderer::Theme::modern();
    let config = mermaid_rs_renderer::LayoutConfig::default();
    let layout = mermaid_rs_renderer::compute_layout(&parsed.graph, &theme, &config);
    Ok(mermaid_rs_renderer::render_svg(&layout, &theme, &config))
}

fn svg_image(source: &str) -> Result<ImageAttachment, String> {
    if source.len() > 256 * 1024 {
        return Err("SVG source must be at most 256 KiB.".into());
    }
    let mut options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(source, &options).map_err(|e| format!("Invalid SVG: {e}"))?;
    let size = tree.size();
    let scale = (2048.0 / size.width().max(size.height())).min(2.0);
    let width = (size.width() * scale).ceil().max(1.0) as u32;
    let height = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixels =
        tiny_skia::Pixmap::new(width, height).ok_or("Cannot allocate diagram preview")?;
    pixels.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    let png = pixels.encode_png().map_err(|e| e.to_string())?;
    ImageAttachment::from_bytes("diagram.png", &png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mermaid_flow_and_sequence_render_to_valid_images() {
        for source in [
            "flowchart LR\n A[Implement] --> B[Review]\n B --> C[PR]",
            "sequenceDiagram\n User->>Agent: Plan\n Agent-->>User: Decide",
        ] {
            let svg = mermaid_svg(source).unwrap();
            assert!(svg.contains("<svg"));
            svg_image(&svg).unwrap().validate().unwrap();
        }
    }

    #[test]
    fn svg_scripts_and_external_images_do_not_reach_the_preview() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><script>alert(1)</script><image href="/etc/passwd" width="20" height="20"/><image href="https://example.invalid/tracker" width="20" height="20"/><rect width="10" height="10" fill="red"/></svg>"#;
        let image = svg_image(svg).unwrap();
        image.validate().unwrap();
        assert_eq!(image.mime_type, "image/png");
        assert!(!image.data.contains("passwd"));
    }

    #[test]
    fn malformed_and_excessive_diagrams_fail() {
        assert!(mermaid_svg("not a diagram").is_err());
        assert!(mermaid_svg(&"x".repeat(8193)).is_err());
        assert!(svg_image("<svg>").is_err());
        assert!(bounded_text(&serde_json::json!({"alt":""}), "alt", 600).is_err());
    }
}
