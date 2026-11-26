//! Shared provider HTTP: where an adapter sends ([`Endpoint`]),
//! authentication, status mapping and capped body reads.
//!
//! Errors contain only a slot, preset and bounded reason. In particular, a
//! reqwest error or response body is never formatted into a [`ProviderError`];
//! a refusal's error message alone is kept, masked and capped, for the log
//! ([`ProviderError::said`]).

use bytes::Bytes;
use pinna_http::{Secret, classify, reqwest};
use reqwest::{RequestBuilder, Response, header};

use crate::{
    error::{ProviderError, Reason, Slot},
    presets::PresetId,
};

/// Authentication applied immediately before a provider request is sent.
#[derive(Clone, Debug, Default)]
pub enum Auth {
    #[default]
    None,
    Bearer(Secret),
    /// An API key and its header, such as `xi-api-key` or `x-goog-api-key`.
    ApiKey(&'static str, Secret),
}

impl Auth {
    fn apply(&self, request: RequestBuilder) -> Result<RequestBuilder, header::InvalidHeaderValue> {
        match self {
            Self::None => Ok(request),
            Self::Bearer(key) => {
                let mut value = header::HeaderValue::from_str(&format!("Bearer {}", key.expose()))?;
                value.set_sensitive(true);
                Ok(request.header(header::AUTHORIZATION, value))
            }
            Self::ApiKey(name, key) => {
                let mut value = header::HeaderValue::from_str(key.expose())?;
                value.set_sensitive(true);
                Ok(request.header(header::HeaderName::from_static(name), value))
            }
        }
    }
}

/// Where and how one adapter reaches its service. Every adapter, for a run or
/// for a save-time check, is built from one.
#[derive(Clone)]
pub struct Endpoint {
    pub http: reqwest::Client,
    /// The connection's address, without a trailing slash.
    pub base_url: String,
    pub auth: Auth,
    pub preset: PresetId,
    /// Whether the clock is set, so a TLS failure before it can be named.
    pub clock_synced: fn() -> bool,
}

impl Endpoint {
    pub fn new(
        http: reqwest::Client,
        base_url: &str,
        auth: Auth,
        preset: PresetId,
        clock_synced: fn() -> bool,
    ) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            auth,
            preset,
            clock_synced,
        }
    }

    /// The base URL with `path` appended.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// [`send`] with this endpoint's key.
    pub async fn send(
        &self,
        request: RequestBuilder,
        slot: Slot,
    ) -> Result<Response, ProviderError> {
        send(request, &self.auth, slot, self.preset, self.clock_synced).await
    }

    /// [`read_capped`] for this endpoint.
    pub async fn read_capped(
        &self,
        response: Response,
        cap: usize,
        slot: Slot,
    ) -> Result<Bytes, ProviderError> {
        read_capped(response, cap, slot, self.preset, self.clock_synced).await
    }

    pub fn error(&self, slot: Slot, reason: Reason) -> ProviderError {
        ProviderError::new(slot, self.preset, reason)
    }
}

/// Send a request, mapping non-success statuses without reading their bodies.
/// Provider clients have redirects disabled, so authentication stays on the
/// origin to which it was configured to go.
pub async fn send<F>(
    request: RequestBuilder,
    auth: &Auth,
    slot: Slot,
    preset: PresetId,
    clock_synced: F,
) -> Result<Response, ProviderError>
where
    F: Fn() -> bool,
{
    let request = auth
        .apply(request)
        .map_err(|_| ProviderError::new(slot, preset, Reason::Protocol))?;
    let response = pinna_http::trace::send(pinna_http::trace::Kind::Provider, request)
        .await
        .map_err(|error| network_error(slot, preset, &error, clock_synced))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let mut error = ProviderError::new(slot, preset, Reason::Http(status));
        error.said = refusal_words(response, auth).await;
        return Err(error);
    }
    Ok(response)
}

/// An error body is read only this far.
const ERROR_BODY_CAP: usize = 4 * 1024;
/// …and for at most this long, so a slow refusal can't hold the request.
const ERROR_BODY_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What a service said in its error body: the message field of the usual
/// JSON shapes, or a short plain-text body. Keys are masked.
async fn refusal_words(mut response: Response, auth: &Auth) -> Option<String> {
    let plain = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/plain"));
    let read = async {
        let mut body = Vec::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            body.extend_from_slice(&chunk);
            if body.len() >= ERROR_BODY_CAP {
                break;
            }
        }
        body
    };
    let body = tokio::time::timeout(ERROR_BODY_WAIT, read).await.ok()?;
    let text = String::from_utf8_lossy(&body[..body.len().min(ERROR_BODY_CAP)]).into_owned();
    let message = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(json) => error_message(&json)?,
        Err(_) if plain => text,
        Err(_) => return None,
    };
    let key = match auth {
        Auth::Bearer(key) | Auth::ApiKey(_, key) => Some(key.expose()),
        Auth::None => None,
    };
    let said = mask(&message, key);
    (!said.is_empty()).then_some(said)
}

/// The message in OpenAI's, Anthropic's and Gemini's `{"error": {"message"}}`,
/// ElevenLabs' `{"detail": {"message"}}`, FastAPI's `{"detail": "…"}`, or a
/// bare `{"error": "…"}` or `{"message": "…"}`. Gemini may wrap it in a list.
fn error_message(json: &serde_json::Value) -> Option<String> {
    let json = json.as_array().and_then(|a| a.first()).unwrap_or(json);
    [
        json.pointer("/error/message"),
        json.pointer("/detail/message"),
        json.get("error"),
        json.get("detail"),
        json.get("message"),
    ]
    .into_iter()
    .flatten()
    .find_map(|v| v.as_str())
    .map(str::to_owned)
}

/// One line, at most [`crate::error::SAID_MAX`] characters, with the key and
/// anything that looks like one (a word with a run of 16 or more letters and
/// digits, or a partly starred token such as `sk-…****abcd`) replaced by
/// `[key]`.
pub(crate) fn mask(message: &str, key: Option<&str>) -> String {
    let mut text = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(key) = key.filter(|key| key.len() >= 8) {
        text = text.replace(key, "[key]");
    }
    let words: Vec<String> = text
        .split(' ')
        .map(|word| {
            // A key has a long run of letters and digits between its
            // dashes; a model name such as gpt-6-luna-2026-09-22 doesn't.
            let long = word.split(|c: char| !c.is_ascii_alphanumeric()).any(|run| {
                run.len() >= 16
                    && run.chars().any(|c| c.is_ascii_digit())
                    && run.chars().any(|c| c.is_ascii_alphabetic())
            });
            if long || word.contains("**") {
                "[key]".to_owned()
            } else {
                word.to_owned()
            }
        })
        .collect();
    let text = words.join(" ");
    let mut out: String = text.chars().take(crate::error::SAID_MAX).collect();
    if text.chars().count() > crate::error::SAID_MAX {
        out.push('…');
    }
    out
}

/// Read at most `cap` bytes. A larger response is dropped as soon as its size
/// is known or the first chunk beyond the cap arrives.
pub async fn read_capped<F>(
    mut response: Response,
    cap: usize,
    slot: Slot,
    preset: PresetId,
    clock_synced: F,
) -> Result<Bytes, ProviderError>
where
    F: Fn() -> bool,
{
    if !response.status().is_success() {
        let status = response.status().as_u16();
        drop(response);
        return Err(ProviderError::new(slot, preset, Reason::Http(status)));
    }
    if response
        .content_length()
        .is_some_and(|length| length > cap as u64)
    {
        return Err(ProviderError::new(slot, preset, Reason::TooLarge));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| network_error(slot, preset, &error, &clock_synced))?
    {
        if chunk.len() > cap.saturating_sub(body.len()) {
            return Err(ProviderError::new(slot, preset, Reason::TooLarge));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

/// A request or body read that failed before or instead of a status.
pub(crate) fn network_error<F>(
    slot: Slot,
    preset: PresetId,
    error: &reqwest::Error,
    clock_synced: F,
) -> ProviderError
where
    F: Fn() -> bool,
{
    ProviderError::new(
        slot,
        preset,
        Reason::from_net(classify(error), clock_synced()),
    )
}

/// A response body stream wraps its request error in an [`std::io::Error`];
/// name the inner error when it is still there.
pub(crate) fn body_reason(error: &std::io::Error, clock_synced: bool) -> Reason {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
    {
        Some(inner) => Reason::from_net(classify(inner), clock_synced),
        None => Reason::Protocol,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pinna_http::{ClientKind, client, reqwest::StatusCode};

    use super::*;
    use crate::testing::{FakeResponse, FakeServer};

    #[test]
    fn an_endpoint_url_tolerates_a_trailing_slash() {
        let client = client(ClientKind::Provider { read: None });
        for base in ["https://api.openai.com/v1", "https://api.openai.com/v1/"] {
            let endpoint =
                Endpoint::new(client.clone(), base, Auth::None, PresetId::OpenAi, || true);
            assert_eq!(
                endpoint.url("/audio/transcriptions"),
                "https://api.openai.com/v1/audio/transcriptions"
            );
        }
    }

    #[test]
    fn tls_before_clock_sync_has_its_own_reason() {
        use pinna_http::NetError;
        assert_eq!(Reason::from_net(NetError::Tls, false), Reason::ClockNotSet);
        assert_eq!(Reason::from_net(NetError::Tls, true), Reason::Tls);
        assert_eq!(Reason::from_net(NetError::Timeout, false), Reason::Timeout);
    }

    #[tokio::test]
    async fn every_provider_auth_header_reaches_the_server() {
        let server = FakeServer::start().await;
        for path in ["/bearer", "/elevenlabs", "/anthropic", "/gemini", "/none"] {
            server.script_status(reqwest::Method::POST, path, StatusCode::OK);
        }
        let client = client(ClientKind::Provider { read: None });
        let cases = [
            (
                "/bearer",
                Auth::Bearer(Secret::new("bearer-key")),
                "authorization",
                "Bearer bearer-key",
            ),
            (
                "/elevenlabs",
                Auth::ApiKey("xi-api-key", Secret::new("elevenlabs-key")),
                "xi-api-key",
                "elevenlabs-key",
            ),
            (
                "/anthropic",
                Auth::ApiKey("x-api-key", Secret::new("anthropic-key")),
                "x-api-key",
                "anthropic-key",
            ),
            (
                "/gemini",
                Auth::ApiKey("x-goog-api-key", Secret::new("gemini-key")),
                "x-goog-api-key",
                "gemini-key",
            ),
        ];
        for (path, auth, header, expected) in cases {
            send(
                client.post(server.url(path)),
                &auth,
                Slot::Chat,
                PresetId::Custom,
                || true,
            )
            .await
            .unwrap();
            let requests = server.requests();
            let request = requests.last().unwrap();
            assert_eq!(request.path, path);
            assert_eq!(request.header(header), Some(expected));
        }

        send(
            client.post(server.url("/none")),
            &Auth::None,
            Slot::Chat,
            PresetId::Custom,
            || true,
        )
        .await
        .unwrap();
        let requests = server.requests();
        assert_eq!(requests.last().unwrap().header("authorization"), None);
        assert_eq!(requests.last().unwrap().header("xi-api-key"), None);
    }

    #[tokio::test]
    async fn a_redirect_is_an_error_and_the_key_does_not_follow_it() {
        let destination = FakeServer::start().await;
        destination.script_status(reqwest::Method::GET, "/stolen", StatusCode::OK);
        let source = FakeServer::start().await;
        source.script(
            reqwest::Method::GET,
            "/redirect",
            FakeResponse::empty(StatusCode::FOUND).header("location", &destination.url("/stolen")),
        );

        let error = send(
            client(ClientKind::Provider { read: None }).get(source.url("/redirect")),
            &Auth::ApiKey("x-api-key", Secret::new("do-not-forward")),
            Slot::Chat,
            PresetId::Custom,
            || true,
        )
        .await
        .unwrap_err();
        assert_eq!(error.reason, Reason::Http(302));
        assert_eq!(source.requests().len(), 1);
        assert!(destination.requests().is_empty());
    }

    #[tokio::test]
    async fn a_refusal_keeps_what_the_service_said_with_the_key_masked() {
        let server = FakeServer::start().await;
        let key = "sk-proj-0123456789abcdefghij";
        server.script(
            reqwest::Method::POST,
            "/openai",
            FakeResponse::new(
                StatusCode::BAD_REQUEST,
                r#"{"error": {"message": "Chat Completions supports function calling only with reasoning_effort set to none.", "type": "invalid_request_error"}}"#,
            ),
        );
        server.script(
            reqwest::Method::POST,
            "/wrong-key",
            FakeResponse::new(
                StatusCode::UNAUTHORIZED,
                format!(r#"{{"error": {{"message": "Incorrect API key provided: {key}. Or sk-proj-01********ghij."}}}}"#),
            ),
        );
        server.script(
            reqwest::Method::POST,
            "/elevenlabs",
            FakeResponse::new(
                StatusCode::BAD_REQUEST,
                r#"{"detail": {"status": "invalid_model_id", "message": "'whisper-1' is not a valid model_id."}}"#,
            ),
        );
        server.script(
            reqwest::Method::POST,
            "/html",
            FakeResponse::new(
                StatusCode::BAD_GATEWAY,
                "<html><body>Bad gateway</body></html>",
            )
            .header("content-type", "text/html"),
        );
        let refuse = |path: &'static str| {
            let server = &server;
            async move {
                send(
                    client(ClientKind::Provider { read: None }).post(server.url(path)),
                    &Auth::Bearer(Secret::new(key)),
                    Slot::Chat,
                    PresetId::OpenAi,
                    || true,
                )
                .await
                .unwrap_err()
            }
        };

        let luna = refuse("/openai").await;
        assert_eq!(luna.reason, Reason::Http(400));
        assert_eq!(
            luna.said.as_deref(),
            Some(
                "Chat Completions supports function calling only with reasoning_effort set to none."
            )
        );
        // The display and the equality stay without it.
        assert_eq!(luna.to_string(), "brain via openai: HTTP 400");
        assert_eq!(
            luna,
            ProviderError::new(Slot::Chat, PresetId::OpenAi, Reason::Http(400))
        );

        let wrong = refuse("/wrong-key").await.said.unwrap();
        assert!(!wrong.contains("0123456789"), "{wrong}");
        assert_eq!(wrong, "Incorrect API key provided: [key]. Or [key]");
        assert_eq!(
            refuse("/elevenlabs").await.said.as_deref(),
            Some("'whisper-1' is not a valid model_id.")
        );
        assert_eq!(refuse("/html").await.said, None);
    }

    #[test]
    fn what_a_service_said_is_one_capped_line() {
        let long = "word ".repeat(100);
        let said = mask(&long, None);
        assert_eq!(said.chars().count(), crate::error::SAID_MAX + 1);
        assert!(said.ends_with('…'));
        assert_eq!(mask("two\n lines", None), "two lines");
        assert_eq!(
            mask("the model gpt-6-luna-2026-09-22 is fine", None),
            "the model gpt-6-luna-2026-09-22 is fine"
        );
        assert_eq!(
            mask("token AbCdEfGhIjKlMnOpQr12345 here", None),
            "token [key] here"
        );
        assert_eq!(
            error_message(&serde_json::json!([{"error": {"message": "From Gemini."}}])),
            Some("From Gemini.".into())
        );
        assert_eq!(
            error_message(&serde_json::json!({"detail": "Not Found"})),
            Some("Not Found".into())
        );
        assert_eq!(error_message(&serde_json::json!({"ok": false})), None);
    }

    #[tokio::test]
    async fn a_capped_read_refuses_only_a_body_over_the_cap() {
        let server = FakeServer::start().await;
        for path in ["/large", "/exact"] {
            server.script(
                reqwest::Method::GET,
                path,
                FakeResponse::new(StatusCode::OK, "123456"),
            );
        }
        let client = client(ClientKind::Provider { read: None });
        let response = send(
            client.get(server.url("/large")),
            &Auth::None,
            Slot::Chat,
            PresetId::Custom,
            || true,
        )
        .await
        .unwrap();
        let error = read_capped(response, 5, Slot::Chat, PresetId::Custom, || true)
            .await
            .unwrap_err();
        assert_eq!(error.reason, Reason::TooLarge);

        let response = send(
            client.get(server.url("/exact")),
            &Auth::None,
            Slot::Chat,
            PresetId::Custom,
            || true,
        )
        .await
        .unwrap();
        let body = read_capped(response, 6, Slot::Chat, PresetId::Custom, || true)
            .await
            .unwrap();
        assert_eq!(body, Bytes::from_static(b"123456"));
    }

    #[tokio::test]
    async fn chunked_sse_is_reassembled_with_a_cap() {
        let server = FakeServer::start().await;
        server.script(
            reqwest::Method::GET,
            "/events",
            FakeResponse::sse(
                ["data: first\n\n", "data: second\n\n"],
                Duration::from_millis(2),
            ),
        );
        let response = send(
            client(ClientKind::Provider { read: None }).get(server.url("/events")),
            &Auth::None,
            Slot::Chat,
            PresetId::Custom,
            || true,
        )
        .await
        .unwrap();
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
        let body = read_capped(response, 1024, Slot::Chat, PresetId::Custom, || true)
            .await
            .unwrap();
        assert_eq!(&body[..], b"data: first\n\ndata: second\n\n");
    }
}
