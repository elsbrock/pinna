//! Dashboard listener (ADR 0005): the built Svelte dashboard and the same
//! ConnectRPC `DeviceService`, with standalone mode's `AssistantService`
//! (ADR 0017), on one origin, for the LAN. The satellite group's
//! `PeerService` (ADR 0026) is here too, for other satellites to call.
//!
//! No authentication. A Host allowlist keeps other web sites from reaching the
//! API through DNS rebinding; cross-site requests are already stopped by the
//! Connect protocol (JSON and proto bodies need a CORS preflight, which this
//! server never grants).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use prometheus_client::registry::Registry;
use tokio::sync::watch;
use tower_http::services::ServeDir;

use crate::api::{self, DeviceApi};
use crate::assistant_api::{self, AssistantApi};
use crate::config::Web;
use crate::group::peer::{self, PeerApi};
use crate::mcp::{self, McpServer};
use crate::metrics::{self, Counters};
use crate::virtual_device::{self, VirtualDevice};
use crate::voice::Snapshot;

/// Host names the dashboard answers to besides IP addresses and `*.local`:
/// lowercase, no port. Those from the configuration, and the host of the
/// dashboard address (ADR 0021), read from the latest status so it applies at
/// once.
///
/// Any `.local` name is fine: only mDNS on this network resolves it, so a remote
/// site cannot rebind one. That also covers the name mDNS picks when two
/// satellites both start as `pinna.local`.
#[derive(Debug, Clone)]
pub struct Hosts {
    fixed: Arc<Vec<String>>,
    added: Option<watch::Receiver<Snapshot>>,
    /// The system hostname at start, for the 421 page's `.local` link and the
    /// allowed names until the status says otherwise.
    hostname: Option<String>,
}

impl Hosts {
    pub fn new(hostname: Option<&str>, extra: &[String]) -> Self {
        let mut names = vec!["localhost".to_string()];
        // The machine's own name stays allowed: a developer's computer isn't
        // renamed, and on a satellite it is the name it started with.
        names.extend(hostname.map(normalise).filter(|n| !n.is_empty()));
        names.extend(extra.iter().map(|n| normalise(n)));
        names.sort();
        names.dedup();
        Self {
            fixed: Arc::new(names),
            added: None,
            hostname: hostname.map(normalise).filter(|n| !n.is_empty()),
        }
    }

    /// Also allow the host of the dashboard address.
    pub fn with_added(mut self, status: watch::Receiver<Snapshot>) -> Self {
        self.added = Some(status);
        self
    }

    /// The hostname now: a rename on the dashboard applies at once (ADR 0010).
    fn current_name(&self) -> Option<String> {
        self.added
            .as_ref()
            .map(|rx| normalise(&rx.borrow().name))
            .filter(|n| !n.is_empty())
            .or_else(|| self.hostname.clone())
    }

    fn known(&self, host: &str) -> bool {
        self.fixed.iter().any(|n| n == host)
            || self.current_name().as_deref() == Some(host)
            || self.added.as_ref().is_some_and(|rx| {
                rx.borrow()
                    .settings
                    .url
                    .as_deref()
                    .and_then(|u| url::Url::parse(u).ok())
                    .and_then(|u| u.host_str().map(normalise))
                    .is_some_and(|n| n == host)
            })
    }

    /// Whether a request's authority (`host[:port]`) may reach the dashboard.
    pub fn allows(&self, authority: &str) -> bool {
        if let Some(rest) = authority.strip_prefix('[') {
            return rest
                .split_once(']')
                .is_some_and(|(ip, _)| ip.parse::<Ipv6Addr>().is_ok());
        }
        let host = authority.split_once(':').map_or(authority, |(h, _)| h);
        if host.parse::<Ipv4Addr>().is_ok() {
            return true;
        }
        let host = normalise(host);
        host.strip_suffix(".local").is_some_and(|l| !l.is_empty()) || self.known(&host)
    }
}

fn normalise(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Where the satellite group's calls between satellites arrive (ADR 0026).
const PEER_ROUTES: &str = "/pinna.v1.PeerService/";

/// The routes: Connect procedures under `/pinna.v1.DeviceService/`,
/// `/pinna.v1.AssistantService/` and `/pinna.v1.PeerService/`, `/metrics`
/// (ADR 0020), the virtual Echo Dot's socket in the development VM
/// (ADR 0014), Pinna's MCP server (ADR 0017), the dashboard's files for
/// everything else.
#[allow(clippy::too_many_arguments)]
pub fn app(
    api: Arc<DeviceApi>,
    assistant: Arc<AssistantApi>,
    peer: Arc<PeerApi>,
    root: &Path,
    hosts: Hosts,
    virt: Option<Arc<VirtualDevice>>,
    mcp: Arc<McpServer>,
    registry: Arc<Registry>,
) -> axum::Router {
    let files = axum::Router::new()
        .fallback_service(ServeDir::new(root))
        .layer(middleware::from_fn(cache_control));
    let mut router = axum::Router::new()
        .route_service(
            "/pinna.v1.DeviceService/{method}",
            api::router(api.clone()).into_axum_service(),
        )
        .route_service(
            "/pinna.v1.AssistantService/{method}",
            assistant_api::router(assistant).into_axum_service(),
        )
        .route_service(
            "/pinna.v1.PeerService/{method}",
            peer::router(peer)
                .into_axum_service()
                .with_limits(peer::limits()),
        )
        .route(mcp::ROUTE, mcp::route(mcp, hosts.clone()))
        .route("/metrics", axum::routing::get(scrape).with_state(registry));
    if let Some(v) = virt {
        router = router.route(
            virtual_device::ROUTE,
            axum::routing::get(virtual_device::socket).with_state(v),
        );
    }
    router
        .fallback_service(files)
        .layer(middleware::from_fn_with_state(hosts, check_host))
        .layer(middleware::from_fn_with_state(api, connection))
}

/// Who connected, and to which of the satellite's addresses.
#[derive(Debug, Clone, Copy)]
pub struct Conn {
    pub peer: SocketAddr,
    pub local: Option<SocketAddr>,
}

impl
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>>
    for Conn
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Self {
            peer: *stream.remote_addr(),
            local: stream.io().local_addr().ok(),
        }
    }
}

/// `axum::serve` knows the peer, but only connectrpc's own `Server` hands it to
/// handlers (`ctx.peer_addr()`); pass it on. A request that reached a Wi-Fi
/// network's address on trial keeps that network (ADR 0008), unless another
/// satellite of the group made it: only the owner opening the dashboard
/// counts (ADR 0026).
async fn connection(State(api): State<Arc<DeviceApi>>, mut req: Request, next: Next) -> Response {
    if let Some(ConnectInfo(c)) = req.extensions().get::<ConnectInfo<Conn>>().copied() {
        req.extensions_mut().insert(connectrpc::PeerAddr(c.peer));
        if let Some(local) = c.local
            && !req.uri().path().starts_with(PEER_ROUTES)
        {
            api.reached(local.ip());
        }
    }
    next.run(req).await
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The 421 page: the name it doesn't answer to, and links that work: the
/// address the request reached, and the hostname's `.local` name.
fn misdirected_page(host: &str, local: Option<SocketAddr>, hostname: Option<&str>) -> String {
    let host = html_escape(host);
    let port = local
        .map(|a| a.port())
        .filter(|&p| p != 80)
        .map_or_else(String::new, |p| format!(":{p}"));
    let mut links = Vec::new();
    if let Some(ip) = local.map(|a| a.ip().to_canonical()) {
        let url = match ip {
            IpAddr::V4(v4) => format!("http://{v4}{port}/"),
            IpAddr::V6(v6) => format!("http://[{v6}]{port}/"),
        };
        links.push(format!("<a href=\"{url}\">{url}</a>"));
    }
    if let Some(h) = hostname {
        let url = html_escape(&format!("http://{h}.local{port}/"));
        links.push(format!("<a href=\"{url}\">{url}</a>"));
    }
    let open = if links.is_empty() {
        "Open it by its IP address or its .local name".to_owned()
    } else {
        format!("Open it at {}", links.join(" or "))
    };
    format!(
        "<!doctype html><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width\"><title>Pinna</title>\
         <p>This satellite does not answer to the name \u{201c}{host}\u{201d}.</p>\
         <p>{open}, then set \u{201c}{host}\u{201d} as the dashboard address under Network.</p>\n"
    )
}

async fn check_host(State(hosts): State<Hosts>, req: Request, next: Next) -> Response {
    // HTTP/2 carries the authority in the URI, HTTP/1.1 in the Host header.
    let authority = req
        .uri()
        .authority()
        .map(|a| a.as_str().to_owned())
        .or_else(|| {
            req.headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        });
    match authority {
        Some(a) if hosts.allows(&a) => next.run(req).await,
        other => {
            let host = other.as_deref().unwrap_or("");
            let host = host.split_once(':').map_or(host, |(h, _)| h);
            let local = req
                .extensions()
                .get::<ConnectInfo<Conn>>()
                .and_then(|c| c.0.local);
            let body = misdirected_page(host, local, hosts.current_name().as_deref());
            (
                StatusCode::MISDIRECTED_REQUEST,
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                ],
                body,
            )
                .into_response()
        }
    }
}

/// `/metrics`: read off the async workers, which also run the voice path.
async fn scrape(State(registry): State<Arc<Registry>>) -> Response {
    match tokio::task::spawn_blocking(move || metrics::render(&registry)).await {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, metrics::CONTENT_TYPE),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!("metrics: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Hashed build output under `/assets/` never changes; everything else
/// (`index.html`) is fetched fresh, because image builds fix file times and a
/// conditional request would keep a stale page after an update.
async fn cache_control(req: Request, next: Next) -> Response {
    let immutable = req.uri().path().starts_with("/assets/");
    let mut res = next.run(req).await;
    let value = if immutable && res.status().is_success() {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    };
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    res
}

/// Bind `[web] listen` and serve in the background. `None` when it is off.
pub async fn start(
    api: Arc<DeviceApi>,
    assistant: Arc<AssistantApi>,
    peer: Arc<PeerApi>,
    cfg: &Web,
    virt: Option<Arc<VirtualDevice>>,
    mcp: Arc<McpServer>,
    counters: &Counters,
) -> Result<Option<SocketAddr>> {
    if cfg.listen.is_empty() {
        return Ok(None);
    }
    if !cfg.root.join("index.html").is_file() {
        tracing::warn!(
            "no dashboard in {}: the web listener serves the API only",
            cfg.root.display()
        );
    }
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
    let hosts = Hosts::new(hostname.as_deref(), &cfg.allowed_hosts).with_added(api.watch());
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .with_context(|| format!("binding the dashboard on {}", cfg.listen))?;
    let addr = listener.local_addr()?;
    let registry = metrics::registry(
        Arc::new(api.info().clone()),
        api.watch(),
        counters,
        metrics::Sources::default(),
    );
    let app = app(
        api,
        assistant,
        peer,
        &cfg.root,
        hosts,
        virt,
        mcp,
        Arc::new(registry),
    );
    tokio::spawn(async move {
        if let Err(e) =
            axum::serve(listener, app.into_make_service_with_connect_info::<Conn>()).await
        {
            tracing::error!("dashboard server stopped: {e}");
        }
    });
    Ok(Some(addr))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    use super::*;
    use crate::api::Info;
    use crate::boot;
    use crate::standalone::service::{Paths, Service};

    fn hosts() -> Hosts {
        Hosts::new(Some("pinna\n"), &["Kitchen.home.arpa.".into()])
    }

    #[test]
    fn the_misdirected_page_links_back_and_escapes_the_name() {
        let page = misdirected_page(
            "<script>x</script>",
            Some("[::ffff:192.168.1.20]:80".parse().unwrap()),
            Some("pinna"),
        );
        assert!(!page.contains("<script>"), "{page}");
        assert!(page.contains("&lt;script&gt;"));
        assert!(page.contains("<a href=\"http://192.168.1.20/\">"), "{page}");
        assert!(page.contains("<a href=\"http://pinna.local/\">"));
        let page = misdirected_page("x", Some("[fd00::5]:8080".parse().unwrap()), Some("pinna"));
        assert!(page.contains("http://[fd00::5]:8080/"), "{page}");
        assert!(page.contains("http://pinna.local:8080/"), "{page}");
        assert!(misdirected_page("x", None, None).contains("by its IP address"));
    }

    #[test]
    fn allows_the_dashboard_address_at_once() {
        let (tx, rx) = watch::channel(Snapshot::default());
        let h = hosts().with_added(rx);
        assert!(!h.allows("dot.home.arpa"));
        tx.send_modify(|s| s.settings.url = Some("https://dot.home.arpa:8443/".into()));
        assert!(h.allows("dot.home.arpa:80") && h.allows("kitchen.home.arpa"));
        assert!(h.allows("DOT.home.arpa."));
        tx.send_modify(|s| s.settings.url = None);
        assert!(!h.allows("dot.home.arpa"));
    }

    #[test]
    fn allows_ip_literals_and_own_names() {
        let h = hosts();
        for ok in [
            "192.168.1.5",
            "192.168.1.5:80",
            "[fe80::1]:80",
            "[::1]",
            "localhost:5173",
            "pinna",
            "pinna.local",
            "PINNA.local.",
            "pinna-2.local:80",
            "kitchen.home.arpa",
        ] {
            assert!(h.allows(ok), "{ok}");
        }
        for bad in [
            "",
            "evil.example",
            "pinna.local.evil.example",
            "1.2.3.4.nip.io",
            "[not-an-ip]:80",
            "[::1",
            ".local",
            "pinna-2",
        ] {
            assert!(!h.allows(bad), "{bad}");
        }
    }

    struct Fixture {
        app: axum::Router,
        _inputs: mpsc::Receiver<crate::voice::Input>,
        dir: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The web routes over a status channel whose sender is already gone, so
    /// `WatchStatus` sends the current state once and ends.
    fn fixture(name: &str, with_files: bool) -> Fixture {
        fixture_with(name, with_files, None)
    }

    /// The same, with Wi-Fi run through `wifi`.
    fn fixture_with(name: &str, with_files: bool, wifi: Option<crate::wifi::Wifi>) -> Fixture {
        let dir = std::env::temp_dir().join(format!("pinnad-web-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        if with_files {
            std::fs::create_dir_all(dir.join("assets")).unwrap();
            std::fs::write(
                dir.join("index.html"),
                "<!doctype html><title>Pinna</title>",
            )
            .unwrap();
            std::fs::write(dir.join("assets/app-1234.js"), "export {}").unwrap();
        }
        let (_status_tx, status_rx) = watch::channel(Snapshot {
            name: "pinna-test".into(),
            ..Default::default()
        });
        let (in_tx, in_rx) = mpsc::channel(1);
        let info = Info {
            version: "0.1.0".into(),
            mac: "02:00:00:aa:bb:cc".into(),
            time_zones: Default::default(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: String::new(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
            key_from_image: false,
        };
        let mut api = DeviceApi::new(info, status_rx.clone(), in_tx);
        if let Some(wifi) = wifi {
            api = api.with_wifi(wifi);
        }
        let api = Arc::new(api);
        let (assistant, peer, mcp) = assistant(&dir, status_rx);
        Fixture {
            app: app(
                api,
                assistant,
                peer,
                &dir,
                hosts(),
                None,
                mcp,
                Arc::new(registry()),
            ),
            _inputs: in_rx,
            dir,
        }
    }

    /// The setup API, the satellite group's calls and `/mcp` over files in
    /// `dir`.
    fn assistant(
        dir: &Path,
        status: watch::Receiver<Snapshot>,
    ) -> (Arc<AssistantApi>, Arc<PeerApi>, Arc<McpServer>) {
        let service = Service::load(Paths::beside(&dir.join("settings.toml")), status.clone());
        let inputs = tokio::sync::mpsc::channel(1).0;
        let mcp = McpServer::new(
            service.clone(),
            inputs.clone(),
            status.clone(),
            crate::tools::ToolKit::new(Counters::default()),
            "0.1.0",
        );
        let group = crate::group::Group::new(
            service.clone(),
            "02:00:00:aa:bb:cc",
            80,
            status,
            watch::channel(crate::satellites::Seen::default()).1,
            crate::group::Limits::default(),
        );
        (
            Arc::new(AssistantApi::new(service, inputs)),
            Arc::new(PeerApi::new(Arc::new(group))),
            Arc::new(mcp),
        )
    }

    fn registry() -> Registry {
        let mut r = Registry::default();
        let c = prometheus_client::metrics::counter::Counter::<u64>::default();
        c.inc();
        r.register("pinna_test", "A test counter", c);
        r
    }

    async fn send(
        f: &Fixture,
        req: axum::http::Request<Body>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let res = f.app.clone().oneshot(req).await.unwrap();
        let (parts, body) = res.into_parts();
        let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
        (
            parts.status,
            parts.headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    fn get(host: &str, path: &str) -> axum::http::Request<Body> {
        axum::http::Request::get(path)
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap()
    }

    fn rpc(
        host: &str,
        method: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> axum::http::Request<Body> {
        call(host, &format!("DeviceService/{method}"), content_type, body)
    }

    /// A Connect call to `procedure` ("Service/Method") under `pinna.v1`.
    fn call(
        host: &str,
        procedure: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> axum::http::Request<Body> {
        axum::http::Request::post(format!("/pinna.v1.{procedure}"))
            .header(header::HOST, host)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn serves_the_dashboard_fresh_and_assets_immutable() {
        let f = fixture("files", true);
        let (status, headers, body) = send(&f, get("pinna.local", "/")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<title>Pinna</title>"));
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        let (status, headers, _) = send(&f, get("10.0.0.2", "/assets/app-1234.js")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        let (status, headers, _) = send(&f, get("10.0.0.2", "/assets/gone-0000.js")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn serves_metrics_to_known_hosts() {
        let f = fixture("metrics", false);
        let (status, headers, body) = send(&f, get("192.168.1.5", "/metrics")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            headers[header::CONTENT_TYPE],
            "text/plain; version=0.0.4; charset=utf-8"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert!(body.contains("\npinna_test_total 1\n"), "{body}");
        let (status, _, _) = send(&f, get("evil.example", "/metrics")).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn foreign_host_is_misdirected() {
        let f = fixture("rebind", true);
        let (status, _, body) = send(&f, get("evil.example", "/")).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
        assert!(body.contains("\u{201c}evil.example\u{201d}"), "{body}");
        assert!(body.contains("<a href=\"http://pinna.local/\">"), "{body}");
        let req = rpc(
            "evil.example:80",
            "GetStatus",
            "application/json",
            b"{}".to_vec(),
        );
        assert_eq!(send(&f, req).await.0, StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn api_answers_on_the_same_origin() {
        let f = fixture("api", true);
        let req = rpc(
            "[fe80::1]:80",
            "GetStatus",
            "application/json",
            b"{}".to_vec(),
        );
        let (status, _, body) = send(&f, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"name\":\"pinna-test\""), "{body}");
    }

    #[tokio::test]
    async fn the_assistant_answers_on_the_same_origin_for_known_hosts_only() {
        let f = fixture("assistant", false);
        let get = |host| {
            call(
                host,
                "AssistantService/GetAssistant",
                "application/json",
                b"{}".to_vec(),
            )
        };
        let (status, _, body) = send(&f, get("pinna.local")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"presets\""), "{body}");
        assert_eq!(
            send(&f, get("evil.example")).await.0,
            StatusCode::MISDIRECTED_REQUEST
        );
    }

    #[tokio::test]
    async fn other_satellites_reach_the_peer_service_by_ip_address_only() {
        let f = fixture("peer-service", false);
        let call = |host| {
            call(
                host,
                "PeerService/GetGroup",
                "application/json",
                b"{}".to_vec(),
            )
        };
        // This satellite is in no group: the call arrives and is refused.
        for host in ["192.168.1.5", "[fd00::5]:80"] {
            let (status, _, body) = send(&f, call(host)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{host}: {body}");
            assert!(body.contains("failed_precondition"), "{body}");
            assert!(
                body.contains(crate::group::peer::NOT_IN_THIS_GROUP),
                "{body}"
            );
        }
        assert_eq!(
            send(&f, call("evil.example")).await.0,
            StatusCode::MISDIRECTED_REQUEST
        );
    }

    /// During a Wi-Fi trial (ADR 0008), only the owner opening the dashboard
    /// on the new network keeps it: a call from another satellite of the
    /// group doesn't (ADR 0026).
    #[tokio::test]
    async fn a_satellites_call_does_not_keep_a_wifi_trial() {
        use crate::wifi::fake::{Fake, Outcome};
        use crate::wifi::{Join, Wifi};
        use pinna_api::{JoinStage, WifiSecurity};
        use std::time::Duration;

        let fake = Fake::start(Outcome::Joins);
        let wifi = Wifi::with_timeouts(
            fake.paths(),
            Duration::from_secs(3),
            Duration::from_secs(60),
        );
        let mut live = wifi
            .join(Join {
                ssid: "Café".into(),
                security: WifiSecurity::WIFI_SECURITY_WPA_PSK,
                passphrase: "secret passphrase".into(),
                hidden: false,
            })
            .unwrap();
        loop {
            let step = tokio::time::timeout(Duration::from_secs(60), live.recv())
                .await
                .unwrap()
                .unwrap();
            if step.stage.as_known() == Some(JoinStage::JOIN_STAGE_TRIAL) {
                break;
            }
        }
        // The join task finishes right after its last step.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let f = fixture_with("peer-trial", false, Some(wifi));
        let at_the_new_address = |procedure: &str| {
            let mut req = call(
                "192.168.1.20",
                procedure,
                "application/json",
                b"{}".to_vec(),
            );
            req.extensions_mut().insert(ConnectInfo(Conn {
                peer: "192.168.1.30:40000".parse().unwrap(),
                local: Some("192.168.1.20:80".parse().unwrap()),
            }));
            req
        };
        let saved = || fake.log().iter().any(|c| c == "SAVE_CONFIG");

        for procedure in ["PeerService/GetGroup", "PeerService/Join"] {
            send(&f, at_the_new_address(procedure)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!saved(), "a satellite's call kept the trial");

        send(&f, at_the_new_address("DeviceService/GetStatus")).await;
        for _ in 0..100 {
            if saved() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(saved(), "the dashboard opened at the new address keeps it");
    }

    #[tokio::test]
    async fn watch_status_streams_over_the_web_listener() {
        let f = fixture("watch", true);
        // One Connect envelope: flags 0, length 2, `{}`.
        let req = rpc(
            "pinna.local",
            "WatchStatus",
            "application/connect+json",
            vec![0, 0, 0, 0, 2, b'{', b'}'],
        );
        let (status, _, body) = send(&f, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("\"name\":\"pinna-test\""), "{body}");
    }

    #[tokio::test]
    async fn get_updates_suggests_the_peers_address() {
        let f = fixture("peer", false);
        let mut req = rpc(
            "pinna.local",
            "GetUpdates",
            "application/json",
            b"{}".to_vec(),
        );
        let peer: SocketAddr = "192.168.1.77:51234".parse().unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(Conn { peer, local: None }));
        let (status, _, body) = send(&f, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains("\"suggestedManifest\":\"http://192.168.1.77:8000/manifest.json\""),
            "{body}"
        );
    }

    #[tokio::test]
    async fn api_works_without_a_built_dashboard() {
        let f = fixture("nofiles", false);
        assert_eq!(
            send(&f, get("pinna.local", "/")).await.0,
            StatusCode::NOT_FOUND
        );
        let req = rpc(
            "pinna.local",
            "GetStatus",
            "application/json",
            b"{}".to_vec(),
        );
        assert_eq!(send(&f, req).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn off_when_listen_is_empty() {
        let f = fixture("off", false);
        let (_tx, rx) = watch::channel(Snapshot::default());
        let (tx, _rx) = mpsc::channel(1);
        let info = Info {
            version: "0.1.0".into(),
            mac: String::new(),
            time_zones: Default::default(),
            audio_backend: String::new(),
            led_backend: String::new(),
            model: String::new(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
            key_from_image: false,
        };
        let api = Arc::new(DeviceApi::new(info, rx.clone(), tx));
        let (assistant, peer, mcp) = assistant(&f.dir, rx);
        let cfg = Web {
            listen: String::new(),
            root: f.dir.clone(),
            allowed_hosts: Vec::new(),
        };
        let counters = Counters::default();
        assert_eq!(
            start(
                api.clone(),
                assistant.clone(),
                peer.clone(),
                &cfg,
                None,
                mcp.clone(),
                &counters
            )
            .await
            .unwrap(),
            None
        );
        // A typo in the config is an error for main to log, never a panic.
        let cfg = Web {
            listen: "0.0.0.0:eighty".into(),
            ..cfg
        };
        assert!(
            start(api, assistant, peer, &cfg, None, mcp, &counters)
                .await
                .is_err()
        );
    }
}
