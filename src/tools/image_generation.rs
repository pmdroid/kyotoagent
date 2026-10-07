use super::*;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

const IMAGE_LIMIT: usize = 32 * 1024 * 1024;
const RESPONSE_LIMIT: usize = IMAGE_LIMIT * 2;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGeneration {
    pub prompt: String,
    pub model: String,
    pub path: String,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub response_format: Option<String>,
}

fn image_error(message: impl std::fmt::Display) -> ToolError {
    ToolError::Web(format!("generate_image: {message}"))
}

fn bounded_response(
    response: reqwest::blocking::Response,
    limit: usize,
) -> Result<Vec<u8>, ToolError> {
    if !response.status().is_success() {
        return Err(image_error(format!("HTTP {}", response.status())));
    }
    let mut bytes = Vec::new();
    response
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| image_error("could not read image response"))?;
    if bytes.len() > limit {
        return Err(image_error(format!("response exceeds {limit} bytes")));
    }
    Ok(bytes)
}

impl ImageGeneration {
    fn body(&self) -> Result<Value, ToolError> {
        if self.prompt.trim().is_empty()
            || self.model.trim().is_empty()
            || self.path.trim().is_empty()
        {
            return Err(image_error("prompt, model and path must not be empty"));
        }
        if self
            .response_format
            .as_deref()
            .is_some_and(|format| !matches!(format, "b64_json" | "url"))
        {
            return Err(image_error("response_format must be b64_json or url"));
        }
        let mut body = json!({"prompt": self.prompt, "model": self.model, "n": 1});
        for (key, value) in [
            ("size", &self.size),
            ("quality", &self.quality),
            ("response_format", &self.response_format),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() {
                    return Err(image_error(format!("{key} must not be empty")));
                }
                body[key] = json!(value);
            }
        }
        Ok(body)
    }
}

impl Tools {
    pub fn generate_image(
        &self,
        turn_id: &str,
        config: &crate::config::Config,
        request: ImageGeneration,
    ) -> Result<WriteFile, ToolError> {
        let body = request.body()?;
        if config.is_codex() {
            return Err(image_error(
                "select an OpenAI-compatible API-key provider for image generation",
            ));
        }
        let target = self.target(&request.path)?;
        let parent = write_parent(&target.absolute)?;
        write_bytes(&parent, &target.absolute)?;
        let url = crate::web::parse(&format!(
            "{}/images/generations",
            config.base_url.trim_end_matches('/')
        ))?;
        let key = config.api_key();
        if config.api_key_env.is_some() && key.is_none() {
            return Err(image_error("the active provider API key is missing"));
        }
        let verdict = self.ask(
            turn_id,
            &PermissionBody {
                action: format!("Generate image with {} at {}", request.model, url),
                path: Some(display(&target.absolute)),
                ..PermissionBody::default()
            },
        )?;
        if !verdict.allowed() {
            return Ok(WriteFile {
                path: display(&target.absolute),
                bytes: 0,
                created: false,
                replaced: false,
                denied: true,
            });
        }
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .map_err(|_| image_error("could not create HTTP client"))?;
        let mut post = client.post(url).json(&body);
        if let Some(key) = key {
            post = post.bearer_auth(key);
        }
        let response = post
            .send()
            .map_err(|_| image_error("image generation request failed"))?;
        let response: Value = serde_json::from_slice(&bounded_response(response, RESPONSE_LIMIT)?)
            .map_err(|_| image_error("invalid image generation JSON response"))?;
        let data = response["data"]
            .as_array()
            .filter(|data| data.len() == 1)
            .ok_or_else(|| image_error("expected exactly one generated image"))?;
        let bytes = if let Some(encoded) = data[0]["b64_json"].as_str() {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| image_error("invalid base64 image"))?
        } else if let Some(raw) = data[0]["url"].as_str() {
            let url = crate::web::parse(raw)?;
            crate::web::ensure_public(&url, &crate::web::production())?;
            let response = client
                .get(url)
                .send()
                .map_err(|_| image_error("image download failed"))?;
            bounded_response(response, IMAGE_LIMIT)?
        } else {
            return Err(image_error("image response has neither b64_json nor url"));
        };
        if bytes.len() > IMAGE_LIMIT {
            return Err(image_error("generated image exceeds 32 MiB"));
        }
        image::guess_format(&bytes)
            .map_err(|_| image_error("response is not a supported image"))?;
        self.write_binary(turn_id, &request.path, &bytes, IMAGE_LIMIT)
    }
}
