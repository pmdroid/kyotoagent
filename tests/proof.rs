use std::fs::{self, File};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::events::{Event, EventKind, ProofBody, ProofFailure, ProofItem, ResultBody};
use kyotoagent::proof::{open_file, store_file, versions, ProofVersion};
use kyotoagent::server::Server;
use kyotoagent::session::{Session, SessionMeta};

const AT: &str = "2026-10-03T12:00:00.000Z";

#[test]
fn artifact_history_requires_published_files() {
    let events = [Event::new("e1", AT, "t1", EventKind::Proof)
        .with_body(&ProofBody {
            failures: vec![ProofFailure {
                argv: vec!["gh".into(), "pr".into(), "list".into()],
                exit: 1,
                tail: "HTTP 504".into(),
            }],
            wrote: vec!["report.md".into()],
            items: vec![ProofItem {
                id: "test".into(),
                kind: "command".into(),
                outcome: "failed".into(),
                argv: vec!["false".into()],
                exit: Some(1),
                tail: "failed".into(),
            }],
            ..Default::default()
        })
        .unwrap()];
    assert!(kyotoagent::proof::artifact_history(&events)
        .unwrap()
        .is_empty());
    assert_eq!(versions(&events).unwrap()[0].proof.failures[0].exit, 1);
}

#[test]
fn legacy_attachment_tool_is_unknown() {
    assert!(!kyotoagent::turn::known_tool_names().contains(&"attach_proof"));
}

fn add_version(session: &Session, turn: &str, proof: ProofBody) {
    let result = Event::new(
        &session.next_event_id().unwrap(),
        AT,
        turn,
        EventKind::Result,
    )
    .with_body(&ResultBody {
        text: format!("Answer for {turn}"),
        note: String::new(),
    })
    .unwrap();
    session.append(&result).unwrap();
    let proof = Event::new(
        &session.next_event_id().unwrap(),
        AT,
        turn,
        EventKind::Proof,
    )
    .with_body(&proof)
    .unwrap();
    session.append(&proof).unwrap();
}

fn create_session(root: &Path, workspace: &Path, id: &str) -> Session {
    let session = Session::at(&root.join("sessions").join(id));
    session
        .create(&SessionMeta::new(id, workspace, "test/model", AT))
        .unwrap();
    session
}

#[tokio::test]
async fn remote_proof_history_and_downloads_preserve_versions_after_workspace_deletion() {
    let root = std::env::temp_dir().join(format!("ka-proof-remote-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let session = create_session(&root, &workspace, "c0ffee01");
    let other = create_session(&root, &workspace, "c0ffee02");
    let mut files = Vec::new();
    for (name, bytes, media_type) in [
        (
            "report.md",
            b"# Report\nVerified changes.\n".as_slice(),
            "text/markdown",
        ),
        ("screenshot.png", kyotoagent::splash::PNG, "image/png"),
        (
            "document.pdf",
            b"%PDF-1.7\n%%EOF\n".as_slice(),
            "application/pdf",
        ),
        (
            "preview.html",
            b"<!doctype html><h1>Report</h1>".as_slice(),
            "text/html",
        ),
        (
            "bundle.zip",
            b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0".as_slice(),
            "application/zip",
        ),
        (
            "binary.dat",
            b"\0\xff\x01\x02".as_slice(),
            "application/octet-stream",
        ),
        (
            "movie.mp4",
            b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom".as_slice(),
            "video/mp4",
        ),
    ] {
        let path = workspace.join(name);
        fs::write(&path, bytes).unwrap();
        let mut artifact = store_file(&session, name, File::open(path).unwrap()).unwrap();
        artifact.git_sha = Some("b".repeat(40));
        assert_eq!(artifact.size, bytes.len() as u64);
        assert_eq!(artifact.media_type, media_type);
        assert_eq!(artifact.sha256.len(), 64);
        assert_eq!(artifact.id.len(), 32);
        assert_eq!(
            fs::metadata(session.dir().join("proof").join(&artifact.id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        files.push((artifact, bytes.to_vec()));
    }
    assert_eq!(
        fs::metadata(session.dir().join("proof"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    add_version(
        &session,
        "t1",
        ProofBody {
            files: files.iter().map(|(file, _)| file.clone()).collect(),
            failures: vec![ProofFailure {
                argv: vec!["false".into()],
                exit: 1,
                tail: "failed check".into(),
            }],
            items: vec![ProofItem {
                id: "lint".into(),
                kind: "command".into(),
                outcome: "failed".into(),
                argv: vec!["false".into()],
                exit: Some(1),
                tail: "failed check".into(),
            }],
            wrote: vec!["report.md".into()],
            ..ProofBody::default()
        },
    );
    fs::write(workspace.join("report.md"), b"# Revised report\n").unwrap();
    let revised = store_file(
        &session,
        "report.md",
        File::open(workspace.join("report.md")).unwrap(),
    )
    .unwrap();
    assert_ne!(files[0].0.id, revised.id);
    add_version(
        &session,
        "t2",
        ProofBody {
            files: vec![revised.clone()],
            ..ProofBody::default()
        },
    );
    add_version(&session, "t3", ProofBody::default());
    fs::remove_dir_all(&workspace).unwrap();

    fs::create_dir_all(root.join("certs")).unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    fs::write(root.join("certs/server.crt"), certificate.cert.pem()).unwrap();
    fs::write(
        root.join("certs/server.key"),
        certificate.key_pair.serialize_pem(),
    )
    .unwrap();
    let config = Config::from_toml(
        "base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\nlisten = \"127.0.0.1:0\"\n",
    )
    .unwrap();
    let server = Arc::new(Server::new(&root, &config).unwrap());
    let serving = Arc::clone(&server);
    let running = tokio::spawn(async move { serving.serve().await });
    let deadline = Instant::now() + Duration::from_secs(10);
    let address = loop {
        if let Some(address) = server.https_addr() {
            break address;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let token = kyotoagent::pairing::PairingKey::load(&root)
        .unwrap()
        .token()
        .unwrap();
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let history_url = format!("https://{address}/v1/sessions/c0ffee01/proof");
    assert_eq!(client.get(&history_url).send().await.unwrap().status(), 401);
    let history: Vec<ProofVersion> = client
        .get(&history_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(
        history
            .iter()
            .map(|version| version.version)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(history[0].turn_id, "t1");
    assert_eq!(history[0].at, AT);
    assert_eq!(history[0].response.as_ref().unwrap().text, "Answer for t1");
    assert_eq!(history[0].proof.items[0].outcome, "failed");
    assert_eq!(history[0].proof.files[0].git_sha, Some("b".repeat(40)));
    assert!(history[2].proof.files.is_empty());
    assert_eq!(history[2].response.as_ref().unwrap().text, "Answer for t3");
    let live_file = kyotoagent::proof::store_bytes(&session, "live.md", b"# Live report").unwrap();
    kyotoagent::proof::publish(
        &session,
        "t4",
        &kyotoagent::events::ArtifactBody {
            source: kyotoagent::events::ArtifactSource::Agent,
            file: live_file.clone(),
            caption: Some("Live report".into()),
            check: None,
        },
    )
    .unwrap();
    let live_url = format!(
        "https://{address}/v1/sessions/c0ffee01/artifacts/files/{}",
        live_file.id
    );
    assert_eq!(client.get(&live_url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .get(&live_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        b"# Live report".as_slice()
    );
    let view: kyotoagent::view::View = client
        .get(format!("https://{address}/v1/sessions/c0ffee01/view"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cards: Vec<_> = view
        .cards
        .iter()
        .filter(|card| card.kind == kyotoagent::view::CardKind::Artifact)
        .collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].body["file"]["id"], live_file.id);
    assert_eq!(cards[0].body["turnId"], "t4");
    let retained =
        kyotoagent::proof::store_bytes(&session, "test-attempt-1.txt", b"failed check").unwrap();
    session
        .append(
            &Event::new("retained", AT, "t5", EventKind::CloseoutRun)
                .with_body(&kyotoagent::events::CloseoutRunBody {
                    passed: None,
                    transcript: Some(retained.clone()),
                    argv: vec!["false".into()],
                    timed_out: false,
                    truncated: false,
                    id: "test".into(),
                    attempt: 1,
                    exit: 1,
                    tail: "failed check".into(),
                    workspace_fingerprint: String::new(),
                })
                .unwrap(),
        )
        .unwrap();
    let retained_url = format!(
        "https://{address}/v1/sessions/c0ffee01/artifacts/files/{}",
        retained.id
    );
    assert_eq!(
        client.get(&retained_url).send().await.unwrap().status(),
        401
    );
    assert_eq!(
        client
            .get(&retained_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        b"failed check".as_slice()
    );
    let artifacts: Vec<ProofVersion> = client
        .get(format!("https://{address}/v1/sessions/c0ffee01/artifacts"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(artifacts.len(), 3);
    assert_eq!(artifacts[2].turn_id, "t4");
    assert!(artifacts[2].response.is_none());
    assert_eq!(artifacts[2].proof.files, vec![live_file]);
    for (artifact, bytes) in files {
        let url = format!("{history_url}/files/{}", artifact.id);
        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        let response = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        assert_eq!(response.headers()["content-type"], artifact.media_type);
        assert!(response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;"));
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert!(response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox"));
        assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
        let other_url = format!(
            "https://{address}/v1/sessions/c0ffee02/proof/files/{}",
            artifact.id
        );
        assert_eq!(
            client
                .get(other_url)
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    let revised_response = client
        .get(format!("{history_url}/files/{}", revised.id))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        revised_response.bytes().await.unwrap().as_ref(),
        b"# Revised report\n"
    );
    assert_eq!(
        client
            .delete(format!("https://{address}/v1/sessions/c0ffee01"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert!(!session.dir().exists());
    assert!(other.dir().exists());
    assert_eq!(
        client
            .get(&history_url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    running.abort();
    let _ = running.await;
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn proof_storage_rejects_non_files_unsafe_names_and_blob_symlinks() {
    let root = std::env::temp_dir().join(format!("ka-proof-paths-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let session = create_session(&root, &root, "session");
    assert!(store_file(&session, "folder", File::open(&root).unwrap()).is_err());
    let source = root.join("source");
    fs::write(&source, b"proof").unwrap();
    for name in ["", "../secret", "report\n.txt", "/tmp/secret"] {
        assert!(store_file(&session, name, File::open(&source).unwrap()).is_err());
    }
    let stored = store_file(&session, "source", File::open(&source).unwrap()).unwrap();
    let blob = session.dir().join("proof").join(&stored.id);
    fs::remove_file(&blob).unwrap();
    symlink(&source, &blob).unwrap();
    assert!(open_file(&session, &stored.id).is_err());
    assert!(open_file(&session, "../source").is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_proof_events_remain_versioned_and_link_to_their_original_response() {
    let result = Event::new("e1", AT, "t1", EventKind::Result)
        .with_body(&ResultBody {
            text: "An answer".into(),
            note: String::new(),
        })
        .unwrap();
    let proof = Event::new("e2", AT, "t1", EventKind::Proof)
        .with_body(&serde_json::json!({"text":"Recorded checks","wrote":[],"items":[]}))
        .unwrap();
    let versions = versions(&[result, proof]).unwrap();
    assert_eq!(versions[0].version, 1);
    assert_eq!(versions[0].response.as_ref().unwrap().event_id, "e1");
    assert!(versions[0].proof.files.is_empty());
}

#[test]
fn live_artifacts_merge_with_finished_evidence_and_omit_empty_turns() {
    let file = kyotoagent::proof::ProofFile {
        id: "0123456789abcdef0123456789abcdef".into(),
        name: "report.md".into(),
        media_type: "text/markdown".into(),
        size: 3,
        sha256: "a".repeat(64),
        git_sha: None,
    };
    let event = |id, turn, kind, body| Event::new(id, AT, turn, kind).with_body(&body).unwrap();
    let mut events = vec![
        event(
            "e1",
            "t1",
            EventKind::Proof,
            serde_json::json!({"text":"Hello"}),
        ),
        event(
            "e2",
            "t2",
            EventKind::Artifact,
            serde_json::json!({"file":file}),
        ),
    ];
    let live = kyotoagent::proof::artifact_history(&events).unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].proof.files, vec![file.clone()]);
    assert!(live[0].response.is_none());
    events.push(event(
        "e3",
        "t2",
        EventKind::Result,
        serde_json::json!({"text":"Report is ready"}),
    ));
    events.push(event(
        "e4",
        "t2",
        EventKind::Proof,
        serde_json::json!({"files":[file]}),
    ));
    let history = kyotoagent::proof::artifact_history(&events).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].version, live[0].version);
    assert_eq!(history[0].event_id, live[0].event_id);
    assert_eq!(history[0].proof.files.len(), 1);
    assert_eq!(
        history[0].response.as_ref().unwrap().text,
        "Report is ready"
    );
}

#[test]
fn canonical_tool_permissions_allow_only_the_artifact_tool() {
    let config =
        Config::from_toml("base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\n[profiles.evidence]\ntools = [\"attach_artifact\", \"finish\"]\n").unwrap();
    assert!(config.tool_allowed(Some("evidence"), "attach_artifact"));
    assert!(!config.tool_allowed(Some("evidence"), "write_file"));
    let tools = kyotoagent::turn::tool_definitions_for(&config, false, Some("evidence"));
    assert!(tools.iter().any(|tool| tool.name == "attach_artifact"));
    assert!(!tools.iter().any(|tool| tool.name == "attach_proof"));
}
