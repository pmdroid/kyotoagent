use super::*;
use crate::attachment::{ImageAttachment, MAX_IMAGES, MAX_IMAGE_BYTES};
use std::io::Read;

pub(super) fn pending(app: &App) -> &[ImageAttachment] {
    app.images
        .get(&app.selected)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub(super) fn attach_drop(app: &mut App, text: &str) -> bool {
    let Some(paths) = dropped_paths(text) else {
        return false;
    };
    let result = (|| {
        if pending(app).len() + paths.len() > MAX_IMAGES {
            return Err("Attach at most four images per message.".to_string());
        }
        let mut images = Vec::new();
        for path in paths {
            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if meta.len() > MAX_IMAGE_BYTES as u64 {
                return Err("Images must be 5 MiB or smaller.".to_string());
            }
            if !meta.is_file() {
                return Err("Choose an image file.".to_string());
            }
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .take(MAX_IMAGE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("Image name is invalid.")?;
            images.push(ImageAttachment::from_bytes(name, &bytes)?);
        }
        Ok(images)
    })();
    match result {
        Ok(images) => {
            app.images
                .entry(app.selected.clone())
                .or_default()
                .extend(images);
            app.notice = None;
        }
        Err(error) => app.notice = Some(error),
    }
    true
}

fn dropped_paths(text: &str) -> Option<Vec<PathBuf>> {
    let text = text.trim();
    if let Ok(url) = reqwest::Url::parse(text) {
        if let Ok(path) = url.to_file_path() {
            return dropped_paths(&format!("\"{}\"", path.display()));
        }
    }
    if Path::new(text).is_absolute()
        && Path::new(text).is_file()
        && Path::new(text)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp"
                )
            })
    {
        return Some(vec![PathBuf::from(text)]);
    }
    let mut paths = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in text.trim().chars() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && (ch == '\'' || ch == '"') {
            quote = Some(ch);
        } else if quote.is_none() && ch.is_whitespace() {
            if !word.is_empty() {
                paths.push(PathBuf::from(std::mem::take(&mut word)));
            }
        } else {
            word.push(ch);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        paths.push(PathBuf::from(word));
    }
    if paths.is_empty()
        || paths.iter().any(|path| {
            !path.is_absolute()
                || !path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(
                        e.to_ascii_lowercase().as_str(),
                        "png" | "jpg" | "jpeg" | "webp"
                    )
                })
        })
    {
        return None;
    }
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_image_paths_preserve_spaces_and_reject_prose() {
        assert_eq!(
            dropped_paths("'/tmp/a b.png' /tmp/c\\ d.webp").unwrap(),
            vec![
                PathBuf::from("/tmp/a b.png"),
                PathBuf::from("/tmp/c d.webp")
            ]
        );
        assert!(dropped_paths("look at /tmp/a.png").is_none());
        assert!(dropped_paths("/tmp/a.txt").is_none());
    }
}

#[cfg(test)]
mod behavior_tests {
    use super::*;

    #[tokio::test]
    async fn dropping_images_keeps_text_and_session_drafts_and_removes_by_click() {
        let path = std::env::temp_dir().join(format!("kyoto-image-{}.png", std::process::id()));
        std::fs::write(&path, crate::splash::PNG).unwrap();
        let mut app = App::new(PathBuf::new(), PathBuf::new(), String::new());
        app.selected = "first".into();
        app.ask = "Describe this".into();
        assert!(attach_drop(&mut app, path.to_str().unwrap()));
        assert_eq!(pending(&app).len(), 1);
        assert_eq!(app.ask, "Describe this");
        app.selected = "second".into();
        assert!(pending(&app).is_empty());
        app.selected = "first".into();
        let client = Client::at(PathBuf::from("/missing.sock"));
        apply(&mut app, &client, Effect::RemoveImage(0))
            .await
            .unwrap();
        assert!(pending(&app).is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
