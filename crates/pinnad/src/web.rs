//! Dashboard listener (ADR 0005): the built Svelte dashboard and the same
//! ConnectRPC `DeviceService`, on one origin, for the LAN.
//!
//! No authentication. A Host allowlist keeps other web sites from reaching the
//! API through DNS rebinding; cross-site requests are already stopped by the
//! Connect protocol (JSON and proto bodies need a CORS preflight, which this
//! server never grants).

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use tower_http::services::ServeDir;

use crate::api::{self, DeviceApi};
use crate::config::Web;
use crate::virtual_device::{self, VirtualDevice};

/// Host names the dashboard answers to besides IP addresses and `*.local`:
/// lowercase, no port.
///
/// Any `.local` name is fine: only mDNS on this network resolves it, so a remote
/// site cannot rebind one. That also covers the name mDNS picks when two
/// satellites both start as `pinna.local`.
#[derive(Debug, Clone)]
pub struct Hosts(Arc<Vec<String>>);

impl Hosts {
    pub fn new(hostname: Option<&str>, extra: &[String]) -> Self {
        let mut names = vec!["localhost".to_string()];
        names.extend(hostname.map(normalise).filter(|n| !n.is_empty()));
        names.extend(extra.iter().map(|n| normalise(n)));
        names.sort();
        names.dedup();
        Self(Arc::new(names))
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
        host.strip_suffix(".local").is_some_and(|l| !l.is_empty()) || self.0.contains(&host)
    }
}

fn normalise(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// The routes: Connect procedures under `/pinna.v1.DeviceService/`, the
/// virtual Echo Dot's socket in the development VM (ADR 0014), the
/// dashboard's files for everything else.
pub fn app(
    api: Arc<DeviceApi>,
    root: &Path,
    hosts: Hosts,
    virt: Option<Arc<VirtualDevice>>,
) -> axum::Router {
    let files = axum::Router::new()
        .fallback_service(ServeDir::new(root))
        .layer(middleware::from_fn(cache_control));
    let mut router = axum::Router::new().route_service(
        "/pinna.v1.DeviceService/{method}",
        api::router(api).into_axum_service(),
    );
    if let Some(v) = virt {
        router = router.route(
            virtual_device::ROUTE,
            axum::routing::get(virtual_device::socket).with_state(v),
        );
    }
    router
        .fallback_service(files)
        .layer(middleware::from_fn_with_state(hosts, check_host))
        .layer(middleware::from_fn(peer_addr))
}

/// `axum::serve` knows the peer, but only connectrpc's own `Server` hands it to
/// handlers (`ctx.peer_addr()`); pass it on.
async fn peer_addr(mut req: Request, next: Next) -> Response {
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() {
        req.extensions_mut().insert(connectrpc::PeerAddr(addr));
    }
    next.run(req).await
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
            let body = format!(
                "This satellite does not answer to the name \"{host}\".\n\
                 Open it by its IP address, or add \"{host}\" to allowed_hosts under [web] \
                 in /var/lib/pinna/pinnad.toml and restart pinnad.\n"
            );
            (
                StatusCode::MISDIRECTED_REQUEST,
                [
                    (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                ],
                body,
            )
                .into_response()
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
    cfg: &Web,
    virt: Option<Arc<VirtualDevice>>,
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
    let hosts = Hosts::new(hostname.as_deref(), &cfg.allowed_hosts);
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .with_context(|| format!("binding the dashboard on {}", cfg.listen))?;
    let addr = listener.local_addr()?;
    let app = app(api, &cfg.root, hosts, virt);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            tracing::error!("dashboard server stopped: {e}");
        }
    });
    Ok(Some(addr))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use tokio::sync::{mpsc, watch};
    use tower::ServiceExt;

    use super::*;
    use crate::api::Info;
    use crate::boot;
    use crate::voice::Snapshot;

    fn hosts() -> Hosts {
        Hosts::new(Some("pinna\n"), &["Kitchen.home.arpa.".into()])
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
        let (_status_tx, status_rx) = watch::channel(Snapshot::default());
        let (in_tx, in_rx) = mpsc::channel(1);
        let info = Info {
            name: "pinna-test".into(),
            mac: "02:00:00:aa:bb:cc".into(),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: String::new(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
        };
        let api = Arc::new(DeviceApi::new(info, status_rx, in_tx));
        Fixture {
            app: app(api, &dir, hosts(), None),
            _inputs: in_rx,
            dir,
        }
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
        axum::http::Request::post(format!("/pinna.v1.DeviceService/{method}"))
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
    async fn foreign_host_is_misdirected() {
        let f = fixture("rebind", true);
        let (status, _, body) = send(&f, get("evil.example", "/")).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
        assert!(body.contains("\"evil.example\""), "{body}");
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
        req.extensions_mut().insert(ConnectInfo(peer));
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
            name: String::new(),
            mac: String::new(),
            wake_words: Default::default(),
            audio_backend: String::new(),
            led_backend: String::new(),
            model: String::new(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
        };
        let api = Arc::new(DeviceApi::new(info, rx, tx));
        let cfg = Web {
            listen: String::new(),
            root: f.dir.clone(),
            allowed_hosts: Vec::new(),
        };
        assert_eq!(start(api.clone(), &cfg, None).await.unwrap(), None);
        // A typo in the config is an error for main to log, never a panic.
        let cfg = Web {
            listen: "0.0.0.0:eighty".into(),
            ..cfg
        };
        assert!(start(api, &cfg, None).await.is_err());
    }
}
