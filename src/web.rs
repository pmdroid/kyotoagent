use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::Url;

use crate::tools::OUTPUT_LIMIT;

const REDIRECT_LIMIT: u8 = 5;
const TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT: &str = "text/html, text/plain, text/markdown, application/json";

#[derive(Clone, Copy)]
pub(crate) struct Config {
    pub allow_loopback: bool,
    pub resolve: fn(&str, u16) -> Result<Vec<IpAddr>, String>,
    pub transport: Transport,
}

#[derive(Clone, Copy)]
pub(crate) enum Transport {
    Network,
    #[cfg(test)]
    Fixed(fn(&str) -> FixedReply),
}

#[cfg(test)]
pub(crate) struct FixedReply {
    pub status: u16,
    pub location: Option<String>,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub(crate) struct Page {
    pub final_url: String,
    pub text: String,
}

#[derive(Debug)]
pub(crate) enum Error {
    BadUrl(String),
    Scheme(String),
    Userinfo,
    Blocked { url: String, address: String },
    Resolve { host: String, detail: String },
    TooManyRedirects { url: String },
    Status { url: String, status: u16 },
    Redirect { url: String },
    Http { url: String, detail: String },
    Limit { bytes: u64 },
    Convert { detail: String },
    Client(String),
    Search(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadUrl(url) => write!(f, "{url} is not a URL that can be fetched"),
            Error::Scheme(scheme) => write!(f, "{scheme} is not http or https"),
            Error::Userinfo => write!(f, "a URL with a username or password is refused"),
            Error::Blocked { url, address } => {
                write!(f, "{url} is refused: {address} is not a public address")
            }
            Error::Resolve { host, detail } => write!(f, "could not resolve {host}: {detail}"),
            Error::TooManyRedirects { url } => {
                write!(f, "{url} redirected more than {REDIRECT_LIMIT} times")
            }
            Error::Status { url, status } => write!(f, "{url} returned {status}"),
            Error::Redirect { url } => write!(f, "{url} redirected without a usable location"),
            Error::Http { url, detail } => write!(f, "could not fetch {url}: {detail}"),
            Error::Limit { bytes } => {
                write!(
                    f,
                    "{bytes} bytes is over the {OUTPUT_LIMIT} byte fetch limit"
                )
            }
            Error::Convert { detail } => write!(f, "could not read the page as markdown: {detail}"),
            Error::Client(detail) => write!(f, "could not prepare a fetch: {detail}"),
            Error::Search(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) fn production() -> Config {
    Config {
        allow_loopback: false,
        resolve: system_resolve,
        transport: Transport::Network,
    }
}

#[cfg(test)]
pub(crate) fn loopback_ok() -> Config {
    Config {
        allow_loopback: true,
        resolve: system_resolve,
        transport: Transport::Network,
    }
}

pub(crate) fn cap(asked: Option<u64>) -> Result<usize, Error> {
    let bytes = asked.unwrap_or(OUTPUT_LIMIT as u64);
    if bytes == 0 || bytes > OUTPUT_LIMIT as u64 {
        return Err(Error::Limit { bytes });
    }
    Ok(bytes as usize)
}

pub(crate) fn parse(raw: &str) -> Result<Url, Error> {
    let url = Url::parse(raw).map_err(|_| Error::BadUrl(raw.to_string()))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(Error::Scheme(other.to_string())),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Userinfo);
    }
    if url.host_str().is_none() {
        return Err(Error::BadUrl(raw.to_string()));
    }
    Ok(url)
}

pub(crate) fn origin(url: &Url) -> Result<String, Error> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::BadUrl(url.to_string()))?;
    let scheme = url.scheme();
    match url.port() {
        Some(port) => Ok(format!("{scheme}://{host}:{port}")),
        None => Ok(format!("{scheme}://{host}")),
    }
}

pub(crate) fn ensure_public(url: &Url, cfg: &Config) -> Result<(), Error> {
    for address in addresses(url, cfg)? {
        if blocked(address, cfg.allow_loopback) {
            return Err(Error::Blocked {
                url: url.to_string(),
                address: address.to_string(),
            });
        }
    }
    Ok(())
}

pub(crate) fn fetch(start: &Url, max_bytes: usize, cfg: &Config) -> Result<Page, Error> {
    let client = match cfg.transport {
        Transport::Network => Some(client()?),
        #[cfg(test)]
        Transport::Fixed(_) => None,
    };
    let mut url = start.clone();
    let mut redirects = 0u8;
    loop {
        ensure_public(&url, cfg)?;
        let raw = match cfg.transport {
            Transport::Network => network_get(client.as_ref().expect("a client"), &url, max_bytes)?,
            #[cfg(test)]
            Transport::Fixed(get) => fixed_get(&url, get, max_bytes),
        };
        if is_redirect(raw.status) {
            if redirects == REDIRECT_LIMIT {
                return Err(Error::TooManyRedirects {
                    url: url.to_string(),
                });
            }
            let Some(location) = raw.location else {
                return Err(Error::Redirect {
                    url: url.to_string(),
                });
            };
            url = url
                .join(&location)
                .map_err(|_| Error::Redirect { url: location })?;
            redirects += 1;
            continue;
        }
        if !(200..300).contains(&raw.status) {
            return Err(Error::Status {
                url: url.to_string(),
                status: raw.status,
            });
        }
        return present(&url, raw, max_bytes);
    }
}

struct Raw {
    status: u16,
    location: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
    truncated: bool,
}

fn client() -> Result<reqwest::blocking::Client, Error> {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .user_agent("kyotoagent")
        .no_proxy()
        .build()
        .map_err(|error| Error::Client(error.to_string()))
}

fn network_get(
    client: &reqwest::blocking::Client,
    url: &Url,
    max_bytes: usize,
) -> Result<Raw, Error> {
    let response = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, ACCEPT)
        .send()
        .map_err(|error| Error::Http {
            url: url.to_string(),
            detail: error.to_string(),
        })?;
    let status = response.status().as_u16();
    let location = header_string(&response, reqwest::header::LOCATION);
    let content_type = header_string(&response, reqwest::header::CONTENT_TYPE);
    let (body, truncated) = read_limited(response, max_bytes, url)?;
    Ok(Raw {
        status,
        location,
        content_type,
        body,
        truncated,
    })
}

#[cfg(test)]
fn fixed_get(url: &Url, get: fn(&str) -> FixedReply, max_bytes: usize) -> Raw {
    let reply = get(url.as_str());
    let truncated = reply.body.len() > max_bytes;
    let mut body = reply.body;
    if truncated {
        body.truncate(max_bytes);
    }
    Raw {
        status: reply.status,
        location: reply.location,
        content_type: Some(reply.content_type),
        body,
        truncated,
    }
}

fn header_string(
    response: &reqwest::blocking::Response,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    response
        .headers()
        .get(name)?
        .to_str()
        .ok()
        .map(str::to_string)
}

fn read_limited(
    response: reqwest::blocking::Response,
    max_bytes: usize,
    url: &Url,
) -> Result<(Vec<u8>, bool), Error> {
    let mut buf = Vec::new();
    response
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|error| Error::Http {
            url: url.to_string(),
            detail: error.to_string(),
        })?;
    let truncated = buf.len() > max_bytes;
    if truncated {
        buf.truncate(max_bytes);
    }
    Ok((buf, truncated))
}

fn present(url: &Url, raw: Raw, max_bytes: usize) -> Result<Page, Error> {
    let media = raw
        .content_type
        .as_deref()
        .map(media_type)
        .unwrap_or_default();
    let final_url = url.as_str();
    let (body, truncated) = match media.as_str() {
        "text/html" => {
            let html = String::from_utf8_lossy(&raw.body);
            let markdown = htmd::convert(&html).map_err(|error| Error::Convert {
                detail: error.to_string(),
            })?;
            cap_text(markdown, max_bytes, raw.truncated)
        }
        "text/plain" | "text/markdown" | "application/json" => cap_text(
            String::from_utf8_lossy(&raw.body).into_owned(),
            max_bytes,
            raw.truncated,
        ),
        other => {
            let line = if other.is_empty() {
                "Refused content type.".to_string()
            } else {
                format!("Refused content type {other}.")
            };
            return Ok(assemble(final_url, &line, false, max_bytes));
        }
    };
    Ok(assemble(final_url, &body, truncated, max_bytes))
}

fn cap_text(mut body: String, max_bytes: usize, already: bool) -> (String, bool) {
    if body.len() <= max_bytes {
        return (body, already);
    }
    let mut end = max_bytes;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    body.truncate(end);
    (body, true)
}

fn assemble(final_url: &str, body: &str, truncated: bool, max_bytes: usize) -> Page {
    let mut text = String::new();
    text.push_str(final_url);
    text.push_str("\n\n");
    text.push_str(body);
    if truncated {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("Truncated at {max_bytes} bytes."));
    }
    Page {
        final_url: final_url.to_string(),
        text,
    }
}

fn media_type(header: &str) -> String {
    header
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn addresses(url: &Url, cfg: &Config) -> Result<Vec<IpAddr>, Error> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::BadUrl(url.to_string()))?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let ips = (cfg.resolve)(host, port).map_err(|detail| Error::Resolve {
        host: host.to_string(),
        detail,
    })?;
    if ips.is_empty() {
        return Err(Error::Resolve {
            host: host.to_string(),
            detail: "no addresses".to_string(),
        });
    }
    Ok(ips)
}

fn system_resolve(host: &str, port: u16) -> Result<Vec<IpAddr>, String> {
    use std::net::ToSocketAddrs;
    let ips: Vec<IpAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .map(|socket| socket.ip())
        .collect();
    if ips.is_empty() {
        return Err("no addresses".to_string());
    }
    Ok(ips)
}

fn blocked(address: IpAddr, allow_loopback: bool) -> bool {
    let address = match address {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        other => other,
    };
    match address {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            let loopback = a == 127;
            if loopback && allow_loopback {
                return false;
            }
            loopback
                || a == 10
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254)
        }
        IpAddr::V6(ip) => {
            if ip.is_loopback() {
                return !allow_loopback;
            }
            let first = ip.segments()[0];
            let link_local = (first & 0xffc0) == 0xfe80;
            let unique_local = (first & 0xfe00) == 0xfc00;
            link_local || unique_local
        }
    }
}

pub(crate) const EXA_ENV: &str = "EXA_API_KEY";
pub(crate) const FIRECRAWL_ENV: &str = "FIRECRAWL_API_KEY";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vendor {
    Exa,
    Firecrawl,
}

impl Vendor {
    fn label(self) -> &'static str {
        match self {
            Vendor::Exa => "Exa",
            Vendor::Firecrawl => "Firecrawl",
        }
    }

    fn endpoint(self) -> String {
        match self {
            Vendor::Exa => format!("{}{}{}api.exa.ai/search", "https:", "/", "/"),
            Vendor::Firecrawl => {
                format!("{}{}{}api.firecrawl.dev/v2/search", "https:", "/", "/")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchEndpoints {
    pub exa: String,
    pub firecrawl: String,
    pub allow_loopback: bool,
}

impl SearchEndpoints {
    pub(crate) fn production() -> SearchEndpoints {
        SearchEndpoints {
            exa: Vendor::Exa.endpoint(),
            firecrawl: Vendor::Firecrawl.endpoint(),
            allow_loopback: false,
        }
    }
}

pub(crate) fn pick_search(
    exa_env: &str,
    firecrawl_env: &str,
    read: impl Fn(&str) -> Option<String>,
) -> Option<(Vendor, String)> {
    if let Some(key) = nonempty(read(exa_env)) {
        return Some((Vendor::Exa, key));
    }
    nonempty(read(firecrawl_env)).map(|key| (Vendor::Firecrawl, key))
}

fn nonempty(value: Option<String>) -> Option<String> {
    let text = value?.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

pub(crate) fn clamp_results(value: Option<u64>) -> u32 {
    match value {
        None => 5,
        Some(count) => count.clamp(1, 10) as u32,
    }
}

pub(crate) fn search(
    vendor: Vendor,
    key: &str,
    endpoint: &str,
    allow_loopback: bool,
    query: &str,
    num_results: u32,
) -> Result<String, Error> {
    if key.trim().is_empty() {
        return Err(Error::Search("No search key is set.".into()));
    }
    let url = parse(endpoint)?;
    let cfg = Config {
        allow_loopback,
        resolve: system_resolve,
        transport: Transport::Network,
    };
    ensure_public(&url, &cfg)?;
    let body = request_body(vendor, query, num_results);
    let client = client()?;
    let raw = network_post(&client, &url, vendor, key, &body)?;
    if !(200..300).contains(&raw.status) {
        return Err(Error::Search(format!(
            "{} search failed: {}",
            vendor.label(),
            raw.status
        )));
    }
    let payload = String::from_utf8_lossy(&raw.body);
    let rendered = render(vendor, &payload).map_err(|_| {
        Error::Search(format!(
            "{} returned a response that could not be read.",
            vendor.label()
        ))
    })?;
    Ok(cap_result(rendered))
}

fn request_body(vendor: Vendor, query: &str, num_results: u32) -> String {
    match vendor {
        Vendor::Exa => serde_json::json!({
            "query": query,
            "type": "auto",
            "numResults": num_results,
            "contents": { "highlights": true }
        })
        .to_string(),
        Vendor::Firecrawl => serde_json::json!({
            "query": query,
            "limit": num_results
        })
        .to_string(),
    }
}

fn network_post(
    client: &reqwest::blocking::Client,
    url: &Url,
    vendor: Vendor,
    key: &str,
    body: &str,
) -> Result<Raw, Error> {
    let response = client
        .post(url.clone())
        .bearer_auth(key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .body(body.to_string())
        .send()
        .map_err(|error| {
            Error::Search(format!(
                "{} search failed: {}",
                vendor.label(),
                one_line(&error.to_string())
            ))
        })?;
    let status = response.status().as_u16();
    let (bytes, _) = read_limited(response, OUTPUT_LIMIT, url)?;
    Ok(Raw {
        status,
        location: None,
        content_type: None,
        body: bytes,
        truncated: false,
    })
}

fn render(vendor: Vendor, body: &str) -> Result<String, ()> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| ())?;
    let hits = match vendor {
        Vendor::Exa => exa_hits(&value)?,
        Vendor::Firecrawl => firecrawl_hits(&value)?,
    };
    Ok(format_hits(vendor, &hits))
}

struct Hit {
    title: String,
    url: String,
    snippet: String,
}

fn exa_hits(value: &serde_json::Value) -> Result<Vec<Hit>, ()> {
    let results = value
        .get("results")
        .and_then(|item| item.as_array())
        .ok_or(())?;
    Ok(results.iter().map(exa_hit).collect())
}

fn exa_hit(value: &serde_json::Value) -> Hit {
    let snippet = first_highlight(value.get("highlights"))
        .or_else(|| short_text(value.get("text")))
        .unwrap_or_default();
    Hit {
        title: text_field(value, "title"),
        url: text_field(value, "url"),
        snippet,
    }
}

fn firecrawl_hits(value: &serde_json::Value) -> Result<Vec<Hit>, ()> {
    let web = value
        .get("data")
        .and_then(|data| data.get("web"))
        .and_then(|web| web.as_array())
        .ok_or(())?;
    Ok(web
        .iter()
        .map(|item| Hit {
            title: text_field(item, "title"),
            url: text_field(item, "url"),
            snippet: short_text(item.get("description")).unwrap_or_default(),
        })
        .collect())
}

fn text_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn first_highlight(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(text) => {
            short_text(Some(&serde_json::Value::String(text.clone())))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|item| short_text(Some(item))),
        _ => None,
    }
}

fn short_text(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = one_line.chars();
    let short: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        Some(format!("{short}..."))
    } else {
        Some(short)
    }
}

fn format_hits(vendor: Vendor, hits: &[Hit]) -> String {
    let mut out = vendor.label().to_string();
    if hits.is_empty() {
        out.push_str("\n\nNo results.");
        return out;
    }
    for hit in hits {
        out.push_str("\n\n");
        out.push_str(&hit.title);
        out.push('\n');
        out.push_str(&hit.url);
        if !hit.snippet.is_empty() {
            out.push('\n');
            out.push_str(&hit.snippet);
        }
    }
    out
}

fn cap_result(text: String) -> String {
    if text.len() <= OUTPUT_LIMIT {
        return text;
    }
    let note = format!("\nTruncated at {OUTPUT_LIMIT} bytes.");
    let mut keep = OUTPUT_LIMIT.saturating_sub(note.len());
    while keep > 0 && !text.is_char_boundary(keep) {
        keep -= 1;
    }
    let mut out = text[..keep].to_string();
    out.push_str(&note);
    if out.len() > OUTPUT_LIMIT {
        let mut end = OUTPUT_LIMIT;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use std::sync::OnceLock;

    use crate::config::Config as AgentConfig;
    use crate::events::{Decision, EventKind, PermissionAnswerBody, PermissionBody};
    use crate::permit::Answer;
    use crate::session::{Session, SessionMeta};
    use crate::tools::{Fetched, Tools, OUTPUT_LIMIT};
    use crate::turn::{tool_definitions, tool_definitions_for};
    use crate::view::{self, CardKind};

    const AT: &str = "2026-09-29T00:00:00.000Z";

    struct Fixture {
        tools: Tools,
        session: Session,
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-fetch-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let root = fs::canonicalize(&root).expect("the root resolves");
        let workspace = root.join("w");
        let session = Session::at(&root.join("session"));
        session
            .create(&SessionMeta::new("91bc", &workspace, "gpt", AT))
            .expect("the session is created");
        let tools = Tools::at(&session).expect("the tools are built");
        Fixture {
            tools,
            session,
            root,
        }
    }

    impl Fixture {
        fn permissions(&self) -> Vec<PermissionBody> {
            self.session
                .events()
                .expect("the log reads")
                .into_iter()
                .filter(|event| event.kind == EventKind::Permission)
                .map(|event| event.body_as().expect("a permission body"))
                .collect()
        }

        fn decisions(&self) -> Vec<Decision> {
            self.session
                .events()
                .expect("the log reads")
                .into_iter()
                .filter(|event| event.kind == EventKind::PermissionAnswer)
                .map(|event| {
                    event
                        .body_as::<PermissionAnswerBody>()
                        .expect("an answer")
                        .decision
                })
                .collect()
        }

        fn wait_for_permissions(&self, count: usize) -> Vec<PermissionBody> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let cards = self.permissions();
                if cards.len() >= count {
                    return cards;
                }
                assert!(
                    Instant::now() < deadline,
                    "only {} of {count} permissions came up",
                    cards.len()
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    struct Site {
        port: u16,
        hits: Arc<AtomicUsize>,
        seen: Arc<Mutex<String>>,
    }

    fn serve(handler: impl Fn(&str) -> Vec<u8> + Send + 'static) -> Site {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(String::new()));
        let hits2 = hits.clone();
        let seen2 = seen.clone();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port");
        let port = listener.local_addr().expect("the port").port();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let raw = read_request(&mut stream);
                *seen2.lock().expect("seen") = raw.clone();
                let path = raw.split_whitespace().nth(1).unwrap_or("/");
                let body = handler(path);
                let _ = stream.write_all(&body);
            }
        });
        Site { port, hits, seen }
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(header_end) = header_end(&buf) {
                        let header = String::from_utf8_lossy(&buf[..header_end]);
                        let length = content_length(&header);
                        if buf.len() - header_end >= length || buf.len() > 8192 + length {
                            break;
                        }
                    } else if buf.len() > 8192 {
                        break;
                    }
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|at| at + 4)
    }

    fn content_length(header: &str) -> usize {
        header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    fn response(status: u16, reason: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
        for (name, value) in headers {
            out.extend(format!("{name}: {value}\r\n").into_bytes());
        }
        out.extend(
            format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes(),
        );
        out.extend_from_slice(body);
        out
    }

    fn html(body: &str) -> Vec<u8> {
        response(
            200,
            "OK",
            &[("Content-Type", "text/html; charset=utf-8")],
            body.as_bytes(),
        )
    }

    #[test]
    fn an_origin_drops_the_default_port_and_keeps_any_other() {
        let docs = parse("https://docs.example/api").expect("docs");
        assert_eq!(origin(&docs).expect("origin"), "https://docs.example");
        let explicit = parse("https://docs.example:443/api").expect("explicit");
        assert_eq!(origin(&explicit).expect("origin"), "https://docs.example");
        let other_port = parse("https://docs.example:444/api").expect("port");
        assert_eq!(
            origin(&other_port).expect("origin"),
            "https://docs.example:444"
        );
        let http = parse("http://docs.example:80/api").expect("http");
        assert_eq!(origin(&http).expect("origin"), "http://docs.example");
    }

    #[test]
    fn private_addresses_are_refused_and_a_public_one_is_not() {
        let cfg = production();
        for raw in [
            "http://127.0.0.1/",
            "http://127.0.0.2/",
            "http://10.1.2.3/",
            "http://192.168.0.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/",
            "http://169.254.1.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:10.0.0.1]/",
        ] {
            let url = parse(raw).expect(raw);
            let error = ensure_public(&url, &cfg).expect_err(raw);
            assert!(error.to_string().contains("refused"), "{raw}: {error}");
        }
        let url = parse("http://172.32.0.1/").expect("outside rfc1918");
        ensure_public(&url, &cfg).expect("172.32 is public enough");
        let url = parse("http://8.8.8.8/").expect("public");
        ensure_public(&url, &cfg).expect("a public address");
        let url = parse("http://127.0.0.1/").expect("loopback");
        ensure_public(&url, &loopback_ok()).expect("tests may use loopback");
        let url = parse("http://10.0.0.1/").expect("rfc1918");
        assert!(ensure_public(&url, &loopback_ok()).is_err());
    }

    #[test]
    fn a_username_or_a_bad_scheme_is_refused_before_a_card() {
        let f = fixture("syntax");
        for raw in ["http://user:pass@example.com/", "ftp://example.com/a"] {
            let error = f.tools.web_fetch("t1", raw, None).expect_err(raw);
            assert!(!error.to_string().is_empty());
        }
        assert!(f.permissions().is_empty());
    }

    #[test]
    fn loopback_and_the_metadata_address_never_ask_and_never_connect() {
        let f = fixture("ssrf");
        let site = serve(|_| html("<h1>no</h1>"));
        let local = format!("http://127.0.0.1:{}/", site.port);
        let started = Instant::now();
        let error = f.tools.web_fetch("t1", &local, None).expect_err("loopback");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "refused before connect: {error}"
        );
        assert!(error.to_string().contains("refused"), "{error}");
        assert_eq!(site.hits.load(Ordering::SeqCst), 0);
        for raw in [
            "http://127.0.0.1/",
            "http://169.254.169.254/",
            "http://localhost/",
        ] {
            let started = Instant::now();
            let error = f.tools.web_fetch("t1", raw, None).expect_err(raw);
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{raw} took too long: {error}"
            );
            assert!(error.to_string().contains("refused"), "{raw}: {error}");
        }
        assert!(f.permissions().is_empty());
    }

    #[test]
    fn html_comes_back_as_markdown_after_a_permission_and_a_decision() {
        let f = fixture("html");
        let site = serve(|path| {
            if path.starts_with("/start") {
                response(302, "Found", &[("Location", "/page")], b"")
            } else {
                html("<h1>Fetched heading</h1><p>Hello</p>")
            }
        });
        let url = format!("http://127.0.0.1:{}/start", site.port);
        let tools = f.tools.clone();
        let asked = url.clone();
        let turn = thread::spawn(move || tools.web_fetch_with("t1", &asked, None, &loopback_ok()));
        let card = f.wait_for_permissions(1).remove(0);
        assert_eq!(card.action, format!("Fetch {url}"));
        assert_eq!(card.path.as_deref(), Some(url.as_str()));
        assert!(card.argv.is_none());
        assert!(card.diff.is_none());
        let cards = view::read(f.session.dir()).expect("the view").cards;
        assert!(cards.iter().any(|card| card.kind == CardKind::Permission));
        let json = serde_json::to_string(&cards).expect("cards");
        assert!(!json.contains("web_fetch"), "{json}");
        assert!(json.contains("\"decision\":null"), "{json}");
        assert!(json.contains(&url), "{json}");
        f.tools.gate().answer(Answer::allow_once()).expect("allow");
        let fetched = turn.join().expect("the turn").expect("the fetch");
        assert!(!fetched.denied);
        let final_url = format!("http://127.0.0.1:{}/page", site.port);
        assert!(fetched.text.starts_with(&final_url), "{}", fetched.text);
        assert!(fetched.text.contains("Fetched heading"), "{}", fetched.text);
        assert!(fetched.text.contains('#'), "{}", fetched.text);
        assert_eq!(f.decisions(), vec![Decision::AllowOnce]);
        assert!(f
            .session
            .meta()
            .expect("meta")
            .allow
            .fetch_origins
            .is_empty());
        let seen = site.seen.lock().expect("seen").clone();
        assert!(
            seen.to_ascii_lowercase().contains("user-agent: kyotoagent"),
            "{seen}"
        );
        assert!(seen.to_ascii_lowercase().contains("text/html"), "{seen}");
        let cards = view::read(f.session.dir()).expect("the view").cards;
        let json = serde_json::to_string(&cards).expect("cards");
        assert!(!json.contains("web_fetch"), "{json}");
    }

    #[test]
    fn a_large_page_returns_the_cap_and_a_truncated_note() {
        let f = fixture("trunc");
        let body = vec![b'a'; 200 * 1024];
        let site = serve(move |_| response(200, "OK", &[("Content-Type", "text/plain")], &body));
        f.tools.gate().queue(Answer::allow_once());
        let url = format!("http://127.0.0.1:{}/big", site.port);
        let fetched = f
            .tools
            .web_fetch_with("t1", &url, None, &loopback_ok())
            .expect("the fetch");
        let note = format!("Truncated at {OUTPUT_LIMIT} bytes.");
        assert!(fetched.text.starts_with(&url), "{}", fetched.text);
        assert!(fetched.text.ends_with(&note), "{}", fetched.text);
        let prefix = format!("{url}\n\n");
        let middle = fetched
            .text
            .strip_prefix(&prefix)
            .expect("the url")
            .strip_suffix(&format!("\n{note}"))
            .expect("the note");
        assert_eq!(middle.len(), OUTPUT_LIMIT);
        assert!(middle.bytes().all(|byte| byte == b'a'));
    }

    #[test]
    fn a_redirect_to_a_private_address_is_refused() {
        let f = fixture("redir");
        let site = serve(|_| {
            response(
                302,
                "Found",
                &[("Location", "http://169.254.169.254/latest")],
                b"",
            )
        });
        f.tools.gate().queue(Answer::allow_once());
        let url = format!("http://127.0.0.1:{}/go", site.port);
        let started = Instant::now();
        let error = f
            .tools
            .web_fetch_with("t1", &url, None, &loopback_ok())
            .expect_err("the redirect");
        assert!(started.elapsed() < Duration::from_secs(2), "{error}");
        assert!(error.to_string().contains("169.254.169.254"), "{error}");
        assert!(error.to_string().contains("refused"), "{error}");
        assert_eq!(site.hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_deny_fetches_nothing() {
        let f = fixture("deny");
        let site = serve(|_| html("<h1>secret</h1>"));
        f.tools.gate().queue(Answer::deny());
        let url = format!("http://127.0.0.1:{}/", site.port);
        let fetched = f
            .tools
            .web_fetch_with("t1", &url, None, &loopback_ok())
            .expect("a denial is a result");
        assert!(fetched.denied);
        assert_eq!(
            fetched.summary(),
            format!("Not allowed, so {url} was not fetched.")
        );
        assert_eq!(site.hits.load(Ordering::SeqCst), 0);
        assert_eq!(f.decisions(), vec![Decision::Deny]);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(site.hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn allow_session_remembers_the_origin_and_a_different_host_asks_again() {
        let f = fixture("origin");
        let cfg = Config {
            allow_loopback: false,
            resolve: resolve_examples,
            transport: Transport::Fixed(scripted_docs),
        };
        f.tools.gate().queue(Answer::allow_session());
        let first = f
            .tools
            .web_fetch_with("t1", "https://docs.example/guide", None, &cfg)
            .expect("the first fetch");
        assert!(first.text.contains("Docs"), "{}", first.text);
        assert!(
            first.text.starts_with("https://docs.example/guide"),
            "{}",
            first.text
        );
        assert_eq!(f.permissions().len(), 1);
        assert_eq!(
            f.session.meta().expect("meta").allow.fetch_origins,
            vec!["https://docs.example".to_string()]
        );
        let second = f
            .tools
            .web_fetch_with("t1", "https://docs.example/api", None, &cfg)
            .expect("the second fetch");
        assert!(second.text.contains("Docs"));
        assert_eq!(f.permissions().len(), 1, "the same origin skips the card");
        let tools = f.tools.clone();
        let turn =
            thread::spawn(move || tools.web_fetch_with("t1", "https://other.example/", None, &cfg));
        let cards = f.wait_for_permissions(2);
        assert_eq!(cards[1].action, "Fetch https://other.example/");
        f.tools.gate().answer(Answer::deny()).expect("deny");
        let fetched: Fetched = turn.join().expect("the turn").expect("the result");
        assert!(fetched.denied);
    }

    #[test]
    fn allow_once_does_not_remember_the_origin() {
        let f = fixture("once");
        let site = serve(|_| html("<h1>Once</h1>"));
        let url = format!("http://127.0.0.1:{}/one", site.port);
        f.tools.gate().queue(Answer::allow_once());
        f.tools
            .web_fetch_with("t1", &url, None, &loopback_ok())
            .expect("the first");
        assert!(f
            .session
            .meta()
            .expect("meta")
            .allow
            .fetch_origins
            .is_empty());
        let tools = f.tools.clone();
        let again = url.clone();
        let turn = thread::spawn(move || tools.web_fetch_with("t1", &again, None, &loopback_ok()));
        let cards = f.wait_for_permissions(2);
        assert_eq!(cards[1].path.as_deref(), Some(url.as_str()));
        f.tools.gate().answer(Answer::deny()).expect("deny");
        let _ = turn.join().expect("the turn");
    }

    #[test]
    fn yolo_answers_a_fetch_and_the_request_goes_out() {
        let f = fixture("yolo");
        f.session
            .update(|meta| {
                meta.yolo = true;
                true
            })
            .expect("yolo");
        let site = serve(|_| html("<h1>Yolo</h1>"));
        let url = format!("http://127.0.0.1:{}/y", site.port);
        let fetched = f
            .tools
            .web_fetch_with("t1", &url, None, &loopback_ok())
            .expect("yolo fetches");
        assert!(fetched.text.contains("Yolo"), "{}", fetched.text);
        assert_eq!(f.decisions(), vec![Decision::AllowOnce]);
        assert_eq!(site.hits.load(Ordering::SeqCst), 1);
        assert!(f
            .session
            .meta()
            .expect("meta")
            .allow
            .fetch_origins
            .is_empty());
    }

    #[test]
    fn max_bytes_cannot_exceed_the_output_limit() {
        let f = fixture("limit");
        let error = f
            .tools
            .web_fetch("t1", "https://docs.example/", Some(OUTPUT_LIMIT as u64 + 1))
            .expect_err("too big");
        assert!(error.to_string().contains("over"), "{error}");
        assert!(f.permissions().is_empty());
    }

    struct EnvGuard {
        exa: Option<String>,
        firecrawl: Option<String>,
    }

    struct HeldEnv {
        env: EnvGuard,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn hold_env(exa: Option<&str>, firecrawl: Option<&str>) -> HeldEnv {
        let lock = env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let env = EnvGuard {
            exa: std::env::var(EXA_ENV).ok(),
            firecrawl: std::env::var(FIRECRAWL_ENV).ok(),
        };
        assign_env(EXA_ENV, exa);
        assign_env(FIRECRAWL_ENV, firecrawl);
        HeldEnv { env, _lock: lock }
    }

    impl Drop for HeldEnv {
        fn drop(&mut self) {
            assign_env(EXA_ENV, self.env.exa.as_deref());
            assign_env(FIRECRAWL_ENV, self.env.firecrawl.as_deref());
        }
    }

    fn assign_env(name: &str, value: Option<&str>) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }

    fn search_site(port: u16) -> SearchEndpoints {
        let root = format!("http://127.0.0.1:{port}");
        SearchEndpoints {
            exa: format!("{root}/exa"),
            firecrawl: format!("{root}/firecrawl"),
            allow_loopback: true,
        }
    }

    fn json_body(status: u16, body: &str) -> Vec<u8> {
        let reason = if status == 200 { "OK" } else { "Error" };
        response(
            status,
            reason,
            &[("Content-Type", "application/json")],
            body.as_bytes(),
        )
    }

    fn exa_fixture() -> &'static str {
        r#"{"results":[{"title":"Kyoto Agent","url":"https://example.com/kyoto","highlights":["a quiet agent"],"text":"unused long text"}]}"#
    }

    fn firecrawl_fixture() -> &'static str {
        r#"{"data":{"web":[{"title":"Fire Title","url":"https://example.com/fire","description":"a firecrawl hit"}]}}"#
    }

    fn request_body_json(raw: &str) -> serde_json::Value {
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
        serde_json::from_str(body.trim()).unwrap_or_else(|error| panic!("{error}: {raw}"))
    }

    fn names(config: &AgentConfig) -> Vec<String> {
        tool_definitions(config, false)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn web_search_is_offered_only_when_the_vendor_and_the_profile_allow_it() {
        let config = AgentConfig::from_toml(
            r#"
base_url = "http://127.0.0.1:9/v1"
model = "m"

[profiles.review]
tools = ["read_file", "web_fetch", "ask", "finish"]

[profiles.search]
tools = ["web_search", "finish"]

[profiles.open]
skills = ["skill-a"]
"#,
        )
        .expect("the profile config parses");
        let offered = |profile: Option<&str>| {
            tool_definitions_for(&config, false, profile)
                .into_iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>()
        };
        {
            let _env = hold_env(Some("exa-profile-key"), None);
            assert!(offered(None).iter().any(|name| name == "web_search"));
            assert!(!offered(Some("review"))
                .iter()
                .any(|name| name == "web_search"));
            assert!(offered(Some("search"))
                .iter()
                .any(|name| name == "web_search"));
            assert!(offered(Some("open"))
                .iter()
                .any(|name| name == "web_search"));
            assert!(offered(Some("open")).iter().any(|name| name == "run"));
        }
        let _env = hold_env(None, None);
        assert!(!offered(Some("search"))
            .iter()
            .any(|name| name == "web_search"));
        assert!(!offered(None).iter().any(|name| name == "web_search"));
    }

    #[test]
    fn no_search_key_omits_the_tool() {
        let _env = hold_env(None, None);
        let config = AgentConfig::default();
        assert!(!names(&config).iter().any(|name| name == "web_search"));
    }

    #[test]
    fn an_exa_key_lists_the_tool_and_projects_the_fixture() {
        let _env = hold_env(Some("exa-test-key"), None);
        let config = AgentConfig::default();
        assert!(names(&config).iter().any(|name| name == "web_search"));
        let f = fixture("exa");
        let site = serve(|path| {
            assert!(path.starts_with("/exa"), "{path}");
            json_body(200, exa_fixture())
        });
        f.tools.gate().queue(Answer::allow_once());
        let found = f
            .tools
            .web_search_with(
                "t1",
                "kyoto agent",
                None,
                EXA_ENV,
                FIRECRAWL_ENV,
                &search_site(site.port),
            )
            .expect("search");
        assert!(found.text.contains("Kyoto Agent"), "{}", found.text);
        assert!(
            found.text.contains("https://example.com/kyoto"),
            "{}",
            found.text
        );
        assert!(found.text.contains("a quiet agent"), "{}", found.text);
        assert!(!found.text.contains("unused long text"), "{}", found.text);
        assert!(!found.text.contains("exa-test-key"), "{}", found.text);
        assert!(found.text.starts_with("Exa"), "{}", found.text);
        let raw = site.seen.lock().expect("seen").clone();
        let body = request_body_json(&raw);
        assert_eq!(body["query"], "kyoto agent");
        assert_eq!(body["type"], "auto");
        assert_eq!(body["numResults"], 5);
        assert_eq!(body["contents"]["highlights"], true);
        assert!(raw
            .to_ascii_lowercase()
            .contains("authorization: bearer exa-test-key"));
        assert!(!raw.contains("scrapeOptions"), "{raw}");
        let cards = view::read(f.session.dir()).expect("view").cards;
        let json = serde_json::to_string(&cards).expect("cards");
        assert!(!json.contains("web_search"), "{json}");
    }

    #[test]
    fn only_a_firecrawl_key_uses_that_fixture() {
        let _env = hold_env(None, Some("fire-test-key"));
        let config = AgentConfig::default();
        assert_eq!(config.search_vendor(), Some(Vendor::Firecrawl));
        let f = fixture("fire");
        let site = serve(|path| {
            assert!(path.starts_with("/firecrawl"), "{path}");
            json_body(200, firecrawl_fixture())
        });
        f.tools.gate().queue(Answer::allow_once());
        let found = f
            .tools
            .web_search_with(
                "t1",
                "fire query",
                Some(3),
                EXA_ENV,
                FIRECRAWL_ENV,
                &search_site(site.port),
            )
            .expect("search");
        assert!(found.text.contains("Fire Title"), "{}", found.text);
        assert!(
            found.text.contains("https://example.com/fire"),
            "{}",
            found.text
        );
        assert!(found.text.contains("a firecrawl hit"), "{}", found.text);
        assert!(found.text.starts_with("Firecrawl"), "{}", found.text);
        assert!(!found.text.contains("fire-test-key"), "{}", found.text);
        let raw = site.seen.lock().expect("seen").clone();
        let body = request_body_json(&raw);
        assert_eq!(body["query"], "fire query");
        assert_eq!(body["limit"], 3);
        assert!(body.get("scrapeOptions").is_none(), "{body}");
        assert!(body.get("numResults").is_none(), "{body}");
    }

    #[test]
    fn both_keys_prefer_exa() {
        let _env = hold_env(Some("exa-both"), Some("fire-both"));
        let config = AgentConfig::default();
        assert_eq!(config.search_vendor(), Some(Vendor::Exa));
        assert!(names(&config).iter().any(|name| name == "web_search"));
        let f = fixture("both");
        let site = serve(|path| {
            assert!(path.starts_with("/exa"), "{path}");
            json_body(200, exa_fixture())
        });
        f.tools.gate().queue(Answer::allow_once());
        let found = f
            .tools
            .web_search_with(
                "t1",
                "both",
                Some(99),
                EXA_ENV,
                FIRECRAWL_ENV,
                &search_site(site.port),
            )
            .expect("search");
        assert!(found.text.contains("Kyoto Agent"), "{}", found.text);
        let body = request_body_json(&site.seen.lock().expect("seen"));
        assert_eq!(body["numResults"], 10);
        assert_eq!(body["type"], "auto");
        assert!(f
            .session
            .meta()
            .expect("meta")
            .allow
            .fetch_origins
            .is_empty());
    }

    #[test]
    fn a_named_env_var_is_what_resolves() {
        let _env = hold_env(None, None);
        std::env::set_var("MY_EXA", "named-key");
        let config = AgentConfig::from_toml(
            "base_url = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\n\n[web]\nexa_api_key_env = \"MY_EXA\"\n",
        )
        .expect("toml");
        assert_eq!(config.search_vendor(), Some(Vendor::Exa));
        std::env::remove_var("MY_EXA");
        assert_eq!(config.search_vendor(), None);
    }

    #[test]
    fn a_search_401_is_one_line() {
        let _env = hold_env(Some("exa-test-key"), None);
        let f = fixture("unauth");
        let site = serve(|_| json_body(401, r#"{"error":"nope"}"#));
        f.tools.gate().queue(Answer::allow_once());
        let error = f
            .tools
            .web_search_with(
                "t1",
                "kyoto",
                None,
                EXA_ENV,
                FIRECRAWL_ENV,
                &search_site(site.port),
            )
            .expect_err("401");
        let message = error.to_string();
        assert_eq!(message, "Exa search failed: 401");
        assert!(!message.contains('\n'));
        assert!(!message.contains("exa-test-key"));
        assert_eq!(site.hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_missing_key_at_call_time_is_a_tool_error() {
        let _env = hold_env(None, None);
        let f = fixture("nokey");
        let site = serve(|_| json_body(200, exa_fixture()));
        f.tools.gate().queue(Answer::allow_once());
        let error = f
            .tools
            .web_search_with(
                "t1",
                "kyoto",
                None,
                EXA_ENV,
                FIRECRAWL_ENV,
                &search_site(site.port),
            )
            .expect_err("missing");
        assert_eq!(error.to_string(), "No search key is set.");
        assert_eq!(site.hits.load(Ordering::SeqCst), 0);
        assert_eq!(f.permissions().len(), 1);
        assert_eq!(f.decisions(), vec![Decision::AllowOnce]);
    }

    #[test]
    fn deny_searches_nothing() {
        let _env = hold_env(Some("exa-test-key"), None);
        let f = fixture("search-deny");
        let site = serve(|_| json_body(200, exa_fixture()));
        f.tools.gate().queue(Answer::deny());
        let found = f
            .tools
            .web_search_with(
                "t1",
                "hidden query",
                None,
                EXA_ENV,
                FIRECRAWL_ENV,
                &search_site(site.port),
            )
            .expect("denied");
        assert!(found.denied);
        assert!(found.text.contains("Not allowed"), "{}", found.text);
        assert!(found.text.contains("hidden query"), "{}", found.text);
        assert_eq!(site.hits.load(Ordering::SeqCst), 0);
        assert_eq!(f.permissions()[0].action, "Search the web");
        assert_eq!(f.permissions()[0].path.as_deref(), Some("hidden query"));
        assert!(!f.session.meta().expect("meta").allow.web_search);
        assert!(f
            .session
            .meta()
            .expect("meta")
            .allow
            .fetch_origins
            .is_empty());
    }

    #[test]
    fn allow_session_skips_the_card_on_the_second_query() {
        let _env = hold_env(Some("exa-test-key"), None);
        let f = fixture("session");
        let site = serve(|_| json_body(200, exa_fixture()));
        let endpoints = search_site(site.port);
        let tools = f.tools.clone();
        let ends = endpoints.clone();
        let turn = thread::spawn(move || {
            tools.web_search_with("t1", "first query", None, EXA_ENV, FIRECRAWL_ENV, &ends)
        });
        let card = f.wait_for_permissions(1).remove(0);
        assert_eq!(card.action, "Search the web");
        assert_eq!(card.path.as_deref(), Some("first query"));
        let cards = view::read(f.session.dir()).expect("view").cards;
        let json = serde_json::to_string(&cards).expect("cards");
        assert!(!json.contains("web_search"), "{json}");
        assert!(json.contains("first query"), "{json}");
        f.tools
            .gate()
            .answer(Answer::allow_session())
            .expect("allow");
        let found = turn.join().expect("join").expect("search");
        assert!(found.text.contains("Kyoto Agent"), "{}", found.text);
        let again = f
            .tools
            .web_search_with(
                "t1",
                "second query",
                None,
                EXA_ENV,
                FIRECRAWL_ENV,
                &endpoints,
            )
            .expect("second");
        assert!(
            again.text.contains("https://example.com/kyoto"),
            "{}",
            again.text
        );
        assert_eq!(f.permissions().len(), 1);
        assert_eq!(f.decisions(), vec![Decision::AllowSession]);
        assert_eq!(site.hits.load(Ordering::SeqCst), 2);
        let allow = f.session.meta().expect("meta").allow;
        assert!(allow.web_search);
        assert!(allow.fetch_origins.is_empty());
        let cards = view::read(f.session.dir()).expect("view").cards;
        let json = serde_json::to_string(&cards).expect("cards");
        assert!(!json.contains("web_search"), "{json}");
    }

    #[test]
    fn allow_once_asks_again_and_yolo_sends_the_search() {
        let _env = hold_env(Some("exa-test-key"), None);
        let f = fixture("search-once");
        let site = serve(|_| json_body(200, exa_fixture()));
        let endpoints = search_site(site.port);
        f.tools.gate().queue(Answer::allow_once());
        f.tools
            .web_search_with("t1", "once", None, EXA_ENV, FIRECRAWL_ENV, &endpoints)
            .expect("once");
        assert!(!f.session.meta().expect("meta").allow.web_search);
        let tools = f.tools.clone();
        let ends = endpoints.clone();
        let turn = thread::spawn(move || {
            tools.web_search_with("t1", "again", None, EXA_ENV, FIRECRAWL_ENV, &ends)
        });
        let card = f.wait_for_permissions(2).pop().expect("second card");
        assert_eq!(card.path.as_deref(), Some("again"));
        f.tools.gate().answer(Answer::deny()).expect("deny");
        let denied = turn.join().expect("join").expect("denied");
        assert!(denied.denied);
        assert_eq!(site.hits.load(Ordering::SeqCst), 1);

        let yolo = fixture("yolo-search");
        yolo.session
            .update(|meta| {
                meta.yolo = true;
                true
            })
            .expect("yolo");
        let yolo_site = serve(|_| json_body(200, exa_fixture()));
        let found = yolo
            .tools
            .web_search_with(
                "t1",
                "yolo query",
                None,
                EXA_ENV,
                FIRECRAWL_ENV,
                &search_site(yolo_site.port),
            )
            .expect("yolo");
        assert!(found.text.contains("Kyoto Agent"), "{}", found.text);
        assert_eq!(yolo.decisions(), vec![Decision::AllowOnce]);
        assert_eq!(yolo_site.hits.load(Ordering::SeqCst), 1);
        assert!(!yolo.session.meta().expect("meta").allow.web_search);
    }

    #[test]
    fn a_long_snippet_is_capped_at_the_output_limit() {
        let huge = "h".repeat(OUTPUT_LIMIT);
        let body = format!(
            r#"{{"results":[{{"title":"{huge}","url":"https://example.com/t","highlights":["short"]}}]}}"#
        );
        let text = cap_result(render(Vendor::Exa, &body).expect("render"));
        assert!(text.len() <= OUTPUT_LIMIT, "{}", text.len());
        assert!(text.contains("Truncated"));
        assert!(text.starts_with("Exa"));
    }

    #[test]
    fn clamp_results_defaults_and_stops_at_ten() {
        assert_eq!(clamp_results(None), 5);
        assert_eq!(clamp_results(Some(0)), 1);
        assert_eq!(clamp_results(Some(4)), 4);
        assert_eq!(clamp_results(Some(99)), 10);
    }

    fn resolve_examples(host: &str, _port: u16) -> Result<Vec<IpAddr>, String> {
        match host {
            "docs.example" | "other.example" | "example.com" => {
                Ok(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))])
            }
            other => Err(format!("no address for {other}")),
        }
    }

    fn scripted_docs(url: &str) -> FixedReply {
        assert!(
            url.starts_with("https://docs.example/") || url.starts_with("https://other.example"),
            "{url}"
        );
        FixedReply {
            status: 200,
            location: None,
            content_type: "text/html".to_string(),
            body: b"<h1>Docs</h1>".to_vec(),
        }
    }
}
