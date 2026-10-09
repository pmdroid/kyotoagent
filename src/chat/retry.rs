use reqwest::header::HeaderMap;
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::time::Instant;
pub(super) const BUDGET: Duration = Duration::from_secs(120);
pub(super) const MAX_RETRIES: usize = 3;
pub type RetryStatus = Arc<Mutex<Option<String>>>;
type Registry = HashMap<(String, u64), Arc<Mutex<Option<Instant>>>>;
pub(super) fn shared(endpoint: &str, account: String) -> Arc<Mutex<Option<Instant>>> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    let mut registry = REGISTRY.get_or_init(Mutex::default).lock().unwrap();
    registry.retain(|_, value| {
        Arc::strong_count(value) > 1 || value.lock().unwrap().is_some_and(|d| d > Instant::now())
    });
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    account.hash(&mut hash);
    let key = (endpoint.to_string(), hash.finish());
    if let Some(state) = registry.get(&key) {
        return state.clone();
    }
    let state = Arc::new(Mutex::new(None));
    registry.insert(key, state.clone());
    state
}
pub(super) struct StatusGuard<'a>(pub Option<&'a RetryStatus>);
impl StatusGuard<'_> {
    pub fn set(&self, delay: Duration) {
        if let Some(status) = self.0 {
            *status.lock().unwrap() = Some(format!(
                "Provider busy. Retrying in {}s",
                delay
                    .as_secs()
                    .saturating_add(u64::from(delay.subsec_nanos() > 0))
            ));
        }
    }
    pub fn clear(&self) {
        if let Some(status) = self.0 {
            *status.lock().unwrap() = None;
        }
    }
}
impl Drop for StatusGuard<'_> {
    fn drop(&mut self) {
        self.clear();
    }
}
pub(super) fn delay(headers: &HeaderMap, body: &str, attempt: usize) -> Duration {
    let now = SystemTime::now();
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let nested = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/metadata/headers"));
    let field = |name: &str| -> Vec<String> {
        let mut values = Vec::new();
        if let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) {
            values.push(value.to_string());
        }
        if let Some(value) = nested.and_then(Value::as_object).and_then(|m| {
            m.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        }) {
            if let Some(value) = value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_u64().map(|v| v.to_string()))
            {
                values.push(value);
            }
        }
        values
    };
    for value in field("retry-after") {
        if let Some(delay) = retry_after(&value, now) {
            return delay;
        }
    }
    for value in field("x-ratelimit-reset") {
        if let Ok(seconds) = value.trim().parse::<u64>() {
            if let Some(reset) = SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)) {
                return reset.duration_since(now).unwrap_or_default();
            }
            return Duration::MAX;
        }
    }
    Duration::from_secs(1 << attempt.min(6)) + jitter()
}
pub(super) fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value.trim())
        .ok()
        .map(|date| date.duration_since(now).unwrap_or_default())
}
pub(super) fn jitter() -> Duration {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let seed = time
        ^ SEQUENCE
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9e3779b97f4a7c15);
    Duration::from_millis(25 + seed % 226)
}
pub(super) fn message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "The provider is rate limiting requests.".to_string())
}
pub(super) fn transient(status: u16, body: &str) -> bool {
    matches!(status, 500 | 502 | 503 | 504)
        || (status == 400
            && serde_json::from_str::<Value>(body)
                .ok()
                .is_some_and(|value| {
                    value.pointer("/error/code").and_then(Value::as_str)
                        == Some("server_is_overloaded")
                }))
}
pub(super) fn permanent(body: &str) -> bool {
    let text = message(body).to_ascii_lowercase();
    [
        "insufficient credits",
        "insufficient balance",
        "payment required",
        "credits exhausted",
        "quota exhausted",
        "billing limit",
    ]
    .iter()
    .any(|term| text.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_delay_sources_and_rejects_bad_values() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(retry_after("10", now), Some(Duration::from_secs(10)));
        let date = httpdate::fmt_http_date(now + Duration::from_secs(15));
        assert_eq!(retry_after(&date, now), Some(Duration::from_secs(15)));
        assert_eq!(retry_after("-1", now), None);
        assert_eq!(retry_after("garbage", now), None);
        let mut headers = HeaderMap::new();
        headers.insert("Retry-After", "3".parse().unwrap());
        let body = r#"{"error":{"metadata":{"headers":{"Retry-After":"10"}}}}"#;
        assert_eq!(delay(&headers, body, 0), Duration::from_secs(3));
        assert_eq!(delay(&HeaderMap::new(), body, 0), Duration::from_secs(10));
        headers.insert("Retry-After", "bad".parse().unwrap());
        let fallback = delay(&headers, "", 2);
        assert!(fallback >= Duration::from_secs(4));
        assert!(fallback < Duration::from_secs(5));
        headers.insert("Retry-After", u64::MAX.to_string().parse().unwrap());
        assert!(delay(&headers, "", 0) > BUDGET);
        headers.remove("Retry-After");
        headers.insert("X-RateLimit-Reset", "1".parse().unwrap());
        assert_eq!(delay(&headers, "", 0), Duration::ZERO);
    }
    #[test]
    fn accounts_and_endpoints_are_isolated() {
        let a = shared("http://policy-test", "a".into());
        assert!(Arc::ptr_eq(&a, &shared("http://policy-test", "a".into())));
        assert!(!Arc::ptr_eq(&a, &shared("http://policy-test", "b".into())));
        assert!(!Arc::ptr_eq(
            &a,
            &shared("http://other-policy-test", "a".into())
        ));
    }
}
