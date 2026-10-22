use super::*;

fn model(protocol: Protocol) -> Model {
    Model {
        row: ModelRow {
            id: "model".into(),
            aliases: Vec::new(),
            reasoning_efforts: Vec::new(),
            service_tiers: Vec::new(),
            default_reasoning_effort: None,
            context_length: None,
            provider: None,
        },
        protocol,
        id: "model".into(),
        base: "https://opencode.ai/inference/go/openai/v1".into(),
        output: 8192,
        headers: Default::default(),
        body: json!({}),
    }
}

fn tool() -> Tool {
    Tool::new(
        "read_file",
        "Read a file",
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
    )
}

#[test]
fn anthropic_tool_and_signed_thinking_round_trip_with_usage_and_images() {
    let model = model(Protocol::Anthropic);
    let value = json!({"content":[{"type":"thinking","thinking":"consider","signature":"signed"},{"type":"text","text":"Reading"},{"type":"tool_use","id":"call_a","name":"read_file","input":{"path":"a"}}],"stop_reason":"tool_use","usage":{"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":4}});
    let thoughts = Arc::new(Mutex::new(String::new()));
    let reply = decode(&model, &value, Some(&thoughts)).unwrap();
    assert_eq!(reply.prompt_tokens, Some(60));
    assert_eq!(reply.completion_tokens, Some(4));
    assert_eq!(*thoughts.lock().unwrap(), "consider");
    let messages = vec![
        Message::System {
            content: "system".into(),
        },
        Message::User {
            content: UserContent::Parts(vec![
                UserPart::Text {
                    text: "read".into(),
                },
                UserPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,AAAA".into(),
                    },
                },
            ]),
        },
        Message::assistant_reply(&reply),
        Message::tool_result("call_a", "file contents"),
    ];
    let (_, body) = request(&model, &messages, &[tool()], None).unwrap();
    assert_eq!(body["messages"][1]["content"], value["content"]);
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call_a");
    assert_eq!(
        body["messages"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(body["system"][0]["text"], "system");
    assert_eq!(body["tools"][0]["input_schema"], tool().parameters);
}

#[test]
fn gemini_preserves_parallel_call_signatures_and_function_names() {
    let model = model(Protocol::Gemini);
    let value = json!({"candidates":[{"content":{"parts":[{"text":"consider","thought":true,"thoughtSignature":"thought-sig"},{"functionCall":{"name":"read_file","args":{"path":"a"}},"thoughtSignature":"call-sig"},{"functionCall":{"name":"read_file","args":{"path":"b"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5}});
    let reply = decode(&model, &value, None).unwrap();
    assert_eq!(reply.tool_calls.len(), 2);
    assert_ne!(reply.tool_calls[0].id, reply.tool_calls[1].id);
    let messages = vec![
        Message::User {
            content: "read".into(),
        },
        Message::assistant_reply(&reply),
        Message::tool_result(&reply.tool_calls[0].id, "a"),
        Message::tool_result(&reply.tool_calls[1].id, "b"),
    ];
    let (url, body) = request(&model, &messages, &[tool()], Some("high")).unwrap();
    assert!(url.ends_with("/models/model:generateContent"));
    let parts = &body["contents"][1]["parts"];
    assert_eq!(parts[0]["thoughtSignature"], "thought-sig");
    assert_eq!(parts[1]["thoughtSignature"], "call-sig");
    assert!(parts[2].get("thoughtSignature").is_none());
    assert_eq!(body["contents"][2]["parts"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "read_file"
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "HIGH"
    );
}

#[test]
fn chat_and_responses_replay_reasoning_without_leaking_native_fields() {
    let mut reply = Reply {
        content: Some("hello".into()),
        native: Some(
            json!({"model":"model","protocol":"chat","content":{"reasoning_content":"reasoning"}}),
        ),
        ..Default::default()
    };
    let (_, body) = request(
        &model(Protocol::Chat),
        &[Message::assistant_reply(&reply)],
        &[],
        None,
    )
    .unwrap();
    assert_eq!(body["messages"][0]["reasoning_content"], "reasoning");
    assert!(body["messages"][0].get("native").is_none());
    let output = json!([{"type":"reasoning","id":"r1","encrypted_content":"opaque"},{"type":"function_call","call_id":"c1","name":"read_file","arguments":"{}"}]);
    reply.native =
        Some(json!({"model":"model","protocol":"responses","content":{"output":output}}));
    let (_, body) = request(
        &model(Protocol::Responses),
        &[
            Message::assistant_reply(&reply),
            Message::tool_result("c1", "done"),
        ],
        &[tool()],
        None,
    )
    .unwrap();
    assert_eq!(body["input"][0], output[0]);
    assert_eq!(body["input"][1], output[1]);
    assert_eq!(body["input"][2]["call_id"], "c1");
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn truncated_or_filtered_generations_never_execute_partial_tool_calls() {
    for (protocol, value) in [
        (
            Protocol::Anthropic,
            json!({"content":[{"type":"tool_use","id":"c","name":"read_file","input":{}}],"stop_reason":"max_tokens"}),
        ),
        (
            Protocol::Gemini,
            json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","args":{}}}]},"finishReason":"SAFETY"}]}),
        ),
    ] {
        assert!(decode(&model(protocol), &value, None).is_err());
    }
}
