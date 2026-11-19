//! Streamable HTTP client for the owner's MCP servers (ADR 0017 §"The pipeline").

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures_core::future::BoxFuture;
use pinna_http::{ClientKind, NetError, Secret, SseDecoder, reqwest};
use reqwest::{
    StatusCode,
    header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    ToolOutput, ToolSet, ToolSpec,
    jsonrpc::{Id, Payload, Response},
    namespaced,
};

const MAX_BODY: usize = 256 * 1024;
const MAX_TOOLS: usize = 32;
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const SESSION_HEADER: HeaderName = HeaderName::from_static("mcp-session-id");
const PROTOCOL_HEADER: HeaderName = HeaderName::from_static("mcp-protocol-version");
const ACCEPT_HEADER: HeaderValue = HeaderValue::from_static("application/json, text/event-stream");

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum McpError {
    #[error("The MCP request timed out.")]
    Timeout,
    #[error("The MCP server is unreachable.")]
    Unreachable,
    #[error("The MCP server's TLS connection failed.")]
    Tls,
    #[error("The MCP server returned HTTP {0}.")]
    Http(u16),
    #[error("The MCP server returned JSON-RPC error {0}.")]
    Remote(i32),
    #[error("The MCP server returned an invalid response.")]
    Protocol,
    #[error("The MCP server response is too large.")]
    TooLarge,
}

fn network(error: reqwest::Error) -> McpError {
    match pinna_http::classify(&error) {
        NetError::Timeout => McpError::Timeout,
        NetError::Unreachable => McpError::Unreachable,
        NetError::Tls => McpError::Tls,
        NetError::Protocol => McpError::Protocol,
    }
}

#[derive(Default)]
struct Session {
    initialized: bool,
    generation: u64,
    id: Option<String>,
    /// The version the server agreed to; every request after `initialize`
    /// names it.
    protocol: &'static str,
}

pub struct McpClient {
    http: reqwest::Client,
    url: Secret,
    session: AsyncMutex<Session>,
    next_id: AtomicU64,
}

impl McpClient {
    pub fn new(url: Secret) -> Self {
        Self {
            http: pinna_http::client(ClientKind::Mcp),
            url,
            session: AsyncMutex::new(Session::default()),
            next_id: AtomicU64::new(1),
        }
    }

    pub async fn list(&self, within: Duration) -> Result<Vec<ToolSpec>, McpError> {
        tokio::time::timeout(within, self.list_inner())
            .await
            .map_err(|_| McpError::Timeout)?
    }

    async fn list_inner(&self) -> Result<Vec<ToolSpec>, McpError> {
        let result = self.rpc("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(McpError::Protocol)?;
        tools
            .iter()
            .take(MAX_TOOLS)
            .cloned()
            .map(|value| {
                serde_json::from_value::<ListedTool>(value)
                    .map(|tool| ToolSpec {
                        name: tool.name,
                        description: tool.description,
                        input_schema: tool.input_schema,
                    })
                    .map_err(|_| McpError::Protocol)
            })
            .collect()
    }

    pub async fn call(&self, tool: &str, args: Value, within: Duration) -> ToolOutput {
        match tokio::time::timeout(within, self.call_inner(tool, args)).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => ToolOutput::error(error.to_string()),
            Err(_) => ToolOutput::error(McpError::Timeout.to_string()),
        }
    }

    async fn call_inner(&self, tool: &str, args: Value) -> Result<ToolOutput, McpError> {
        let result = self
            .rpc("tools/call", json!({"name": tool, "arguments": args}))
            .await?;
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .ok_or(McpError::Protocol)?;
        let mut text = String::new();
        for item in content {
            if item.get("type").and_then(Value::as_str) != Some("text") {
                continue;
            }
            let part = item
                .get("text")
                .and_then(Value::as_str)
                .ok_or(McpError::Protocol)?;
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(part);
        }
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            Ok(ToolOutput::error(text))
        } else {
            Ok(ToolOutput::ok(text))
        }
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, McpError> {
        self.ensure_initialized().await?;
        let (generation, result) = self.rpc_once(method, &params).await;
        match result {
            Err(McpError::Http(status)) if status == StatusCode::NOT_FOUND.as_u16() => {
                {
                    let mut session = self.session.lock().await;
                    if session.generation == generation {
                        session.initialized = false;
                    }
                }
                self.ensure_initialized().await?;
                self.rpc_once(method, &params).await.1
            }
            result => result,
        }
    }

    async fn rpc_once(&self, method: &str, params: &Value) -> (u64, Result<Value, McpError>) {
        let session = self.session.lock().await;
        let generation = session.generation;
        let id = session.id.clone();
        let protocol = session.protocol;
        drop(session);
        let result = self
            .exchange(method, params.clone(), id.as_deref(), Some(protocol))
            .await
            .map(|(result, _)| result);
        (generation, result)
    }

    async fn ensure_initialized(&self) -> Result<(), McpError> {
        let mut session = self.session.lock().await;
        if session.initialized {
            return Ok(());
        }
        let (result, id) = self
            .exchange(
                "initialize",
                json!({
                    "protocolVersion": crate::PROTOCOL_VERSIONS[0],
                    "capabilities": {},
                    "clientInfo": {"name": "pinna", "version": pinna_version::VERSION},
                }),
                None,
                None,
            )
            .await?;
        // A server may answer with an older version it shares with us.
        let protocol = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .and_then(|v| crate::PROTOCOL_VERSIONS.iter().copied().find(|s| *s == v))
            .ok_or(McpError::Protocol)?;
        self.notify_initialized(id.as_deref(), protocol).await?;
        session.id = id;
        session.protocol = protocol;
        session.generation = session.generation.wrapping_add(1);
        session.initialized = true;
        Ok(())
    }

    async fn notify_initialized(
        &self,
        session: Option<&str>,
        protocol: &str,
    ) -> Result<(), McpError> {
        let response = self
            .post(
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                session,
                Some(protocol),
            )
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(McpError::Http(response.status().as_u16()))
        }
    }

    async fn exchange(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        protocol: Option<&str>,
    ) -> Result<(Value, Option<String>), McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .post(
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
                session,
                protocol,
            )
            .await?;
        if !response.status().is_success() {
            return Err(McpError::Http(response.status().as_u16()));
        }
        let new_session = response
            .headers()
            .get(SESSION_HEADER)
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|_| McpError::Protocol)
            })
            .transpose()?;
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .unwrap_or_default();
        let response = if content_type.eq_ignore_ascii_case("application/json") {
            read_json(response, id).await?
        } else if content_type.eq_ignore_ascii_case("text/event-stream") {
            read_sse(response, id).await?
        } else {
            return Err(McpError::Protocol);
        };
        if response.id != Id::Number(id.into()) {
            return Err(McpError::Protocol);
        }
        match response.payload {
            Payload::Success { result } => Ok((result, new_session)),
            Payload::Failure { error } => Err(McpError::Remote(error.code)),
        }
    }

    async fn post(
        &self,
        body: Value,
        session: Option<&str>,
        protocol: Option<&str>,
    ) -> Result<reqwest::Response, McpError> {
        let mut request = self
            .http
            .post(self.url.expose())
            .header(ACCEPT, ACCEPT_HEADER)
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(session) = session {
            request = request.header(SESSION_HEADER, session);
        }
        if let Some(protocol) = protocol {
            request = request.header(PROTOCOL_HEADER, protocol);
        }
        request.send().await.map_err(network)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedTool {
    name: String,
    #[serde(default)]
    description: String,
    input_schema: Value,
}

fn decode_response(value: Value, id: u64) -> Result<Option<Response>, McpError> {
    if value.get("method").is_some() || value.get("id") != Some(&Value::from(id)) {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| McpError::Protocol)
}

async fn read_json(mut response: reqwest::Response, id: u64) -> Result<Response, McpError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if body.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(McpError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| McpError::Protocol)?;
    decode_response(value, id)?.ok_or(McpError::Protocol)
}

async fn read_sse(mut response: reqwest::Response, id: u64) -> Result<Response, McpError> {
    let mut decoder = SseDecoder::new(MAX_BODY);
    let mut read = 0usize;
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        read = read.saturating_add(chunk.len());
        if read > MAX_BODY {
            return Err(McpError::TooLarge);
        }
        let events = decoder.push(&chunk).map_err(|_| McpError::TooLarge)?;
        for event in events {
            let value: Value = serde_json::from_str(&event.data).map_err(|_| McpError::Protocol)?;
            if let Some(response) = decode_response(value, id)? {
                return Ok(response);
            }
        }
    }
    if let Some(event) = decoder.finish() {
        let value: Value = serde_json::from_str(&event.data).map_err(|_| McpError::Protocol)?;
        if let Some(response) = decode_response(value, id)? {
            return Ok(response);
        }
    }
    Err(McpError::Protocol)
}

/// How long a failed listing is remembered: a server that is down costs one
/// listing timeout per this, not one per request.
const FAILED_TTL: Duration = Duration::from_secs(30);

/// The last listing, with the server's tool names as the model sees them.
struct CachedTools {
    fetched: Instant,
    outcome: Result<Arc<[RemoteTool]>, McpError>,
}

/// One remote tool: its spec under the namespaced name, and the name the
/// server knows it by.
struct RemoteTool {
    spec: ToolSpec,
    remote: String,
}

pub struct McpToolSet {
    server: String,
    client: Arc<McpClient>,
    cache: Mutex<Option<CachedTools>>,
    refresh: AsyncMutex<()>,
}

impl McpToolSet {
    pub fn new(server: impl Into<String>, client: Arc<McpClient>) -> Self {
        Self {
            server: server.into(),
            client,
            cache: Mutex::new(None),
            refresh: AsyncMutex::new(()),
        }
    }

    fn cached(&self) -> Option<Result<Arc<[RemoteTool]>, McpError>> {
        self.cache
            .lock()
            .unwrap()
            .as_ref()
            .filter(|cached| {
                let ttl = if cached.outcome.is_ok() {
                    CACHE_TTL
                } else {
                    FAILED_TTL
                };
                cached.fetched.elapsed() < ttl
            })
            .map(|cached| cached.outcome.clone())
    }

    async fn remote_tools(&self) -> Result<Arc<[RemoteTool]>, McpError> {
        if let Some(outcome) = self.cached() {
            return outcome;
        }
        let _refresh = self.refresh.lock().await;
        if let Some(outcome) = self.cached() {
            return outcome;
        }
        let outcome = self.client.list(LIST_TIMEOUT).await.map(|tools| {
            tools
                .into_iter()
                .filter_map(|mut spec| {
                    let remote = spec.name;
                    spec.name = namespaced(&self.server, &remote)?;
                    Some(RemoteTool { spec, remote })
                })
                .collect::<Arc<[RemoteTool]>>()
        });
        self.cache.lock().unwrap().replace(CachedTools {
            fetched: Instant::now(),
            outcome: outcome.clone(),
        });
        outcome
    }

    /// How the last listing went: the tools as the model sees them, or why
    /// it failed. `None` before the first. It stays after the cache expires:
    /// it is what the server last said.
    pub fn last_listing(&self) -> Option<Result<Vec<ToolSpec>, McpError>> {
        self.cache.lock().unwrap().as_ref().map(|cached| {
            cached
                .outcome
                .clone()
                .map(|tools| tools.iter().map(|tool| tool.spec.clone()).collect())
        })
    }

    /// Ask the server again now, whatever the cache holds.
    pub async fn refresh(&self) -> Result<Vec<ToolSpec>, McpError> {
        if let Some(cached) = self.cache.lock().unwrap().as_mut() {
            cached.fetched = Instant::now() - CACHE_TTL.max(FAILED_TTL);
        }
        self.list().await
    }

    /// The server's tools as the model sees them, or why it can't be listed.
    /// Both outcomes are cached, a failure for a shorter time.
    pub async fn list(&self) -> Result<Vec<ToolSpec>, McpError> {
        Ok(self
            .remote_tools()
            .await?
            .iter()
            .map(|tool| tool.spec.clone())
            .collect())
    }
}

impl ToolSet for McpToolSet {
    fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
        Box::pin(async move { self.list().await.unwrap_or_default() })
    }

    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let tools = match self.remote_tools().await {
                Ok(tools) => tools,
                Err(error) => return ToolOutput::error(error.to_string()),
            };
            match tools.iter().find(|tool| tool.spec.name == name) {
                Some(tool) => self.client.call(&tool.remote, args, CALL_TIMEOUT).await,
                None => ToolOutput::error(format!("Unknown MCP tool {name}.")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::atomic::{AtomicBool, AtomicUsize},
    };

    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{HeaderMap, StatusCode},
        response::Response,
        routing::post,
    };
    use futures_util::stream;
    use tokio::{net::TcpListener, task::JoinHandle};

    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Wire {
        Json,
        Sse,
    }

    #[derive(Clone)]
    struct Recorded {
        method: String,
        session: Option<String>,
        protocol: Option<String>,
        accept: Option<String>,
        body: Value,
    }

    struct FakeState {
        wire: Wire,
        requests: Mutex<Vec<Recorded>>,
        initialize_count: AtomicUsize,
        list_count: AtomicUsize,
        missing_session_once: AtomicBool,
        slow_list: AtomicBool,
        oversized_list: AtomicBool,
        long_output: AtomicBool,
        tool_count: AtomicUsize,
        /// The protocol version `initialize` answers with.
        version: Mutex<&'static str>,
    }

    impl FakeState {
        fn new(wire: Wire) -> Self {
            Self {
                wire,
                requests: Mutex::new(Vec::new()),
                initialize_count: AtomicUsize::new(0),
                list_count: AtomicUsize::new(0),
                missing_session_once: AtomicBool::new(false),
                slow_list: AtomicBool::new(false),
                oversized_list: AtomicBool::new(false),
                long_output: AtomicBool::new(false),
                tool_count: AtomicUsize::new(1),
                version: Mutex::new(crate::PROTOCOL_VERSIONS[0]),
            }
        }

        fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
    }

    struct FakeServer {
        url: String,
        state: Arc<FakeState>,
        task: JoinHandle<()>,
    }

    impl FakeServer {
        async fn start(wire: Wire) -> Self {
            let state = Arc::new(FakeState::new(wire));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let app = Router::new()
                .route("/mcp", post(handle))
                .with_state(state.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self { url, state, task }
        }

        fn client(&self) -> McpClient {
            McpClient::new(Secret::new(self.url.clone()))
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn handle(
        State(state): State<Arc<FakeState>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let body: Value = serde_json::from_slice(&body).unwrap();
        let method = body["method"].as_str().unwrap().to_owned();
        state.requests.lock().unwrap().push(Recorded {
            method: method.clone(),
            session: headers
                .get(SESSION_HEADER)
                .map(|value| value.to_str().unwrap().to_owned()),
            protocol: headers
                .get(PROTOCOL_HEADER)
                .map(|value| value.to_str().unwrap().to_owned()),
            accept: headers
                .get(ACCEPT)
                .map(|value| value.to_str().unwrap().to_owned()),
            body: body.clone(),
        });
        if method == "notifications/initialized" {
            return StatusCode::ACCEPTED.into_response();
        }
        if method == "tools/list" && state.missing_session_once.swap(false, Ordering::Relaxed) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if method == "tools/list" && state.slow_list.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        let id = body["id"].clone();
        let result = match method.as_str() {
            "initialize" => {
                state.initialize_count.fetch_add(1, Ordering::Relaxed);
                json!({
                    "protocolVersion": *state.version.lock().unwrap(),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1"},
                })
            }
            "tools/list" => {
                state.list_count.fetch_add(1, Ordering::Relaxed);
                let description = if state.oversized_list.load(Ordering::Relaxed) {
                    "x".repeat(MAX_BODY + 1)
                } else {
                    "Local time".into()
                };
                let tools = (0..state.tool_count.load(Ordering::Relaxed))
                    .map(|index| {
                        json!({
                            "name": if index == 0 { "time".into() } else { format!("tool_{index}") },
                            "description": description.clone(),
                            "inputSchema": {"type": "object"},
                        })
                    })
                    .collect::<Vec<_>>();
                json!({"tools": tools})
            }
            "tools/call" => {
                assert_eq!(body["params"]["name"], "time");
                let text = if state.long_output.load(Ordering::Relaxed) {
                    format!("{}é", "a".repeat(crate::MAX_TOOL_OUTPUT - 1))
                } else {
                    format!(
                        "noon {}",
                        body["params"]["arguments"]["zone"].as_str().unwrap()
                    )
                };
                json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": false,
                })
            }
            _ => unreachable!("unexpected method {method}"),
        };
        reply(
            &state,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        )
    }

    fn reply(state: &FakeState, value: Value) -> Response {
        let mut builder = Response::builder().header(SESSION_HEADER, "test-session");
        match state.wire {
            Wire::Json => builder
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap(),
            Wire::Sse => {
                let event = format!("event: message\ndata: {value}\n\n");
                let chunks = event
                    .as_bytes()
                    .chunks(7)
                    .map(|chunk| Ok::<_, Infallible>(Bytes::copy_from_slice(chunk)))
                    .collect::<Vec<_>>();
                builder = builder.header(CONTENT_TYPE, "text/event-stream");
                builder
                    .body(Body::from_stream(stream::iter(chunks)))
                    .unwrap()
            }
        }
    }

    use axum::response::IntoResponse;

    #[tokio::test]
    async fn json_and_sse_replies_set_up_a_session_and_call_tools() {
        for wire in [Wire::Json, Wire::Sse] {
            let server = FakeServer::start(wire).await;
            let client = server.client();
            let specs = client.list(Duration::from_secs(1)).await.unwrap();
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].name, "time");
            let output = client
                .call("time", json!({"zone": "UTC"}), Duration::from_secs(1))
                .await;
            assert_eq!(output, ToolOutput::ok("noon UTC"));

            let requests = server.state.requests();
            assert_eq!(
                requests
                    .iter()
                    .map(|r| r.method.as_str())
                    .collect::<Vec<_>>(),
                [
                    "initialize",
                    "notifications/initialized",
                    "tools/list",
                    "tools/call",
                ]
            );
            assert!(requests[0].session.is_none());
            assert!(requests[0].protocol.is_none());
            assert!(
                requests[1..]
                    .iter()
                    .all(|request| request.session.as_deref() == Some("test-session"))
            );
            assert!(
                requests[1..]
                    .iter()
                    .all(|request| request.protocol.as_deref() == Some(crate::PROTOCOL_VERSIONS[0]))
            );
            assert!(
                requests
                    .iter()
                    .all(|request| request.accept.as_deref()
                        == Some(ACCEPT_HEADER.to_str().unwrap()))
            );
            assert_eq!(
                requests[0].body["params"]["protocolVersion"],
                crate::PROTOCOL_VERSIONS[0]
            );
        }
    }

    #[tokio::test]
    async fn a_server_on_the_older_protocol_is_used_with_it() {
        let server = FakeServer::start(Wire::Json).await;
        *server.state.version.lock().unwrap() = "2025-03-26";
        let client = server.client();
        assert_eq!(client.list(Duration::from_secs(1)).await.unwrap().len(), 1);
        let requests = server.state.requests();
        assert!(
            requests[1..]
                .iter()
                .all(|request| request.protocol.as_deref() == Some("2025-03-26"))
        );

        let server = FakeServer::start(Wire::Json).await;
        *server.state.version.lock().unwrap() = "1999-01-01";
        assert_eq!(
            server
                .client()
                .list(Duration::from_secs(1))
                .await
                .unwrap_err(),
            McpError::Protocol
        );
    }

    #[tokio::test]
    async fn a_missing_session_reinitializes_once_and_retries() {
        let server = FakeServer::start(Wire::Json).await;
        server
            .state
            .missing_session_once
            .store(true, Ordering::Relaxed);
        let client = server.client();
        assert_eq!(client.list(Duration::from_secs(1)).await.unwrap().len(), 1);
        assert_eq!(server.state.initialize_count.load(Ordering::Relaxed), 2);
        assert_eq!(server.state.list_count.load(Ordering::Relaxed), 1);
        let methods = server
            .state
            .requests()
            .into_iter()
            .map(|request| request.method)
            .collect::<Vec<_>>();
        assert_eq!(
            methods,
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "initialize",
                "notifications/initialized",
                "tools/list",
            ]
        );
    }

    #[tokio::test]
    async fn a_slow_list_is_empty_within_two_seconds() {
        let server = FakeServer::start(Wire::Json).await;
        server.state.slow_list.store(true, Ordering::Relaxed);
        let tools = McpToolSet::new("home", Arc::new(server.client()));
        let started = Instant::now();
        assert!(
            tokio::time::timeout(Duration::from_millis(2_500), tools.specs())
                .await
                .expect("list exceeded its bound")
                .is_empty()
        );
        assert!(started.elapsed() >= LIST_TIMEOUT);
        server.task.abort();
    }

    #[tokio::test]
    async fn oversized_bodies_and_tool_output_are_bounded() {
        let server = FakeServer::start(Wire::Json).await;
        server.state.oversized_list.store(true, Ordering::Relaxed);
        let client = server.client();
        assert!(matches!(
            client.list(Duration::from_secs(1)).await,
            Err(McpError::TooLarge)
        ));

        let server = FakeServer::start(Wire::Json).await;
        server.state.long_output.store(true, Ordering::Relaxed);
        let output = server
            .client()
            .call("time", json!({"zone": "UTC"}), Duration::from_secs(1))
            .await;
        assert_eq!(output.text, "a".repeat(crate::MAX_TOOL_OUTPUT - 1));
    }

    #[tokio::test]
    async fn tool_specs_are_namespaced_capped_and_cached() {
        let server = FakeServer::start(Wire::Json).await;
        server
            .state
            .tool_count
            .store(MAX_TOOLS + 1, Ordering::Relaxed);
        let client = Arc::new(server.client());
        let tools = McpToolSet::new("home-2", client);
        assert_eq!(tools.last_listing(), None);
        let specs = tools.specs().await;
        assert_eq!(specs.len(), MAX_TOOLS);
        assert_eq!(tools.last_listing().unwrap().unwrap().len(), MAX_TOOLS);
        assert_eq!(specs[0].name, "home_2_time");
        // The listing names each tool as the model sees it, which is the
        // name of its switch.
        assert_eq!(tools.last_listing().unwrap().unwrap()[0], specs[0]);
        assert_eq!(
            tools.call("home_2_time", json!({"zone": "UTC"})).await,
            ToolOutput::ok("noon UTC")
        );
        assert!(tools.specs().await.len() == MAX_TOOLS);
        assert_eq!(server.state.list_count.load(Ordering::Relaxed), 1);

        tools.cache.lock().unwrap().as_mut().unwrap().fetched = Instant::now() - CACHE_TTL;
        // What the server last said stays while the cache has expired.
        assert_eq!(tools.last_listing().unwrap().unwrap().len(), MAX_TOOLS);
        assert_eq!(tools.specs().await.len(), MAX_TOOLS);
        assert_eq!(server.state.list_count.load(Ordering::Relaxed), 2);
        // A refresh asks again even while the cache is fresh.
        tools.refresh().await.unwrap();
        assert_eq!(server.state.list_count.load(Ordering::Relaxed), 3);
        assert_eq!(
            tools.call("home_2_unknown", json!({})).await,
            ToolOutput::error("Unknown MCP tool home_2_unknown.")
        );
    }
}
