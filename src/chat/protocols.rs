use super::*;

#[cfg(test)]
mod tests;
use super::opencode::{Model, Protocol};
use serde_json::json;

pub(super) fn request(
    model: &Model,
    messages: &[Message],
    tools: &[Tool],
    effort: Option<&str>,
) -> Result<(String, Value), ChatError> {
    let base = model.base.trim_end_matches('/');
    let result = match model.protocol {
        Protocol::Chat => {
            let mut body = serde_json::to_value(Request {
                model: &model.id,
                stream: true,
                tool_choice: (!tools.is_empty()).then_some("auto"),
                messages,
                tools: (!tools.is_empty()).then_some(tools),
                reasoning_effort: effort,
                service_tier: None,
            })
            .map_err(ChatError::Decode)?;
            for (index, message) in messages.iter().enumerate() {
                if let Message::Assistant { native, .. } = message {
                    if let Some(reasoning) = replay(native, model, "chat")
                        .and_then(|value| value.get("reasoning_content"))
                    {
                        body["messages"][index]["reasoning_content"] = reasoning.clone();
                    }
                }
            }
            (format!("{base}/chat/completions"), body)
        }
        Protocol::Responses => {
            let mut body = responses_body(&model.id, effort, None, &[], tools, true);
            let mut input = Vec::new();
            let mut instructions = Vec::new();
            for message in messages {
                if let Message::System { content } = message {
                    instructions.push(content.clone());
                    continue;
                }
                if let Message::Assistant { native, .. } = message {
                    if let Some(output) = replay(native, model, "responses")
                        .and_then(|value| value.get("output"))
                        .and_then(Value::as_array)
                    {
                        input.extend(output.clone());
                        continue;
                    }
                }
                let mapped = responses_body(
                    &model.id,
                    None,
                    None,
                    std::slice::from_ref(message),
                    &[],
                    true,
                );
                input.extend(mapped["input"].as_array().unwrap().clone());
            }
            body["input"] = input.into();
            body["include"] = json!(["reasoning.encrypted_content"]);
            if !instructions.is_empty() {
                body["instructions"] = instructions.join("\n").into();
            }
            (format!("{base}/responses"), body)
        }
        Protocol::Anthropic => (
            format!("{base}/messages"),
            anthropic(model, messages, tools, effort)?,
        ),
        Protocol::Gemini => {
            let mut url =
                reqwest::Url::parse(&format!("{base}/models/")).map_err(|_| ChatError::NoChoice)?;
            url.path_segments_mut()
                .map_err(|_| ChatError::NoChoice)?
                .pop_if_empty()
                .push(&format!("{}:generateContent", model.id));
            (url.to_string(), gemini(model, messages, tools, effort)?)
        }
    };
    Ok(result)
}

fn replay<'a>(native: &'a Option<Value>, model: &Model, protocol: &str) -> Option<&'a Value> {
    native
        .as_ref()
        .filter(|value| {
            value["model"].as_str() == Some(&model.id)
                && value["protocol"].as_str() == Some(protocol)
        })
        .and_then(|value| value.get("content"))
}

fn append(messages: &mut Vec<Value>, role: &str, field: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut().filter(|last| last["role"] == role) {
        last[field].as_array_mut().unwrap().extend(parts);
    } else {
        messages.push(json!({"role": role, field: parts}));
    }
}

fn image(url: &str) -> Result<(&str, &str), ChatError> {
    let (mime, data) = url
        .strip_prefix("data:")
        .and_then(|url| url.split_once(";base64,"))
        .ok_or_else(|| ChatError::Status {
            status: 400,
            body: "This model requires image attachments encoded as data URLs.".into(),
        })?;
    Ok((mime, data))
}

fn arguments(call: &ToolCall) -> Result<Value, ChatError> {
    serde_json::from_str(&call.arguments).map_err(ChatError::Decode)
}

fn anthropic(
    model: &Model,
    messages: &[Message],
    tools: &[Tool],
    effort: Option<&str>,
) -> Result<Value, ChatError> {
    let mut system = Vec::new();
    let mut output = Vec::new();
    for message in messages {
        let (role, content) = match message {
            Message::System { content } => {
                system.push(json!({"type":"text", "text":content}));
                continue;
            }
            Message::User { content } => {
                let mut parts = Vec::new();
                match content {
                    UserContent::Text(text) => parts.push(json!({"type":"text", "text":text})),
                    UserContent::Parts(items) => {
                        for part in items {
                            parts.push(match part {
                            UserPart::Text { text } => json!({"type":"text", "text":text}),
                            UserPart::ImageUrl { image_url } => {
                                let (mime, data) = image(&image_url.url)?;
                                json!({"type":"image", "source":{"type":"base64", "media_type":mime, "data":data}})
                            }
                        });
                        }
                    }
                }
                ("user", parts)
            }
            Message::Assistant {
                content,
                tool_calls,
                native,
            } => {
                let parts = if let Some(parts) =
                    replay(native, model, "anthropic").and_then(Value::as_array)
                {
                    parts.clone()
                } else {
                    let mut parts = Vec::new();
                    if let Some(text) = content.as_ref().filter(|text| !text.is_empty()) {
                        parts.push(json!({"type":"text", "text":text}));
                    }
                    for call in tool_calls {
                        parts.push(json!({"type":"tool_use", "id":call.id, "name":call.name, "input":arguments(call)?}));
                    }
                    parts
                };
                ("assistant", parts)
            }
            Message::Tool {
                tool_call_id,
                content,
            } => (
                "user",
                vec![json!({"type":"tool_result", "tool_use_id":tool_call_id, "content":content})],
            ),
        };
        append(&mut output, role, "content", content);
    }
    let mut body = json!({"model":model.id, "messages":output, "max_tokens":model.output.min(32768), "stream":false});
    if let Some(effort) = effort {
        body["output_config"] = json!({"effort":effort});
    }
    if !system.is_empty() {
        body["system"] = system.into();
    }
    if !tools.is_empty() {
        body["tools"] = tools.iter().map(|tool| json!({"name":tool.name,"description":tool.description,"input_schema":tool.parameters})).collect();
        body["tool_choice"] = json!({"type":"auto"});
    }
    Ok(body)
}

fn gemini(
    model: &Model,
    messages: &[Message],
    tools: &[Tool],
    effort: Option<&str>,
) -> Result<Value, ChatError> {
    let mut system = Vec::new();
    let mut contents = Vec::new();
    let mut names = std::collections::BTreeMap::new();
    for message in messages {
        let (role, parts) = match message {
            Message::System { content } => {
                system.push(json!({"text":content}));
                continue;
            }
            Message::User { content } => {
                let mut parts = Vec::new();
                match content {
                    UserContent::Text(text) => parts.push(json!({"text":text})),
                    UserContent::Parts(items) => {
                        for part in items {
                            parts.push(match part {
                                UserPart::Text { text } => json!({"text":text}),
                                UserPart::ImageUrl { image_url } => {
                                    let (mime, data) = image(&image_url.url)?;
                                    json!({"inlineData":{"mimeType":mime,"data":data}})
                                }
                            });
                        }
                    }
                }
                ("user", parts)
            }
            Message::Assistant {
                content,
                tool_calls,
                native,
            } => {
                for call in tool_calls {
                    names.insert(call.id.clone(), call.name.clone());
                }
                let parts = if let Some(parts) =
                    replay(native, model, "gemini").and_then(Value::as_array)
                {
                    parts.clone()
                } else {
                    let mut parts = Vec::new();
                    if let Some(text) = content.as_ref().filter(|text| !text.is_empty()) {
                        parts.push(json!({"text":text}));
                    }
                    for call in tool_calls {
                        parts.push(json!({"functionCall":{"id":call.id,"name":call.name,"args":arguments(call)?}, "thoughtSignature":"skip_thought_signature_validator"}));
                    }
                    parts
                };
                ("model", parts)
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                let name = names.get(tool_call_id).ok_or(ChatError::NoChoice)?;
                (
                    "user",
                    vec![
                        json!({"functionResponse":{"id":tool_call_id,"name":name,"response":{"output":content}}}),
                    ],
                )
            }
        };
        append(&mut contents, role, "parts", parts);
    }
    let mut body = json!({"contents":contents, "generationConfig":{"maxOutputTokens":model.output.min(32768)}});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts":system});
    }
    if let Some(effort) = effort {
        body["generationConfig"]["thinkingConfig"] =
            json!({"thinkingLevel":effort.to_uppercase(),"includeThoughts":true});
    }
    if !tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations":tools.iter().map(|tool| json!({"name":tool.name,"description":tool.description,"parametersJsonSchema":tool.parameters})).collect::<Vec<_>>()}]);
    }
    Ok(body)
}

pub(super) async fn read(
    response: reqwest::Response,
    model: &Model,
    thoughts: Option<&Arc<Mutex<String>>>,
) -> Result<Reply, ChatError> {
    match model.protocol {
        Protocol::Chat | Protocol::Responses => {
            let responses = model.protocol == Protocol::Responses;
            let mut reply = read_completion(
                response,
                thoughts,
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
                responses,
            )
            .await?;
            if let Some(content) = reply.native.take() {
                reply.native = Some(
                    json!({"model":model.id, "protocol": if responses { "responses" } else { "chat" }, "content":content}),
                );
            }
            return Ok(reply);
        }
        _ => {}
    }
    let status = response.status().as_u16();
    let text = response.text().await.map_err(ChatError::Transport)?;
    if !(200..300).contains(&status) {
        return Err(ChatError::Status { status, body: text });
    }
    let value: Value = serde_json::from_str(&text).map_err(ChatError::Decode)?;
    decode(model, &value, thoughts)
}

fn decode(
    model: &Model,
    value: &Value,
    thoughts: Option<&Arc<Mutex<String>>>,
) -> Result<Reply, ChatError> {
    let anthropic = model.protocol == Protocol::Anthropic;
    let (parts, finish) = if anthropic {
        (&value["content"], value["stop_reason"].as_str())
    } else {
        (
            &value["candidates"][0]["content"]["parts"],
            value["candidates"][0]["finishReason"].as_str(),
        )
    };
    if !matches!(
        finish,
        Some("end_turn" | "tool_use" | "stop_sequence" | "STOP")
    ) {
        return Err(ChatError::Status {
            status: 502,
            body: format!(
                "The model generation ended with {}.",
                finish.unwrap_or("no finish reason")
            ),
        });
    }
    let mut reply = Reply::default();
    let mut saved_parts = parts.clone();
    let mut text = String::new();
    for (index, part) in parts
        .as_array()
        .ok_or(ChatError::NoChoice)?
        .iter()
        .enumerate()
    {
        let reasoning = part["type"] == "thinking" || part["thought"] == true;
        if let Some(content) = part["text"].as_str().or_else(|| part["thinking"].as_str()) {
            if reasoning {
                if let Some(thoughts) = thoughts {
                    thoughts.lock().unwrap().push_str(content);
                }
            } else {
                text.push_str(content);
            }
        }
        let call = if part["type"] == "tool_use" {
            Some((&part["id"], &part["name"], &part["input"]))
        } else {
            part.get("functionCall")
                .map(|call| (&call["id"], &call["name"], &call["args"]))
        };
        if let Some((id, name, args)) = call {
            let id = id
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("gemini_{index}"));
            if !anthropic {
                saved_parts[index]["functionCall"]["id"] = id.clone().into();
            }
            reply.tool_calls.push(ToolCall {
                id,
                kind: function_kind(),
                name: name.as_str().ok_or(ChatError::NoChoice)?.into(),
                arguments: args.to_string(),
            });
        }
    }
    if text.is_empty() && reply.tool_calls.is_empty() {
        return Err(ChatError::NoChoice);
    }
    validate_tool_calls(&reply.tool_calls)?;
    reply.content = (!text.is_empty()).then_some(text);
    reply.native = Some(
        json!({"model":model.id, "protocol":if anthropic { "anthropic" } else { "gemini" }, "content":saved_parts}),
    );
    reply.prompt_tokens = if anthropic {
        value["usage"]["input_tokens"].as_u64().map(|input| {
            input
                + value["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                + value["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or(0)
        })
    } else {
        value["usageMetadata"]["promptTokenCount"].as_u64()
    };
    reply.completion_tokens = if anthropic {
        value["usage"]["output_tokens"].as_u64()
    } else {
        value["usageMetadata"]["candidatesTokenCount"].as_u64()
    };
    Ok(reply)
}
