//! A traced send: each request's timing, size and outcome go to one sink
//! (ADR 0036).
//!
//! While debugging is off, or no sink is installed, [`send`] is
//! `request.send()` plus one relaxed atomic load. While it is on, the response
//! body is wrapped so that the first byte, the end, a failure or the caller
//! dropping the body are all reported. At [`Level::Text`] the text bodies are
//! kept too, cut and masked here, so a key never leaves this module.

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use bytes::Bytes;
use reqwest::{RequestBuilder, Response, ResponseBuilderExt, Url, header};

use crate::{NetError, classify};

/// What a request is for. The kind decides how much of its address is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Provider,
    Mcp,
    Tool,
    Media,
    Peer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Off,
    Metadata,
    Text,
}

/// Unique within the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u64);

impl Id {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The caller dropped the body before its end.
    Dropped,
    Failed(NetError),
}

#[derive(Clone, Debug)]
pub enum Event {
    Started {
        id: Id,
        kind: Kind,
        method: String,
        /// Redacted: see [`redact`].
        url: String,
        at: Instant,
        bytes_out: Option<u64>,
        body_out: Option<String>,
    },
    /// The headers arrived.
    Response { id: Id, status: u16, at: Instant },
    /// The first body chunk arrived.
    FirstBody { id: Id, at: Instant },
    Ended {
        id: Id,
        at: Instant,
        bytes_in: u64,
        outcome: Outcome,
        body_in: Option<String>,
    },
}

pub trait Sink: Send + Sync {
    fn record(&self, event: Event);
}

static LEVEL: AtomicU8 = AtomicU8::new(0);
static SINK: OnceLock<Arc<dyn Sink>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Off,
        1 => Level::Metadata,
        _ => Level::Text,
    }
}

/// Sets the sink once. A second call is ignored.
pub fn install(sink: Arc<dyn Sink>) {
    let _ = SINK.set(sink);
}

/// What the caller of a traced send gets, plus the tracing when it is on.
pub async fn send(kind: Kind, request: RequestBuilder) -> Result<Response, reqwest::Error> {
    let level = level();
    if level == Level::Off {
        return request.send().await;
    }
    match SINK.get() {
        Some(sink) => send_with(sink, level, kind, request).await,
        None => request.send().await,
    }
}

/// Starts a record by hand, for callers that don't use reqwest. `None` while
/// debugging is off or no sink is installed.
pub fn start(kind: Kind, method: &str, url: &str) -> Option<Handle> {
    let sink = SINK.get()?;
    start_with(sink, level(), kind, method, url)
}

fn start_with(
    sink: &Arc<dyn Sink>,
    level: Level,
    kind: Kind,
    method: &str,
    url: &str,
) -> Option<Handle> {
    if level == Level::Off {
        return None;
    }
    // An address that doesn't parse is recorded as empty, not as it was given.
    let url = Url::parse(url)
        .map(|u| redact(kind, &u))
        .unwrap_or_default();
    let open = Open::new(sink);
    sink.record(Event::Started {
        id: open.id,
        kind,
        method: method.to_owned(),
        url,
        at: Instant::now(),
        bytes_out: None,
        body_out: None,
    });
    Some(Handle { open })
}

/// A request recorded by hand. Dropping it without [`Handle::end`] records
/// [`Outcome::Dropped`].
pub struct Handle {
    open: Open,
}

impl Handle {
    pub fn response(&self, status: u16) {
        self.open.response(status);
    }

    pub fn end(mut self, outcome: Outcome) {
        self.open.end(0, outcome, None);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.open.end(0, Outcome::Dropped, None);
    }
}

/// A started record that has not ended yet.
struct Open {
    sink: Arc<dyn Sink>,
    id: Id,
    ended: bool,
}

impl Open {
    fn new(sink: &Arc<dyn Sink>) -> Self {
        Self {
            sink: sink.clone(),
            id: Id(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
            ended: false,
        }
    }

    fn response(&self, status: u16) {
        self.sink.record(Event::Response {
            id: self.id,
            status,
            at: Instant::now(),
        });
    }

    fn end(&mut self, bytes_in: u64, outcome: Outcome, body_in: Option<String>) {
        if !std::mem::replace(&mut self.ended, true) {
            self.sink.record(Event::Ended {
                id: self.id,
                at: Instant::now(),
                bytes_in,
                outcome,
                body_in,
            });
        }
    }
}

async fn send_with(
    sink: &Arc<dyn Sink>,
    level: Level,
    kind: Kind,
    request: RequestBuilder,
) -> Result<Response, reqwest::Error> {
    if level == Level::Off {
        return request.send().await;
    }
    let (client, built) = request.build_split();
    let request = built?;
    let secrets = secrets(request.headers());
    let body = request.body().and_then(|b| b.as_bytes());
    let bytes_out = body.map(|b| b.len() as u64).or_else(|| {
        request
            .headers()
            .get(header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    });
    let body_out = match body {
        Some(bytes) if level == Level::Text && textual(request.headers()) => {
            let mut capture = Capture::default();
            capture.push(bytes);
            Some(mask_body(&capture.finish(), &secrets))
        }
        _ => None,
    };
    let mut open = Open::new(sink);
    sink.record(Event::Started {
        id: open.id,
        kind,
        method: request.method().as_str().to_owned(),
        url: redact(kind, request.url()),
        at: Instant::now(),
        bytes_out,
        body_out,
    });

    let response = match client.execute(request).await {
        Ok(response) => response,
        Err(e) => {
            open.end(0, Outcome::Failed(classify(&e)), None);
            return Err(e);
        }
    };
    open.response(response.status().as_u16());

    let capture = (level == Level::Text && textual(response.headers())).then(Capture::default);
    let url = response.url().clone();
    let (parts, inner) = http::Response::<reqwest::Body>::from(response).into_parts();
    // The conversion to `http::Response` drops the URL, so it goes back in.
    let mut rebuilt = http::Response::builder()
        .status(parts.status)
        .version(parts.version)
        .url(url);
    if let Some(headers) = rebuilt.headers_mut() {
        *headers = parts.headers;
    }
    if let Some(extensions) = rebuilt.extensions_mut() {
        extensions.extend(parts.extensions);
    }
    let traced = Traced {
        inner,
        open,
        bytes_in: 0,
        first: false,
        capture,
        secrets,
    };
    let rebuilt = rebuilt
        .body(reqwest::Body::wrap(traced))
        .expect("status, version and headers came from a valid response");
    Ok(Response::from(rebuilt))
}

/// The response body as the caller reads it. It wraps reqwest's own body, so
/// the read timeouts still apply.
struct Traced {
    inner: reqwest::Body,
    open: Open,
    bytes_in: u64,
    first: bool,
    capture: Option<Capture>,
    secrets: Vec<String>,
}

impl Traced {
    fn end(&mut self, outcome: Outcome) {
        let body = self
            .capture
            .take()
            .map(|c| mask_body(&c.finish(), &self.secrets));
        self.open.end(self.bytes_in, outcome, body);
    }
}

impl http_body::Body for Traced {
    type Data = Bytes;
    type Error = reqwest::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    if !std::mem::replace(&mut this.first, true) {
                        this.open.sink.record(Event::FirstBody {
                            id: this.open.id,
                            at: Instant::now(),
                        });
                    }
                    this.bytes_in += data.len() as u64;
                    if let Some(capture) = &mut this.capture {
                        capture.push(data);
                    }
                }
            }
            Poll::Ready(Some(Err(e))) => this.end(Outcome::Failed(classify(e))),
            Poll::Ready(None) => this.end(Outcome::Done),
            Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for Traced {
    fn drop(&mut self) {
        self.end(Outcome::Dropped);
    }
}

const HEAD: usize = 8 * 1024;
const TAIL: usize = 8 * 1024;

/// The first and the rolling last 8 KiB of a body.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    /// Bytes that fell out of the middle.
    cut: bool,
}

impl Capture {
    fn push(&mut self, mut bytes: &[u8]) {
        if self.head.len() < HEAD {
            let take = (HEAD - self.head.len()).min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend(bytes);
        let excess = self.tail.len().saturating_sub(TAIL);
        if excess > 0 {
            self.tail.drain(..excess);
            self.cut = true;
        }
    }

    fn finish(self) -> String {
        let mut bytes = self.head;
        if self.cut {
            bytes.extend_from_slice("\n\u{2026}\n".as_bytes());
        }
        bytes.extend(self.tail);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// `text/*` (which includes event streams), `application/json` and
/// `application/*+json`.
fn textual(headers: &header::HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let essence = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("text/")
        || essence == "application/json"
        || (essence.starts_with("application/") && essence.ends_with("+json"))
}

/// The values of the headers marked sensitive, without their scheme.
fn secrets(headers: &header::HeaderMap) -> Vec<String> {
    headers
        .values()
        .filter(|v| v.is_sensitive())
        .filter_map(|v| v.to_str().ok())
        .map(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("Basic "))
                .unwrap_or(v)
                .to_owned()
        })
        .collect()
}

/// The address as it is recorded: never userinfo or headers. The path of a
/// media or MCP address is left out, because tokens live there. A query is
/// always left out.
pub fn redact(kind: Kind, url: &Url) -> String {
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    let path = url.path();
    match kind {
        Kind::Provider | Kind::Tool | Kind::Peer => out.push_str(path),
        Kind::Media | Kind::Mcp => {
            if !path.is_empty() && path != "/" {
                out.push_str("/\u{2026}");
            }
        }
    }
    if url.query().is_some() {
        out.push_str("?\u{2026}");
    }
    out
}

/// Replaces the given secrets and anything that looks like a key with
/// `[key]`, and keeps whitespace and line breaks as they are. The heuristic
/// is the one `pinna-assist` uses for error messages.
pub fn mask_body(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|s| s.len() >= 8) {
        text = text.replace(secret.as_str(), "[key]");
    }
    let mut out = String::with_capacity(text.len());
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() || matches!(c, '"' | '{' | '}' | '[' | ']' | ',' | ':') {
            if let Some(s) = start.take() {
                push_token(&mut out, &text[s..i]);
            }
            out.push(c);
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        push_token(&mut out, &text[s..]);
    }
    out
}

fn push_token(out: &mut String, token: &str) {
    // A key has a long run of letters and digits between its dashes; a model
    // name such as gpt-6-luna-2026-09-22 doesn't.
    let long = token
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|run| {
            run.len() >= 16
                && run.chars().any(|c| c.is_ascii_digit())
                && run.chars().any(|c| c.is_ascii_alphabetic())
        });
    out.push_str(if long || token.contains("**") {
        "[key]"
    } else {
        token
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientKind, client};
    use axum::{
        Router,
        routing::{get, post},
    };
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct Collect(Mutex<Vec<Event>>);

    impl Sink for Collect {
        fn record(&self, event: Event) {
            self.0.lock().unwrap().push(event);
        }
    }

    impl Collect {
        fn events(&self) -> Vec<Event> {
            self.0.lock().unwrap().clone()
        }
    }

    fn sink() -> (Arc<Collect>, Arc<dyn Sink>) {
        let collect = Arc::new(Collect::default());
        let dynamic: Arc<dyn Sink> = collect.clone();
        (collect, dynamic)
    }

    async fn serve(routes: Router) -> String {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", socket.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(socket, routes).await.unwrap() });
        url
    }

    fn ended(events: &[Event]) -> (u64, Outcome, Option<String>) {
        match events.last() {
            Some(Event::Ended {
                bytes_in,
                outcome,
                body_in,
                ..
            }) => (*bytes_in, *outcome, body_in.clone()),
            other => panic!("not ended: {other:?}"),
        }
    }

    fn started(events: &[Event]) -> (Option<u64>, Option<String>) {
        match events.first() {
            Some(Event::Started {
                bytes_out,
                body_out,
                ..
            }) => (*bytes_out, body_out.clone()),
            other => panic!("not started: {other:?}"),
        }
    }

    const KEY: &str = "sk-abcdefgh12345678";

    fn sensitive(value: &str) -> header::HeaderValue {
        let mut v = header::HeaderValue::from_str(value).unwrap();
        v.set_sensitive(true);
        v
    }

    #[tokio::test]
    async fn metadata_events_in_order() {
        let url = serve(Router::new().route("/j", get(|| async { r#"{"a":1}"# }))).await;
        let (collect, sink) = sink();
        let request = client(ClientKind::Mcp).get(format!("{url}/j?x=1"));
        let response = send_with(&sink, Level::Metadata, Kind::Tool, request)
            .await
            .unwrap();
        assert_eq!(response.url().as_str(), format!("{url}/j?x=1"));
        assert_eq!(response.bytes().await.unwrap().len(), 7);
        let events = collect.events();
        assert_eq!(events.len(), 4, "{events:?}");
        match &events[0] {
            Event::Started {
                kind,
                method,
                url: shown,
                bytes_out,
                body_out,
                ..
            } => {
                assert_eq!(*kind, Kind::Tool);
                assert_eq!(method, "GET");
                assert_eq!(shown, &format!("{url}/j?…"));
                assert_eq!((*bytes_out, body_out), (None, &None));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(events[1], Event::Response { status: 200, .. }));
        assert!(matches!(events[2], Event::FirstBody { .. }));
        assert_eq!(ended(&events), (7, Outcome::Done, None));
    }

    #[tokio::test]
    async fn off_records_nothing() {
        let url = serve(Router::new().route("/", get(|| async { "ok" }))).await;
        let (collect, sink) = sink();
        let request = client(ClientKind::Mcp).get(&url);
        let response = send_with(&sink, Level::Off, Kind::Tool, request)
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        assert!(collect.events().is_empty());
    }

    #[tokio::test]
    async fn dropping_the_body_is_reported() {
        let big = vec![b'x'; 8 * 1024 * 1024];
        let url = serve(Router::new().route("/", get(move || async move { big }))).await;
        let (collect, sink) = sink();
        let request = client(ClientKind::Media).get(&url);
        let mut response = send_with(&sink, Level::Metadata, Kind::Media, request)
            .await
            .unwrap();
        assert!(response.chunk().await.unwrap().is_some());
        drop(response);
        let (bytes_in, outcome, _) = ended(&collect.events());
        assert_eq!(outcome, Outcome::Dropped);
        assert!(bytes_in > 0 && bytes_in < 8 * 1024 * 1024);
    }

    #[tokio::test]
    async fn refused_connection_fails_without_a_response() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", socket.local_addr().unwrap());
        drop(socket);
        let (collect, sink) = sink();
        let request = client(ClientKind::Mcp).get(url);
        send_with(&sink, Level::Metadata, Kind::Tool, request)
            .await
            .unwrap_err();
        let events = collect.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(
            ended(&events),
            (0, Outcome::Failed(NetError::Unreachable), None)
        );
    }

    #[test]
    fn redaction() {
        let plain = Url::parse("https://user:pw@example.com:8443/v1/chat?key=abc").unwrap();
        let none = Url::parse("http://example.com").unwrap();
        let root = Url::parse("http://example.com/").unwrap();
        let token = Url::parse("http://example.com/stream/tok3n.mp3").unwrap();
        for (kind, url, want) in [
            (Kind::Provider, &plain, "https://example.com:8443/v1/chat?…"),
            (Kind::Tool, &plain, "https://example.com:8443/v1/chat?…"),
            (Kind::Peer, &plain, "https://example.com:8443/v1/chat?…"),
            (Kind::Media, &plain, "https://example.com:8443/…?…"),
            (Kind::Mcp, &plain, "https://example.com:8443/…?…"),
            (Kind::Media, &token, "http://example.com/…"),
            (Kind::Mcp, &none, "http://example.com"),
            (Kind::Mcp, &root, "http://example.com"),
            (Kind::Media, &root, "http://example.com"),
            (Kind::Tool, &none, "http://example.com/"),
        ] {
            assert_eq!(redact(kind, url), want, "{kind:?} {url}");
        }
    }

    #[tokio::test]
    async fn text_bodies_are_kept_and_masked() {
        let url = serve(Router::new().route(
            "/",
            post(|body: String| async move { ([("content-type", "application/json")], body) }),
        ))
        .await;
        let (collect, sink) = sink();
        let request = client(ClientKind::Mcp)
            .post(&url)
            .header(header::AUTHORIZATION, sensitive(&format!("Bearer {KEY}")))
            .header(header::CONTENT_TYPE, "application/json")
            .body(serde_like(KEY));
        let response = send_with(&sink, Level::Text, Kind::Provider, request)
            .await
            .unwrap();
        response.bytes().await.unwrap();
        let events = collect.events();
        let (bytes_out, body_out) = started(&events);
        let body_out = body_out.unwrap();
        assert_eq!(bytes_out, Some(serde_like(KEY).len() as u64));
        assert!(body_out.contains("\"prompt\":\"hi\""), "{body_out}");
        assert!(body_out.contains("\"key\":\"[key]\""), "{body_out}");
        let body_in = ended(&events).2.unwrap();
        assert!(body_in.contains("\"key\":\"[key]\""), "{body_in}");
        assert!(!body_in.contains(KEY));
    }

    /// A JSON body without a serializer dependency.
    fn serde_like(key: &str) -> String {
        format!(r#"{{"prompt":"hi","key":"{key}"}}"#)
    }

    #[tokio::test]
    async fn metadata_level_keeps_no_text() {
        let url = serve(Router::new().route(
            "/",
            post(|body: String| async move { ([("content-type", "application/json")], body) }),
        ))
        .await;
        let (collect, sink) = sink();
        let request = client(ClientKind::Mcp)
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .body("{}");
        let response = send_with(&sink, Level::Metadata, Kind::Provider, request)
            .await
            .unwrap();
        response.bytes().await.unwrap();
        let events = collect.events();
        assert_eq!(started(&events), (Some(2), None));
        assert_eq!(ended(&events), (2, Outcome::Done, None));
    }

    #[tokio::test]
    async fn multipart_requests_and_audio_responses_are_not_kept() {
        let url = serve(Router::new().route(
            "/",
            post(|_: axum::body::Bytes| async { ([("content-type", "audio/mpeg")], "ID3 audio") }),
        ))
        .await;
        let (collect, sink) = sink();
        let form = reqwest::multipart::Form::new().text("a", "b");
        let request = client(ClientKind::Mcp).post(&url).multipart(form);
        let response = send_with(&sink, Level::Text, Kind::Provider, request)
            .await
            .unwrap();
        response.bytes().await.unwrap();
        let events = collect.events();
        assert_eq!(started(&events).1, None);
        assert_eq!(ended(&events).2, None);
        assert_eq!(ended(&events).0, 9);
    }

    #[tokio::test]
    async fn long_text_keeps_head_and_tail() {
        let text = "word ".repeat(8192);
        let sent = text.clone();
        let url = serve(Router::new().route(
            "/",
            get(move || async move { ([("content-type", "text/plain; charset=utf-8")], sent) }),
        ))
        .await;
        let (collect, sink) = sink();
        let request = client(ClientKind::Mcp).get(&url);
        let response = send_with(&sink, Level::Text, Kind::Provider, request)
            .await
            .unwrap();
        response.bytes().await.unwrap();
        let (bytes_in, _, body) = ended(&collect.events());
        let body = body.unwrap();
        assert_eq!(bytes_in, text.len() as u64);
        assert_eq!(body.len(), 2 * 8 * 1024 + "\n…\n".len());
        assert_eq!(&body[..8192], &text[..8192]);
        assert_eq!(&body[8192..8192 + 5], "\n…\n");
        assert_eq!(&body[8192 + 5..], &text[text.len() - 8192..]);
    }

    #[test]
    fn mask_body_keeps_layout_and_masks_keys() {
        let text = "{\n  \"a\": \"sk-proj-Abcdefgh12345678xyz\",\n  \"b\": \"hello  world\"\n}";
        let want = "{\n  \"a\": \"[key]\",\n  \"b\": \"hello  world\"\n}";
        assert_eq!(mask_body(text, &[]), want);
        assert_eq!(
            mask_body("a secret-value-1 b", &["secret-value-1".into()]),
            "a [key] b"
        );
        assert_eq!(
            mask_body("gpt-6-luna-2026-09-22 ****", &[]),
            "gpt-6-luna-2026-09-22 [key]"
        );
    }

    #[test]
    fn handle_reports_start_response_end() {
        let (collect, sink) = sink();
        let handle =
            start_with(&sink, Level::Metadata, Kind::Peer, "POST", "http://h:1/p?q").unwrap();
        handle.response(200);
        handle.end(Outcome::Done);
        let events = collect.events();
        assert_eq!(events.len(), 3);
        match &events[0] {
            Event::Started { url, kind, .. } => {
                assert_eq!(url, "http://h:1/p?…");
                assert_eq!(*kind, Kind::Peer);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(events[1], Event::Response { status: 200, .. }));
        assert_eq!(ended(&events), (0, Outcome::Done, None));
    }

    #[test]
    fn dropped_handle_is_dropped() {
        let (collect, sink) = sink();
        drop(start_with(&sink, Level::Metadata, Kind::Peer, "GET", "http://h/").unwrap());
        assert_eq!(ended(&collect.events()).1, Outcome::Dropped);
        assert!(start_with(&sink, Level::Off, Kind::Peer, "GET", "http://h/").is_none());
    }
}
