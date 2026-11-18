//! Pinna's MCP server, `/mcp` on the dashboard listener (ADR 0017
//! §"Pinna as an MCP server").
//!
//! It trusts the LAN as the dashboard does: no token, and the dashboard's Host
//! allowlist applies. On top of that a browser `Origin` must be one the
//! dashboard answers to, and only `application/json` bodies are taken, so a
//! web page can't post a form here. Replies are JSON; there is no event
//! stream, so GET is refused.
//!
//! The tools are the device tools on the MCP surface. None of them opens the
//! microphone.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequest, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use futures::future::BoxFuture;
use pinna_mcp::jsonrpc::{self, Id, Incoming};
use pinna_mcp::server::{self, ServerInfo};
use pinna_mcp::{ToolOutput, ToolSet, ToolSpec};
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc, watch};

use crate::settings::Mode;
use crate::standalone::AssistLive;
use crate::standalone::service::Service;
use crate::tools::{self, Caller, DeviceTools, Surface, ToolKit};
use crate::voice::{Input, Snapshot};
use crate::web::{Conn, Hosts};

pub const ROUTE: &str = "/mcp";
/// The largest request body.
const MAX_BODY: usize = 64 * 1024;
/// Tool calls that may run at once; another is told the satellite is busy.
const MAX_CALLS: usize = 2;
/// JSON-RPC's implementation-defined server error, which MCP servers also use
/// for transport refusals.
const SERVER_ERROR: i32 = -32000;

const HOME_ASSISTANT_MODE: &str =
    "Pinna answers MCP in standalone mode only. Switch it on under Assistant.";
const POST_ONLY: &str = "Pinna's MCP server takes POST requests only.";
const NOT_JSON: &str = "Send MCP requests with Content-Type application/json.";
const FOREIGN_ORIGIN: &str = "Web pages from this origin may not call Pinna's MCP server.";
const TOO_LARGE: &str = "The request is larger than 64 KiB.";
const UNREADABLE: &str = "The request body couldn't be read.";

/// What every `/mcp` request shares.
pub struct McpServer {
    service: Arc<Service>,
    live: watch::Receiver<Arc<AssistLive>>,
    inputs: mpsc::Sender<Input>,
    status: watch::Receiver<Snapshot>,
    calls: Semaphore,
    info: ServerInfo,
    kit: ToolKit,
}

impl McpServer {
    pub fn new(
        service: Arc<Service>,
        inputs: mpsc::Sender<Input>,
        status: watch::Receiver<Snapshot>,
        kit: ToolKit,
        version: impl Into<String>,
    ) -> Self {
        Self {
            live: service.live(),
            service,
            inputs,
            status,
            kit,
            calls: Semaphore::new(MAX_CALLS),
            info: ServerInfo {
                name: "pinna".into(),
                version: version.into(),
            },
        }
    }

    /// The tools as the owner switched them now.
    fn tools(&self) -> DeviceTools {
        DeviceTools::new(
            &self.kit,
            self.inputs.clone(),
            self.status.clone(),
            self.live.borrow().clone(),
            Surface::Mcp,
            Caller::Mcp,
        )
    }
}

#[derive(Clone)]
struct Endpoint {
    server: Arc<McpServer>,
    hosts: Hosts,
}

/// The `/mcp` route. `hosts` are the names the dashboard answers to, for the
/// Origin check.
pub fn route(server: Arc<McpServer>, hosts: Hosts) -> MethodRouter {
    axum::routing::any(serve)
        .with_state(Endpoint { server, hosts })
        .layer(DefaultBodyLimit::max(MAX_BODY))
}

async fn serve(State(endpoint): State<Endpoint>, req: Request) -> Response {
    if req.method() != Method::POST {
        let mut res = refusal(StatusCode::METHOD_NOT_ALLOWED, Id::Null, POST_ONLY);
        res.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return res;
    }
    if !origin_allowed(&endpoint.hosts, req.headers()) {
        return refusal(StatusCode::FORBIDDEN, Id::Null, FOREIGN_ORIGIN);
    }
    if !is_json(req.headers()) {
        return refusal(StatusCode::UNSUPPORTED_MEDIA_TYPE, Id::Null, NOT_JSON);
    }
    let peer = req
        .extensions()
        .get::<ConnectInfo<Conn>>()
        .map(|c| c.0.peer.ip().to_canonical());
    let body = match Bytes::from_request(req, &()).await {
        Ok(body) => body,
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return refusal(StatusCode::PAYLOAD_TOO_LARGE, Id::Null, TOO_LARGE);
        }
        Err(_) => return refusal(StatusCode::BAD_REQUEST, Id::Null, UNREADABLE),
    };
    let server = &endpoint.server;
    if server.status.borrow().mode != Mode::Standalone {
        return home_assistant_mode(&body);
    }

    let tools = Limited {
        tools: server.tools(),
        calls: &server.calls,
    };
    let handled = server::handle(&tools, &server.info, &body).await;
    if let (Some(name), Some(peer)) = (handled.client_name, peer) {
        server.service.agent_seen(name, peer);
    }
    match handled.reply {
        Some(reply) => json(StatusCode::OK, reply),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Every Origin header, if any, names a host the dashboard answers to.
/// Clients outside a browser send none.
fn origin_allowed(hosts: &Hosts, headers: &HeaderMap) -> bool {
    headers.get_all(header::ORIGIN).iter().all(|origin| {
        origin
            .to_str()
            .ok()
            .and_then(|o| url::Url::parse(o).ok())
            .and_then(|u| u.host_str().map(|h| hosts.allows(h)))
            .unwrap_or(false)
    })
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("application/json"))
}

/// The refusal in Home Assistant mode, answering the request's id. A
/// notification needs no answer, and does nothing in either mode.
fn home_assistant_mode(body: &[u8]) -> Response {
    let id = match jsonrpc::parse(body) {
        Ok(Incoming::Single(req)) => match req.id {
            Some(id) => id,
            None => return StatusCode::ACCEPTED.into_response(),
        },
        _ => Id::Null,
    };
    refusal(StatusCode::OK, id, HOME_ASSISTANT_MODE)
}

fn refusal(status: StatusCode, id: Id, message: &str) -> Response {
    json(status, pinna_mcp::server::error(id, SERVER_ERROR, message))
}

fn json(status: StatusCode, body: Vec<u8>) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// The device tools with at most [`MAX_CALLS`] calls running at once.
struct Limited<'a> {
    tools: DeviceTools,
    calls: &'a Semaphore,
}

impl ToolSet for Limited<'_> {
    fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
        self.tools.specs()
    }

    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let Ok(_permit) = self.calls.try_acquire() else {
                return ToolOutput::error(tools::BUSY);
            };
            self.tools.call(name, args).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, SocketAddr};
    use std::path::Path;

    use axum::body::Body;
    use pinna_esphome::{DeviceEvent, EsphomeHandle};
    use prometheus_client::registry::Registry;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::api::{DeviceApi, Info};
    use crate::leds::LedState;
    use crate::settings::Settings;
    use crate::standalone::service::Paths;
    use crate::voice::{ApiCommand, Brain, Defaults, Options};
    use crate::{boot, web};

    const HOST: &str = "pinna.local";

    /// `/mcp` on the dashboard routes, over whatever answers `inputs`.
    struct Fixture {
        app: axum::Router,
        service: Arc<Service>,
        inputs: mpsc::Sender<Input>,
        status: watch::Receiver<Snapshot>,
        _events: Option<mpsc::Receiver<DeviceEvent>>,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        fn over(
            inputs: mpsc::Sender<Input>,
            status: watch::Receiver<Snapshot>,
            dir: tempfile::TempDir,
        ) -> Self {
            let service = Service::load(
                Paths::beside(&dir.path().join("settings.toml")),
                status.clone(),
            );
            let server = McpServer::new(
                service.clone(),
                inputs.clone(),
                status.clone(),
                crate::tools::ToolKit::new(crate::metrics::Counters::default()),
                "0.1.0",
            );
            let api = Arc::new(DeviceApi::new(info(), status.clone(), inputs.clone()));
            let hosts = Hosts::new(Some("pinna"), &[]);
            let assistant = Arc::new(crate::assistant_api::AssistantApi::new(
                service.clone(),
                inputs.clone(),
            ));
            let app = web::app(
                api,
                assistant,
                Path::new("/nonexistent"),
                hosts,
                None,
                Arc::new(server),
                Arc::new(Registry::default()),
            );
            Self {
                app,
                service,
                inputs,
                status,
                _events: None,
                _dir: dir,
            }
        }

        /// Over a bare channel in `mode`, to see exactly what reaches the Brain.
        fn bare(mode: Mode) -> (Self, mpsc::Receiver<Input>) {
            let (inputs, rx) = mpsc::channel(64);
            let status = watch::channel(Snapshot {
                mode,
                ..Default::default()
            })
            .1;
            let dir = tempfile::tempdir().unwrap();
            (Self::over(inputs, status, dir), rx)
        }

        /// Over a real Brain on its own task in standalone mode.
        async fn device() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (esp, events) = EsphomeHandle::detached(256);
            let (status_tx, mut status) = watch::channel(Snapshot::default());
            let (inputs, rx) = mpsc::channel(64);
            let brain = Brain::new(
                esp,
                None,
                Arc::default(),
                watch::channel(LedState::default()).0,
                status_tx,
                inputs.clone(),
                Options {
                    key_file: dir.path().join("key"),
                    settings_file: dir.path().join("settings.toml"),
                    wake_words: crate::wakewords::test_catalogue(),
                    active_wake_words: vec!["alexa_v0.1".into()],
                    keywords: None,
                    defaults: Defaults {
                        node_name: "pinna-test".into(),
                        friendly_name: "Test".into(),
                        default_name: true,
                        wake_sound: false,
                        update_channel: crate::updates::UpdateChannel::Off,
                    },
                    updates: None,
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    airplay: false,
                    bluetooth: false,
                    settings: Settings {
                        mode: Mode::Standalone,
                        ..Settings::default()
                    },
                },
            );
            tokio::spawn(brain.run(rx));
            status
                .wait_for(|s| s.name == "pinna-test")
                .await
                .expect("the Brain published no snapshot");
            Self {
                _events: Some(events),
                ..Self::over(inputs, status, dir)
            }
        }

        async fn send(&self, req: axum::http::Request<Body>) -> Reply {
            send(&self.app, req).await
        }
    }

    fn info() -> Info {
        Info {
            version: "0.1.0".into(),
            mac: "02:00:00:aa:bb:cc".into(),
            time_zones: Default::default(),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: String::new(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
            key_from_image: false,
        }
    }

    struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    impl Reply {
        fn json(&self) -> Value {
            assert_eq!(self.headers[header::CONTENT_TYPE], "application/json");
            serde_json::from_slice(&self.body).unwrap()
        }

        /// The sentence of a JSON-RPC error.
        fn refusal(&self) -> String {
            let reply = self.json();
            assert_eq!(reply["error"]["code"], SERVER_ERROR, "{reply}");
            reply["error"]["message"].as_str().unwrap().to_owned()
        }
    }

    async fn send(app: &axum::Router, req: axum::http::Request<Body>) -> Reply {
        let res = app.clone().oneshot(req).await.unwrap();
        let (parts, body) = res.into_parts();
        Reply {
            status: parts.status,
            headers: parts.headers,
            body: axum::body::to_bytes(body, 1 << 20).await.unwrap().to_vec(),
        }
    }

    /// A JSON POST to `/mcp` from a client outside a browser.
    fn raw(body: Vec<u8>) -> axum::http::Request<Body> {
        axum::http::Request::post(ROUTE)
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    /// A request, or a notification when `id` is null.
    fn rpc(method: &str, id: Value, params: Value) -> axum::http::Request<Body> {
        let mut wire = json!({"jsonrpc": "2.0", "method": method, "params": params});
        if !id.is_null() {
            wire["id"] = id;
        }
        raw(serde_json::to_vec(&wire).unwrap())
    }

    fn call(id: u64, tool: &str, args: Value) -> axum::http::Request<Body> {
        rpc(
            "tools/call",
            json!(id),
            json!({"name": tool, "arguments": args}),
        )
    }

    fn initialize(name: &str) -> axum::http::Request<Body> {
        rpc(
            "initialize",
            json!(1),
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": name, "version": "1.0"},
            }),
        )
    }

    fn from_peer(mut req: axum::http::Request<Body>, peer: &str) -> axum::http::Request<Body> {
        let peer: SocketAddr = peer.parse().unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(Conn { peer, local: None }));
        req
    }

    #[tokio::test]
    async fn mcp_takes_only_json_bodies() {
        let (f, mut rx) = Fixture::bare(Mode::Standalone);
        let ping = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        for content_type in [Some("text/plain"), Some("application/jsonl"), None] {
            let mut req = axum::http::Request::post(ROUTE).header(header::HOST, HOST);
            if let Some(ct) = content_type {
                req = req.header(header::CONTENT_TYPE, ct);
            }
            let reply = f.send(req.body(Body::from(&ping[..])).unwrap()).await;
            assert_eq!(
                reply.status,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{content_type:?}"
            );
            assert_eq!(reply.refusal(), NOT_JSON);
        }

        let req = axum::http::Request::post(ROUTE)
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "Application/JSON; charset=utf-8")
            .body(Body::from(&ping[..]))
            .unwrap();
        let reply = f.send(req).await;

        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(
            reply.json(),
            json!({"jsonrpc": "2.0", "id": 1, "result": {}})
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn mcp_refuses_a_web_page_from_another_origin() {
        let (f, _rx) = Fixture::bare(Mode::Standalone);
        for origin in [
            "http://evil.example",
            "null",
            "https://pinna.local.evil.example",
        ] {
            let mut req = rpc("ping", json!(1), json!({}));
            req.headers_mut()
                .insert(header::ORIGIN, HeaderValue::from_static(origin));
            let reply = f.send(req).await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin}");
            assert_eq!(reply.refusal(), FOREIGN_ORIGIN);
        }
        for origin in [
            "http://pinna.local:8080",
            "http://192.168.1.5",
            "http://[fe80::1]",
        ] {
            let mut req = rpc("ping", json!(1), json!({}));
            req.headers_mut()
                .insert(header::ORIGIN, HeaderValue::from_static(origin));
            assert_eq!(f.send(req).await.status, StatusCode::OK, "{origin}");
        }
    }

    #[tokio::test]
    async fn mcp_answers_only_post() {
        let (f, _rx) = Fixture::bare(Mode::Standalone);
        for method in [Method::GET, Method::DELETE, Method::OPTIONS] {
            let req = axum::http::Request::builder()
                .method(method.clone())
                .uri(ROUTE)
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap();
            let reply = f.send(req).await;
            assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
            assert_eq!(reply.headers[header::ALLOW], "POST");
            assert_eq!(reply.refusal(), POST_ONLY);
        }
    }

    #[tokio::test]
    async fn mcp_inherits_the_dashboard_host_allowlist() {
        let (f, _rx) = Fixture::bare(Mode::Standalone);
        let mut req = rpc("ping", json!(1), json!({}));
        req.headers_mut()
            .insert(header::HOST, HeaderValue::from_static("evil.example"));

        let reply = f.send(req).await;

        assert_eq!(reply.status, StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn mcp_refuses_a_body_over_64_kib() {
        let (f, _rx) = Fixture::bare(Mode::Standalone);
        let padding = "x".repeat(MAX_BODY);
        let reply = f
            .send(rpc("ping", json!(1), json!({ "padding": padding })))
            .await;
        assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(reply.refusal(), TOO_LARGE);
    }

    #[tokio::test]
    async fn mcp_refuses_with_a_sentence_in_home_assistant_mode() {
        let (f, mut rx) = Fixture::bare(Mode::HomeAssistant);

        let reply = f
            .send(from_peer(initialize("helper"), "192.168.1.77:50000"))
            .await;

        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.json()["id"], 1);
        assert_eq!(reply.refusal(), HOME_ASSISTANT_MODE);
        let reply = f.send(call(2, "set_volume", json!({"percent": 30}))).await;
        assert_eq!(reply.json()["id"], 2);
        assert_eq!(reply.refusal(), HOME_ASSISTANT_MODE);
        let reply = f.send(raw(b"{".to_vec())).await;
        assert_eq!(reply.json()["id"], Value::Null);
        assert_eq!(reply.refusal(), HOME_ASSISTANT_MODE);
        let note = rpc("notifications/initialized", Value::Null, json!({}));
        let reply = f.send(note).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED);
        assert!(reply.body.is_empty());
        assert!(rx.try_recv().is_err());
        assert!(f.service.view().agents.is_empty());
    }

    #[tokio::test]
    async fn mcp_initializes_and_lists_the_mcp_tools() {
        let (f, _rx) = Fixture::bare(Mode::Standalone);
        let reply = f.send(initialize("helper")).await;
        assert_eq!(reply.status, StatusCode::OK);
        let reply = reply.json();
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(
            reply["result"]["serverInfo"],
            json!({"name": "pinna", "version": "0.1.0"})
        );
        let reply = f
            .send(rpc("notifications/initialized", Value::Null, json!({})))
            .await;
        assert_eq!(reply.status, StatusCode::ACCEPTED);

        let reply = f.send(rpc("tools/list", json!(2), json!({}))).await;

        assert_eq!(reply.status, StatusCode::OK);
        let listed: Vec<String> = reply.json()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        let (inputs, _) = mpsc::channel(1);
        let expected: Vec<String> = DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
            inputs,
            f.status.clone(),
            Arc::new(AssistLive::empty()),
            Surface::Mcp,
            Caller::Mcp,
        )
        .specs()
        .await
        .into_iter()
        .map(|s| s.name)
        .collect();
        assert_eq!(listed, expected);
        for name in ["status", "cancel_request", "set_timer"] {
            assert!(listed.iter().any(|n| n == name), "{name}");
        }
        assert!(!listed.iter().any(|n| n == tools::weather::NAME));
        // Nothing lets a client listen in.
        for word in ["unmute", "listen", "conversation", "record"] {
            assert!(!listed.iter().any(|n| n.contains(word)), "{word}");
        }
    }

    #[tokio::test]
    async fn mcp_set_timer_reaches_the_brain() {
        let f = Fixture::device().await;

        let reply = f
            .send(call(3, "set_timer", json!({"seconds": 300, "name": "Tea"})))
            .await;

        assert_eq!(reply.status, StatusCode::OK);
        let result = reply.json()["result"].clone();
        assert_eq!(result["isError"], false, "{result}");
        let timers = f.status.borrow().timers.clone();
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].name, "Tea");
        assert_eq!(
            result["content"][0]["text"],
            format!("Timer \"Tea\" set for 5 minutes (id {}).", timers[0].id)
        );
    }

    #[tokio::test]
    async fn mcp_is_busy_beyond_two_calls() {
        let (f, mut rx) = Fixture::bare(Mode::Standalone);
        let pending: Vec<_> = (0..MAX_CALLS as u64)
            .map(|id| {
                let app = f.app.clone();
                tokio::spawn(async move {
                    send(&app, call(id, "set_volume", json!({"percent": 30}))).await
                })
            })
            .collect();
        let mut replies = Vec::new();
        for _ in 0..MAX_CALLS {
            match rx.recv().await.unwrap() {
                Input::Api(ApiCommand::SetVolume(30, reply)) => replies.push(reply),
                other => panic!("unexpected input {other:?}"),
            }
        }

        let reply = f.send(call(9, "set_volume", json!({"percent": 30}))).await;

        assert_eq!(reply.status, StatusCode::OK);
        let result = reply.json()["result"].clone();
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], tools::BUSY);
        assert!(rx.try_recv().is_err());
        // Listing needs no call slot.
        let reply = f.send(rpc("tools/list", json!(10), json!({}))).await;
        assert!(reply.json()["result"]["tools"].is_array());

        for reply in replies {
            reply
                .send(Ok(Snapshot {
                    volume: 30,
                    ..Default::default()
                }))
                .unwrap();
        }
        for call in pending {
            let reply = call.await.unwrap().json();
            assert_eq!(reply["result"]["isError"], false, "{reply}");
        }
        let next = tokio::spawn({
            let app = f.app.clone();
            async move { send(&app, call(11, "stop_media", json!({}))).await }
        });
        match rx.recv().await.unwrap() {
            Input::Api(ApiCommand::ControlMedia(_, _, reply)) => {
                reply.send(Ok(Snapshot::default())).unwrap()
            }
            other => panic!("unexpected input {other:?}"),
        }
        assert_eq!(next.await.unwrap().json()["result"]["isError"], false);
    }

    #[tokio::test]
    async fn mcp_is_busy_while_the_brain_queue_is_nearly_full() {
        let (f, _rx) = Fixture::bare(Mode::Standalone);
        // Room for one more input, but the last ones are the device's.
        while f.inputs.capacity() > 1 {
            f.inputs.try_send(Input::Privacy(false)).unwrap();
        }

        let reply = f.send(call(1, "stop_media", json!({}))).await;

        let result = reply.json()["result"].clone();
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], tools::BUSY);
        assert_eq!(f.inputs.capacity(), 1);
    }

    #[tokio::test]
    async fn mcp_remembers_the_agents_it_sees() {
        let (f, _rx) = Fixture::bare(Mode::Standalone);

        f.send(from_peer(initialize("helper"), "192.168.1.77:50000"))
            .await;
        f.send(from_peer(
            initialize("other\u{7}"),
            "[::ffff:192.168.1.78]:50001",
        ))
        .await;
        // Only `initialize` names the client.
        f.send(from_peer(
            rpc("ping", json!(2), json!({})),
            "192.168.1.79:50002",
        ))
        .await;

        let agents = f.service.view().agents;
        let seen: Vec<(&str, IpAddr)> = agents.iter().map(|a| (a.name.as_str(), a.peer)).collect();
        assert_eq!(
            seen,
            [
                ("other", "192.168.1.78".parse::<IpAddr>().unwrap()),
                ("helper", "192.168.1.77".parse::<IpAddr>().unwrap()),
            ]
        );
    }
}
