//! Shared provider HTTP: where an adapter sends ([`Endpoint`]),
//! authentication, status mapping and capped body reads.
//!
//! Errors contain only a slot, preset and bounded reason. In particular, a
//! reqwest error or response body is never formatted into a [`ProviderError`].

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
    let response = request
        .send()
        .await
        .map_err(|error| network_error(slot, preset, &error, clock_synced))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        drop(response);
        return Err(ProviderError::new(slot, preset, Reason::Http(status)));
    }
    Ok(response)
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
