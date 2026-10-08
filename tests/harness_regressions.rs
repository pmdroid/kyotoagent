use axum::{
    extract::State,
    http::header,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use kyotoagent::{
    chat::{ChatClient, Message},
    config::Config,
    events::EventKind,
    permit::Answer,
    screen::Status,
    session::{Session, SessionMeta},
    turn::{AskOutcome, Runner},
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Model {
    replies: Arc<Mutex<VecDeque<(String, String)>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn complete(State(model): State<Model>, Json(body): Json<Value>) -> impl IntoResponse {
    model.requests.lock().unwrap().push(body);
    let (content_type, body) = model
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| ("application/json".into(), text("done").to_string()));
    ([(header::CONTENT_TYPE, content_type)], body)
}

fn text(value: &str) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":value}}]})
}

fn calls(values: Vec<(&str, Value)>) -> Value {
    let calls: Vec<Value> = values.into_iter().enumerate().map(|(i, (name, args))| json!({"id":format!("call_{i}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect();
    json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":calls}}]})
}

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    session: Session,
    runner: Arc<Runner>,
    config: Config,
    model: Model,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(name: &str, replies: Vec<Value>, extra: &str) -> Self {
        let root = PathBuf::from(format!(
            "/tmp/kyoto-harness-regressions-{}-{name}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("workspace")).unwrap();
        let workspace = root.join("workspace");
        let model = Model {
            replies: Arc::new(Mutex::new(
                replies
                    .into_iter()
                    .map(|reply| ("application/json".into(), reply.to_string()))
                    .collect(),
            )),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(complete))
            .route("/v1/responses", post(complete))
            .route("/v1/models", get(|| async { Json(json!({"data":[{"id":"default-model","context_length":1000000},{"id":"alternate-model","context_length":1000000}]})) }))
            .with_state(model.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = Config::from_toml(&format!(
            "base_url = \"http://{}/v1\"\nmodel = \"default-model\"\ntitle_model = \"\"\n{extra}",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let session = Session::at(&root.join("sessions/s"));
        session
            .create(&SessionMeta::new(
                "s",
                &workspace,
                "default-model",
                "2026-10-03T00:00:00.000Z",
            ))
            .unwrap();
        let runner = Runner::new(&config).unwrap();
        runner.add_session(&session).unwrap();
        Self {
            root,
            workspace,
            session,
            runner,
            config,
            model,
            server,
        }
    }

    fn ask(&self) {
        self.runner.ask("s", "perform the task").unwrap();
    }

    async fn wait_status(&self, status: Status) {
        wait(|| self.session.meta().unwrap().status == status).await;
    }

    async fn wait_result(&self) {
        wait(|| {
            self.session
                .events()
                .unwrap()
                .iter()
                .any(|event| event.kind == EventKind::Result)
        })
        .await;
    }

    fn hooks(&self) {
        fs::create_dir_all(self.workspace.join(".agents")).unwrap();
        fs::write(self.workspace.join(".agents/hooks.json"), json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"if ! test -f stop-fired; then printf fired > stop-fired; printf blocked >&2; exit 2; fi"}]}]}}).to_string()).unwrap();
    }

    fn evidence(&self, value: Value) {
        fs::write(
            self.root.join("observed.json"),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runner.cancel("s");
        let _ = self.runner.answer("s", Answer::deny());
        let question = self
            .runner
            .view("s")
            .ok()
            .and_then(|view| {
                view.cards
                    .into_iter()
                    .rev()
                    .find(|card| card.kind == kyotoagent::view::CardKind::Question)
            })
            .and_then(|card| card.body["eventId"].as_str().map(str::to_string));
        if let Some(question) = question {
            let _ = self.runner.answer_question("s", &question, "cancelled");
        }
        self.server.abort();
    }
}

async fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < deadline, "condition did not become true");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn cancellation_unblocks_pending_permission() {
    let f = Fixture::new(
        "cancel-permission",
        vec![calls(vec![(
            "write_file",
            json!({"path":"file","contents":"x"}),
        )])],
        "",
    )
    .await;
    f.ask();
    f.wait_status(Status::Waiting).await;
    f.runner.cancel("s");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let observed = f.session.meta().unwrap().status;
    f.evidence(json!({"status_after_cancel":format!("{observed:?}"),"file_exists":f.workspace.join("file").exists()}));
    let _ = f.runner.answer("s", Answer::deny());
    f.wait_result().await;
    assert_eq!(
        observed,
        Status::Idle,
        "cancel leaves the permission wait blocked"
    );
}

#[tokio::test]
async fn cancellation_retains_turn_slot_until_cleanup() {
    let f = Fixture::new(
        "cancel-slot",
        vec![
            calls(vec![(
                "run",
                json!({"argv":["sh","-c","trap '' TERM; printf ready > ready; sleep 20"]}),
            )]),
            text("next turn"),
        ],
        "",
    )
    .await;
    f.session.set_yolo(true).unwrap();
    f.ask();
    wait(|| f.workspace.join("ready").exists()).await;
    f.runner.cancel("s");
    let observed = f.runner.ask("s", "next ask").unwrap();
    f.evidence(json!({"ask_immediately_after_cancel":format!("{observed:?}")}));
    tokio::time::sleep(Duration::from_millis(2400)).await;
    assert!(
        matches!(observed, AskOutcome::Queued(_)),
        "a second turn starts while the previous command is still terminating"
    );
}

#[tokio::test]
async fn delete_waits_for_running_turn_cleanup() {
    let f = Fixture::new(
        "delete-wait",
        vec![calls(vec![(
            "run",
            json!({"argv":["sh","-c","trap '' TERM; printf ready > ready; sleep 20"]}),
        )])],
        "",
    )
    .await;
    f.session.set_yolo(true).unwrap();
    f.ask();
    wait(|| f.workspace.join("ready").exists()).await;
    let start = Instant::now();
    f.runner.finish_for_delete("s").await;
    let elapsed = start.elapsed();
    f.evidence(json!({"delete_wait_micros":elapsed.as_micros(),"status_when_delete_returns":format!("{:?}",f.session.meta().unwrap().status)}));
    tokio::time::sleep(Duration::from_millis(2400)).await;
    assert!(
        elapsed >= Duration::from_millis(1500),
        "delete returned after {elapsed:?}, before the two second kill grace period"
    );
}

#[tokio::test]
async fn delete_waits_for_background_task_cleanup() {
    let f = Fixture::new(
        "delete-background-task",
        vec![calls(vec![(
            "start_task",
            json!({"argv":["sh","-c","trap '' TERM; printf ready > ready; sleep 20"]}),
        )])],
        "",
    )
    .await;
    f.session.set_yolo(true).unwrap();
    f.ask();
    wait(|| f.workspace.join("ready").exists()).await;
    f.runner.finish_for_delete("s").await;
    assert!(f
        .session
        .events()
        .unwrap()
        .iter()
        .any(|event| event.kind == EventKind::TaskDone));
}

#[tokio::test]
async fn text_completion_runs_stop_hooks() {
    let f = Fixture::new(
        "text-stop-hook",
        vec![text("first result"), text("checked result")],
        "",
    )
    .await;
    f.hooks();
    f.ask();
    f.wait_result().await;
    assert!(f.workspace.join("stop-fired").exists());
    assert_eq!(f.model.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn text_completion_requires_closeout_checks() {
    let f = Fixture::new(
        "text-closeout",
        vec![
            calls(vec![("write_file", json!({"path":"file","contents":"x"}))]),
            text("premature result"),
            calls(vec![("run_closeout", json!({"id":"required"}))]),
            text("checked result"),
        ],
        "",
    )
    .await;
    fs::create_dir_all(f.workspace.join(".agents")).unwrap();
    fs::write(f.workspace.join(".agents/closeout.yaml"), "version: 1\nitems:\n  - id: required\n    kind: command\n    run: 'true'\n    hint: Run the required check\n").unwrap();
    f.session.set_yolo(true).unwrap();
    f.ask();
    f.wait_result().await;
    let requests = f.model.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[2]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Cannot finish yet")
    );
    assert!(f
        .session
        .events()
        .unwrap()
        .iter()
        .any(|event| event.kind == EventKind::CloseoutRun));
}

#[tokio::test]
async fn text_completion_waits_for_background_tasks() {
    let f = Fixture::new(
        "text-task",
        vec![
            calls(vec![(
                "start_task",
                json!({"argv":["sh","-c","while ! test -f release; do sleep 0.02; done"]}),
            )]),
            text("premature result"),
            calls(vec![("run", json!({"argv":["touch","release"]}))]),
            text("checked result"),
        ],
        "",
    )
    .await;
    f.session.set_yolo(true).unwrap();
    f.ask();
    f.wait_result().await;
    assert!(f.workspace.join("release").exists());
    let requests = f.model.requests.lock().unwrap();
    assert!(
        requests[2]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("still running")
    );
}

#[tokio::test]
async fn cancellation_unblocks_pending_question_and_allows_next_turn() {
    let f = Fixture::new(
        "cancel-question",
        vec![
            calls(vec![(
                "ask",
                json!({"text":"Continue?","choices":["yes","no"]}),
            )]),
            text("next result"),
        ],
        "",
    )
    .await;
    f.ask();
    f.wait_status(Status::Waiting).await;
    f.runner.cancel("s");
    f.wait_status(Status::Idle).await;
    assert!(matches!(
        f.runner.ask("s", "next ask").unwrap(),
        AskOutcome::Started(_) | AskOutcome::Queued(_)
    ));
    wait(|| f.model.requests.lock().unwrap().len() == 2).await;
    f.wait_status(Status::Idle).await;
}

#[tokio::test]
async fn rejected_finish_settles_every_call_before_next_request() {
    let f = Fixture::new(
        "unpaired-tools",
        vec![
            calls(vec![
                ("finish", json!({"text":"done"})),
                ("read_file", json!({"path":"file"})),
            ]),
            text("done"),
        ],
        "",
    )
    .await;
    f.hooks();
    f.ask();
    f.wait_result().await;
    let requests = f.model.requests.lock().unwrap().clone();
    let messages = requests[1]["messages"].as_array().unwrap();
    let call_ids: Vec<&str> = messages
        .iter()
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
        .filter_map(|call| call["id"].as_str())
        .collect();
    let result_ids: Vec<&str> = messages
        .iter()
        .filter_map(|message| message["tool_call_id"].as_str())
        .collect();
    f.evidence(json!({"call_ids":call_ids,"result_ids":result_ids,"request":requests[1]}));
    assert_eq!(
        call_ids, result_ids,
        "the next request contains unanswered tool calls"
    );
}

#[tokio::test]
async fn incomplete_provider_generations_execute_nothing() {
    let call = json!({"id":"call_0","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"file\",\"contents\":\"secret\"}"}});
    let chat_cases = [
        (
            "chat-json-failed",
            "application/json",
            json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","tool_calls":[call]}}]}).to_string(),
        ),
        (
            "chat-sse-eof",
            "text/event-stream",
            format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_0","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"file\",\"contents\":\"secret\"}"}}]}}]})
            ),
        ),
        (
            "chat-sse-error",
            "text/event-stream",
            format!(
                "data: {}\n\ndata: {}\n\n",
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_0","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"file\",\"contents\":\"secret\"}"}}]}}]}),
                json!({"error":{"message":"provider failed"}})
            ),
        ),
    ];
    for (name, content_type, body) in chat_cases {
        let f = Fixture::new(name, vec![], "").await;
        f.model
            .replies
            .lock()
            .unwrap()
            .push_back((content_type.into(), body));
        f.ask();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let events = f.session.events().unwrap();
            if events.iter().any(|event| event.kind == EventKind::Result) {
                break;
            }
            if events
                .iter()
                .any(|event| event.kind == EventKind::Permission)
            {
                let _ = f.runner.answer("s", Answer::deny());
                break;
            }
            assert!(Instant::now() < deadline, "{name} never settled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        f.wait_result().await;
        let wrote = f.workspace.join("file").exists();
        let kinds: Vec<_> = f
            .session
            .events()
            .unwrap()
            .iter()
            .map(|event| event.kind.label())
            .collect();
        f.evidence(json!({"wrote":wrote,"kinds":kinds}));
        assert!(!wrote, "{name} executed a tool");
        assert!(
            !kinds.contains(&"tool_call"),
            "{name} exposed an executable tool call: {kinds:?}"
        );
    }

    let f = Fixture::new("responses-json-incomplete", vec![], "").await;
    let config = Config::from_toml(&format!(
        "provider = \"codex\"\n\n[providers.codex]\nkind = \"codex\"\nbase_url = \"{}\"\nmodel = \"gpt-6.1-sol\"\n",
        f.config.base_url.trim_end_matches("/v1")
    ))
    .unwrap();
    fs::write(
        f.root.join("codex-auth.json"),
        r#"{"access_token":"access","refresh_token":"refresh","id_token":"id","account_id":"acc","expires_at":"2035-01-01T00:00:00.000Z"}"#,
    )
    .unwrap();
    let runner = Runner::with_config_file(&config, &f.root).unwrap();
    f.session
        .update(|meta| {
            meta.model = "gpt-6.1-sol".into();
            true
        })
        .unwrap();
    runner.add_session(&f.session).unwrap();
    f.model.replies.lock().unwrap().push_back((
        "application/json".into(),
        json!({"object":"response","status":"incomplete","output":[{"type":"function_call","call_id":"call_0","name":"write_file","arguments":"{\"path\":\"file\",\"contents\":\"secret\"}"}]}).to_string(),
    ));
    runner.ask("s", "perform the task").unwrap();
    f.wait_result().await;
    assert!(
        !f.workspace.join("file").exists(),
        "an incomplete responses body executed a tool"
    );
}

#[tokio::test]
async fn streaming_arguments_preserve_repeated_fragments() {
    let f = Fixture::new("stream-repetition", vec![], "").await;
    let mut stream = String::new();
    for (index, fragment) in ["{\"path\":\"file\",\"contents\":\"", "ha", "ha", "\"}"]
        .iter()
        .enumerate()
    {
        let call = if index == 0 {
            json!({"index":0,"id":"call_0","type":"function","function":{"name":"write_file","arguments":fragment}})
        } else {
            json!({"index":0,"function":{"arguments":fragment}})
        };
        stream.push_str(&format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"tool_calls":[call]}}]})
        ));
    }
    stream.push_str("data: [DONE]\n\n");
    f.model
        .replies
        .lock()
        .unwrap()
        .push_back(("text/event-stream".into(), stream));
    let reply = ChatClient::new(&f.config)
        .unwrap()
        .complete(
            &[Message::User {
                content: "write".into(),
            }],
            &[],
        )
        .await
        .unwrap();
    let args: Value = serde_json::from_str(&reply.tool_calls[0].arguments).unwrap();
    f.evidence(json!({"decoded_arguments":args}));
    assert_eq!(
        args["contents"], "haha",
        "repeated SSE argument fragments must be appended"
    );
}

#[tokio::test]
async fn subagent_model_is_used_in_actual_request() {
    let f = Fixture::new("model-selection", vec![text("done")], "").await;
    let result = f.runner.spawn_subagent("s", &json!({"prompt":"finish", "description":"different model", "model":"alternate-model", "run_in_background":false})).await;
    let child: Value = serde_json::from_str(&result).unwrap();
    let meta = Session::at(&f.root.join("sessions").join(child["id"].as_str().unwrap()))
        .meta()
        .unwrap();
    let request = f.model.requests.lock().unwrap()[0].clone();
    f.evidence(json!({"child_model":meta.model,"actual_request":request}));
    assert_eq!(request["model"], "alternate-model");
}

#[tokio::test]
async fn profile_denies_session_tools_before_dispatch() {
    let f = Fixture::new(
        "profile",
        vec![
            calls(vec![(
                "spawn_subagent",
                json!({"prompt":"read only", "description":"child", "run_in_background":true}),
            )]),
            text("done"),
            text("done"),
        ],
        "\n[profiles.review]\ntools = [\"read_file\", \"finish\"]\n",
    )
    .await;
    f.session.set_profile(Some("review")).unwrap();
    f.ask();
    f.wait_result().await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let count = fs::read_dir(f.root.join("sessions")).unwrap().count();
    f.evidence(json!({"session_count":count,"requests":f.model.requests.lock().unwrap().clone()}));
    assert_eq!(
        count, 1,
        "spawn_subagent bypasses the profile tool allowlist"
    );
}

#[tokio::test]
async fn cached_views_follow_prompt_metadata_and_external_event_changes() {
    let fixture = Fixture::new("view-cache", vec![], "").await;
    let before = fixture.runner.view("s").unwrap();
    fs::write(
        fixture.workspace.join("AGENTS.md"),
        "Additional workspace instructions. ".repeat(1000),
    )
    .unwrap();
    let changed = fixture.runner.view("s").unwrap();
    assert!(changed.context.unwrap().used > before.context.unwrap().used);
    let mut meta = fixture.session.meta().unwrap();
    meta.status = Status::Working;
    meta.context_length = Some(2000000);
    fixture.session.write_meta(&meta).unwrap();
    let changed = fixture.runner.view("s").unwrap();
    assert_eq!(changed.status, Status::Working);
    assert_eq!(changed.context.unwrap().window, Some(2000000));
    let other = Session::at(fixture.session.dir());
    other
        .append(
            &kyotoagent::events::Event::new(
                "e1",
                "2026-10-03T00:00:00.000Z",
                "t1",
                EventKind::Result,
            )
            .with_body(&json!({"text":"external result"}))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        fixture.runner.view("s").unwrap().cards[0].body["text"],
        "external result"
    );
    fs::write(fixture.session.events_path(), "").unwrap();
    assert!(fixture.runner.view("s").unwrap().cards.is_empty());
}
