use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use kyotoagent::pairing::{connection, PairingKey};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn pairing_codes_expire_after_ten_minutes_and_exchange_only_once() {
    let root = std::env::temp_dir().join(format!("kyotoagent-codes-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let key = PairingKey::load(&root).unwrap();
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let code = key.code().unwrap();
    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let code_path = root.join("pairing-codes").join(&code);
    let expires = fs::read_to_string(&code_path)
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!((before + 600..=after + 600).contains(&expires));
    assert_eq!(
        fs::metadata(&code_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.join("pairing-codes"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(!key.accepts(&code));
    assert!(PairingKey::load(&root).unwrap().accepts_code(&code));
    assert!(!PairingKey::load(&root.join("other"))
        .unwrap()
        .accepts_code(&code));
    assert!(!key.accepts_code("../pairing.key"));
    let token = key.exchange(&code).unwrap();
    assert!(key.accepts(&token));
    assert!(!key.accepts_code(&token));
    assert!(!key.accepts_code(&code));
    assert!(key.exchange(&code).is_err());
    assert!(!code_path.exists());
    let expired = key.code().unwrap();
    let expired_path = root.join("pairing-codes").join(&expired);
    fs::write(&expired_path, before.to_string()).unwrap();
    assert!(!key.accepts_code(&expired));
    assert!(key.exchange(&expired).is_err());
    let next = key.code().unwrap();
    assert_ne!(expired, next);
    assert!(!expired_path.exists());
    assert!(PairingKey::load(&root).unwrap().accepts(&token));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_exchanges_cannot_redeem_the_same_code_twice() {
    let root = std::env::temp_dir().join(format!("kyotoagent-code-race-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let key = PairingKey::load(&root).unwrap();
    let code = key.code().unwrap();
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let reloaded = PairingKey::load(&root).unwrap();
                    barrier.wait();
                    reloaded.exchange(&code).is_ok()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|handle| usize::from(handle.join().unwrap()))
                .sum::<usize>(),
            1
        );
    });
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pairing_key_issues_permanent_tokens_that_survive_reload_and_reject_forgery() {
    let root = std::env::temp_dir().join(format!("kyotoagent-pairing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let key = PairingKey::load(&root).expect("key");
    let token = key.token().expect("token");
    let payload = token.split('.').nth(1).expect("payload");
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("payload bytes");
    let claims: serde_json::Value = serde_json::from_slice(&payload).expect("claims");
    assert!(claims.get("exp").is_none());
    assert_eq!(claims["iss"], "kyotoagent");
    assert_eq!(claims["aud"], "kyotoagent-paired-client");
    assert!(key.accepts(&token));
    assert!(PairingKey::load(&root).expect("reload").accepts(&token));
    assert_eq!(
        fs::metadata(root.join("pairing.key"))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let other = root.join("other");
    assert!(!PairingKey::load(&other).expect("other key").accepts(&token));
    assert!(!key.accepts("invalid"));
    let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
    let replacement = if parts[2].starts_with('A') { "B" } else { "A" };
    parts[2].replace_range(..1, replacement);
    assert!(!key.accepts(&parts.join(".")));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn legacy_pairing_tokens_are_rejected_even_with_the_server_signing_key() {
    let root = std::env::temp_dir().join(format!("kyotoagent-legacy-token-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let key = PairingKey::load(&root).unwrap();
    let secret = EncodingKey::from_secret(&fs::read(root.join("pairing.key")).unwrap());
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    for claims in [
        serde_json::json!({"iss":"kyotoagent", "aud":"kyotoagent-client"}),
        serde_json::json!({"iss":"kyotoagent", "aud":"kyotoagent-client", "exp":expires}),
    ] {
        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &secret).unwrap();
        assert!(!key.accepts(&token));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn access_tokens_validate_expiration_and_required_identity_claims() {
    let root = std::env::temp_dir().join(format!("kyotoagent-claims-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let key = PairingKey::load(&root).expect("key");
    let bytes = fs::read(root.join("pairing.key")).expect("private key");
    let secret = EncodingKey::from_secret(&bytes);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let valid = serde_json::json!({
        "exp": now + 3600,
        "iss": "kyotoagent",
        "aud": "kyotoagent-paired-client"
    });
    let current = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &valid, &secret).unwrap();
    assert!(key.accepts(&current));
    for claims in [
        serde_json::json!({"exp": 0, "iss": "kyotoagent", "aud": "kyotoagent-paired-client"}),
        serde_json::json!({"iss": "other", "aud": "kyotoagent-paired-client"}),
        serde_json::json!({"iss": "kyotoagent", "aud": "other"}),
        serde_json::json!({"aud": "kyotoagent-paired-client"}),
        serde_json::json!({"iss": "kyotoagent"}),
    ] {
        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &secret).unwrap();
        assert!(!key.accepts(&token), "{claims}");
    }
    let other_algorithm =
        jsonwebtoken::encode(&Header::new(Algorithm::HS384), &valid, &secret).unwrap();
    assert!(!key.accepts(&other_algorithm));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn connection_uri_keeps_token_out_of_the_https_address() {
    let (base, token) = connection("kyotoagent://box.example:7841?token=test-token").expect("URI");
    assert_eq!(base, "https://box.example:7841");
    assert_eq!(token.as_deref(), Some("test-token"));
    assert_eq!(
        connection("kyotoagent://[::1]:7841?token=test-token")
            .expect("IPv6")
            .0,
        "https://[::1]:7841"
    );
    for invalid in [
        "kyotoagent://box.example:7841",
        "kyotoagent://box.example:7841?token=",
        "kyotoagent://box.example:7841?token=a&token=b",
        "kyotoagent://user:pass@box.example:7841?token=a",
        "https://box.example:7841?token=a",
        "kyotoagent://box.example:7841/path?token=a",
    ] {
        assert!(connection(invalid).is_err(), "{invalid}");
    }
}
