use super::*;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

const IMAGE_LIMIT: usize = 32 * 1024 * 1024;
const RESPONSE_LIMIT: usize = IMAGE_LIMIT * 2;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGeneration {
    pub provider: Option<String>,
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
    fn provider_config(
        &self,
        config: &crate::config::Config,
    ) -> Result<crate::config::Config, ToolError> {
        let provider = self.provider.as_deref().or_else(|| {
            if self.model.starts_with("grok-imagine-") {
                Some(crate::auth::GROK_PROVIDER)
            } else if self.model.starts_with("gpt-image-") && config.providers.contains_key("codex")
            {
                Some("codex")
            } else {
                None
            }
        });
        match provider {
            Some(provider) => config.for_provider(provider).map_err(image_error),
            None => Ok(config.clone()),
        }
    }

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
        self.generate_image_in_root(turn_id, config, request, None)
    }

    pub(crate) fn generate_image_in_root(
        &self,
        turn_id: &str,
        config: &crate::config::Config,
        request: ImageGeneration,
        root: Option<&Path>,
    ) -> Result<WriteFile, ToolError> {
        let body = request.body()?;
        let config = request.provider_config(config)?;
        let target = self.target(&request.path)?;
        let parent = write_parent(&target.absolute)?;
        write_bytes(&parent, &target.absolute)?;
        let url = crate::web::parse(&format!(
            "{}/images/generations",
            config.base_url.trim_end_matches('/')
        ))?;
        let codex_path = config
            .is_codex()
            .then(|| {
                root.map(|root| root.join(crate::auth::CODEX_AUTH_FILE))
                    .or_else(crate::auth::default_codex_path)
                    .ok_or_else(|| image_error("Codex login is missing"))
            })
            .transpose()?;
        let key = if let Some(path) = &codex_path {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| image_error("could not load Codex authentication"))?;
            Some(
                runtime
                    .block_on(async {
                        crate::auth::codex_access(
                            &crate::auth::CodexAuth::at(crate::auth::CODEX_ISSUER),
                            path,
                        )
                        .await
                    })
                    .map_err(|_| image_error("Codex login needs renewal"))?,
            )
        } else {
            crate::auth::stored_provider_key(&config, root)
                .map_err(|_| image_error("could not load image provider credentials"))?
        };
        let key = if key.is_none() && config.provider.as_deref() == Some(crate::auth::GROK_PROVIDER)
        {
            let path = root
                .map(|root| root.join(crate::auth::AUTH_FILE))
                .or_else(crate::auth::default_path)
                .ok_or_else(|| image_error("Grok login is missing"))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| image_error("could not load Grok authentication"))?;
            Some(
                runtime
                    .block_on(async {
                        let client = crate::auth::AuthClient::new(
                            config
                                .grok_client_id
                                .as_deref()
                                .unwrap_or(crate::auth::DEFAULT_CLIENT_ID),
                        );
                        crate::auth::access_token(&client, &path).await
                    })
                    .map_err(|_| image_error("Grok login needs renewal"))?,
            )
        } else {
            key
        };
        if config.api_key_env.is_some() && key.is_none() {
            return Err(image_error("the image provider API key is missing"));
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
        if let Some(path) = &codex_path {
            let tokens = crate::auth::load_codex(path)
                .map_err(|_| image_error("Codex login needs renewal"))?;
            post = post
                .header("originator", crate::auth::CODEX_ORIGINATOR)
                .header("version", crate::auth::CODEX_CLIENT_VERSION)
                .header("x-codex-image-turn-id", turn_id);
            if !tokens.account_id.is_empty() {
                post = post.header("chatgpt-account-id", tokens.account_id);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_provider_override_wins_over_model_routing() {
        let config = crate::config::Config::from_toml(
            "provider = \"codex\"\n[providers.codex]\nkind = \"codex\"\nmodel = \"coding\"\n[providers.grok]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"grok-chat\"\n[providers.proxy]\nbase_url = \"http://127.0.0.1:2/v1\"\nmodel = \"proxy-chat\"\n",
        ).unwrap();
        for model in [
            "grok-imagine-image",
            "grok-imagine-image-pro",
            "grok-imagine-image-2.0",
        ] {
            let mut request: ImageGeneration = serde_json::from_value(
                json!({"model": model, "prompt": "garden", "path": "garden.png"}),
            )
            .unwrap();
            assert_eq!(
                request
                    .provider_config(&config)
                    .unwrap()
                    .provider
                    .as_deref(),
                Some("grok")
            );
            request.provider = Some("proxy".into());
            assert_eq!(
                request
                    .provider_config(&config)
                    .unwrap()
                    .provider
                    .as_deref(),
                Some("proxy")
            );
        }
        assert!(config.is_codex());
    }

    #[test]
    fn separate_grok_provider_uses_saved_login_without_switching_codex() {
        let root = std::env::temp_dir().join(format!("kyoto-image-auth-{}", std::process::id()));
        fs::create_dir_all(root.join("workspace")).unwrap();
        let session = Session::at(&root.join("session"));
        session
            .create(&crate::session::SessionMeta::new(
                "image-auth",
                &root.join("workspace"),
                "coding-model",
                "2026-10-07T00:00:00.000Z",
            ))
            .unwrap();
        let tools = Tools::at(&session).unwrap();
        fs::write(
            root.join(crate::auth::AUTH_FILE),
            serde_json::to_vec(&crate::auth::Tokens {
                access_token: "fixture-image-token".into(),
                refresh_token: "fixture-refresh-token".into(),
                expires_at: "2099-01-01T00:00:00.000Z".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = crate::config::Config::from_toml(&format!(
            "provider = \"codex\"\n[providers.codex]\nkind = \"codex\"\nmodel = \"coding-model\"\n[providers.grok]\nbase_url = \"http://{}/v1\"\nmodel = \"grok-chat\"\n", listener.local_addr().unwrap(),
        )).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.starts_with("post /v1/images/generations "));
            assert!(headers.contains("authorization: bearer fixture-image-token"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["model"], "grok-imagine-image");
            assert!(body.get("provider").is_none());
            let mut image = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1, 1)
                .write_to(&mut image, image::ImageFormat::Png)
                .unwrap();
            let response = json!({"data": [{"b64_json": base64::engine::general_purpose::STANDARD.encode(image.into_inner())}]}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
        });
        tools.gate().queue(crate::permit::Answer::allow_once());
        tools.gate().queue(crate::permit::Answer::allow_once());
        let request = serde_json::from_value(
            json!({"model": "grok-imagine-image", "prompt": "garden", "path": "garden.png"}),
        )
        .unwrap();
        let output = tools
            .generate_image_in_root("t1", &config, request, Some(&root))
            .unwrap();
        assert!(output.created);
        assert!(config.is_codex());
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn gpt_image_model_uses_saved_codex_login_while_coding_with_grok() {
        let root =
            std::env::temp_dir().join(format!("kyoto-codex-image-auth-{}", std::process::id()));
        fs::create_dir_all(root.join("workspace")).unwrap();
        let session = Session::at(&root.join("session"));
        session
            .create(&crate::session::SessionMeta::new(
                "image-auth",
                &root.join("workspace"),
                "coding-model",
                "2026-10-07T00:00:00.000Z",
            ))
            .unwrap();
        let tools = Tools::at(&session).unwrap();
        fs::write(
            root.join(crate::auth::CODEX_AUTH_FILE),
            serde_json::to_vec(&crate::auth::CodexTokens {
                access_token: "fixture-codex-token".into(),
                id_token: "fixture-id-token".into(),
                account_id: "fixture-account".into(),
                refresh_token: "fixture-refresh-token".into(),
                expires_at: "2099-01-01T00:00:00.000Z".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = crate::config::Config::from_toml(&format!(
            "provider = \"grok\"\n[providers.codex]\nkind = \"codex\"\nbase_url = \"http://{}/v1\"\nmodel = \"coding-model\"\n[providers.grok]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"grok-chat\"\n", listener.local_addr().unwrap(),
        )).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.starts_with("post /v1/images/generations "));
            assert!(headers.contains("authorization: bearer fixture-codex-token"));
            assert!(headers.contains("chatgpt-account-id: fixture-account"));
            assert!(headers.contains("originator: codex_cli_rs"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["model"], "gpt-image-2");
            assert!(body.get("provider").is_none());
            let mut image = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1, 1)
                .write_to(&mut image, image::ImageFormat::Png)
                .unwrap();
            let response = json!({"data": [{"b64_json": base64::engine::general_purpose::STANDARD.encode(image.into_inner())}]}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
        });
        tools.gate().queue(crate::permit::Answer::allow_once());
        tools.gate().queue(crate::permit::Answer::allow_once());
        let request = serde_json::from_value(
            json!({"model": "gpt-image-2", "prompt": "garden", "path": "garden.png"}),
        )
        .unwrap();
        let output = tools
            .generate_image_in_root("t1", &config, request, Some(&root))
            .unwrap();
        assert!(output.created);
        assert_eq!(config.provider.as_deref(), Some("grok"));
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
