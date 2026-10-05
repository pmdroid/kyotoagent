use super::*;
use crate::attachment::ImageAttachment;

const MAX_DOCUMENT_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct ReadOptions {
    pub offset: Option<u64>,
    pub line: Option<u64>,
    pub limit: Option<u64>,
    pub pages: Option<String>,
    pub format: Option<String>,
}

fn invalid(path: &Path, message: impl Into<String>) -> ToolError {
    ToolError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()),
    }
}

fn binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
        || bytes
            .iter()
            .filter(|&&byte| byte < 9 || (14..=31).contains(&byte))
            .count()
            * 10
            > bytes.len() * 3
}

pub(super) fn decode_text(
    bytes: &[u8],
    more: bool,
    path: &Path,
) -> Result<(String, usize), ToolError> {
    if binary(bytes) {
        return Err(invalid(path, "Cannot read binary file as text."));
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok((text.to_string(), bytes.len())),
        Err(error) if more && error.error_len().is_none() && error.valid_up_to() > 0 => {
            let count = error.valid_up_to();
            Ok((
                std::str::from_utf8(&bytes[..count]).unwrap().to_string(),
                count,
            ))
        }
        Err(_) => Err(invalid(
            path,
            "Cannot read binary or non-UTF-8 file as text.",
        )),
    }
}

pub(super) fn read_document(
    file: &mut File,
    path: &Path,
    options: &ReadOptions,
) -> Result<Option<ReadFile>, ToolError> {
    let mut header = [0; 8192];
    let count = fill(file, &mut header).map_err(|source| ToolError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let header = &header[..count];
    let mime = infer::get(header).map(|kind| kind.mime_type());
    let is_pdf = header.starts_with(b"%PDF-");
    let is_image = mime.is_some_and(|mime| mime.starts_with("image/"));
    if !is_pdf && !is_image {
        if mime.is_some_and(|mime| {
            !mime.starts_with("text/")
                && !matches!(
                    mime,
                    "application/xml"
                        | "application/json"
                        | "application/rtf"
                        | "application/postscript"
                )
        }) {
            return Err(invalid(path, "Cannot read binary file as text."));
        }
        decode_text(header, count == 8192, path)?;
        if options.pages.is_some() || options.format.is_some() {
            return Err(invalid(path, "pages and format require a PDF file."));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|source| ToolError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        return Ok(None);
    }
    if options.offset.is_some() || options.line.is_some() || options.limit.is_some() {
        return Err(invalid(
            path,
            "offset, line and limit require a text file. Use pages for PDFs.",
        ));
    }
    if is_image
        && (options.pages.is_some()
            || options
                .format
                .as_deref()
                .is_some_and(|format| format != "image"))
    {
        return Err(invalid(path, "pages and format require a PDF file."));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid(path, "Document exceeds the 50 MiB read limit."));
    }
    let mut result = ReadFile {
        path: display(path),
        text: String::new(),
        next_offset: None,
        next_line: None,
        denied: false,
        images: Vec::new(),
    };
    if is_image {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        result.images.push(
            ImageAttachment::from_file_bytes(&name, &bytes)
                .map_err(|error| invalid(path, error))?,
        );
        result.text = format!("Read image {}.", path.display());
    } else {
        read_pdf(bytes, path, options, &mut result)?;
    }
    Ok(Some(result))
}

fn pdf_pages(spec: Option<&str>, count: usize) -> Result<Vec<usize>, String> {
    if count == 0 {
        return Err("PDF has no pages.".into());
    }
    let Some(spec) = spec else {
        return if count <= 10 {
            Ok((0..count).collect())
        } else {
            Err("PDF has more than 10 pages. Specify pages, with at most 20 pages per read.".into())
        };
    };
    let (start, end) = match spec.split_once('-') {
        Some((start, end)) => (
            start
                .parse::<usize>()
                .map_err(|_| "Invalid PDF page range.")?,
            if end.is_empty() {
                count
            } else {
                end.parse::<usize>()
                    .map_err(|_| "Invalid PDF page range.")?
            },
        ),
        None => {
            let page = spec
                .parse::<usize>()
                .map_err(|_| "Invalid PDF page range.")?;
            (page, page)
        }
    };
    if start == 0 || start > end || end > count || end - start >= 20 {
        return Err(
            "Invalid PDF page range. Use 1-based pages, at most 20 per read, within the document."
                .into(),
        );
    }
    Ok((start - 1..end).collect())
}

fn read_pdf(
    bytes: Vec<u8>,
    path: &Path,
    options: &ReadOptions,
    result: &mut ReadFile,
) -> Result<(), ToolError> {
    let text = match options.format.as_deref() {
        None | Some("image") => false,
        Some("text") => true,
        Some(_) => return Err(invalid(path, "PDF format must be image or text.")),
    };
    let doc = pdf_oxide::PdfDocument::from_bytes(bytes)
        .map_err(|error| invalid(path, error.to_string()))?;
    let count = doc
        .page_count()
        .map_err(|error| invalid(path, error.to_string()))?;
    let pages = pdf_pages(options.pages.as_deref(), count).map_err(|error| invalid(path, error))?;
    for page in pages {
        result
            .text
            .push_str(&format!("Page {} of {count}\n", page + 1));
        if text {
            result.text.push_str(
                &doc.extract_text(page)
                    .map_err(|error| invalid(path, error.to_string()))?,
            );
            result.text.push('\n');
            if result.text.len() > READ_LIMIT {
                return Err(invalid(
                    path,
                    "Extracted PDF text exceeds the read limit. Read fewer pages.",
                ));
            }
        } else {
            let (x0, y0, x1, y1) = doc
                .get_page_visible_box(page)
                .map_err(|error| invalid(path, error.to_string()))?;
            let width = (x1 - x0).abs();
            let height = (y1 - y0).abs();
            if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                return Err(invalid(
                    path,
                    "PDF page dimensions exceed the rendering limit.",
                ));
            }
            let render = pdf_oxide::rendering::RenderOptions::with_dpi(72).as_jpeg(75);
            let image = pdf_oxide::rendering::render_page_fit(&doc, page, 1024, 1024, &render)
                .map_err(|error| invalid(path, error.to_string()))?;
            result.images.push(
                ImageAttachment::from_file_bytes(
                    &format!(
                        "{}-page-{}.jpg",
                        take_bytes(&path.file_name().unwrap_or_default().to_string_lossy(), 200),
                        page + 1
                    ),
                    &image.data,
                )
                .map_err(|error| invalid(path, error))?,
            );
        }
    }
    Ok(())
}
