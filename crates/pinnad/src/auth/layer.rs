//! The check in front of the web listener, and its login and logout routes
//! (ADR 0029).
//!
//! Default-deny: [`guard`] wraps the whole router, the file fallback included,
//! and a request is public only when it matches one of the exceptions below
//! by exact route. A route added later has a session or it is refused, and
//! nobody has to remember to say so. Once a password is set:
//!
//! - the dashboard's own files (GET and HEAD on whatever no route matches),
//!   `POST /auth/login`, `POST /auth/logout`, and five `DeviceService` calls
//!   that check their own credentials are public;
//! - `/mcp` and `/metrics` need their token as a bearer token;
//! - `PeerService` has its own group key (ADR 0026);
//! - everything else needs a session cookie.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use connectrpc::ConnectError;
use futures::StreamExt;
use pinna_mcp::jsonrpc::Id;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;

use super::{Auth, LoginError, TokenKind};
use crate::mcp;
use crate::web::{Conn, Hosts};

pub const LOGIN: &str = "/auth/login";
pub const LOGOUT: &str = "/auth/logout";
pub const DEVICE_ROUTE: &str = "/pinna.v1.DeviceService/{method}";
pub const ASSISTANT_ROUTE: &str = "/pinna.v1.AssistantService/{method}";
pub const PEER_ROUTE: &str = "/pinna.v1.PeerService/{method}";
pub const METRICS: &str = "/metrics";

const DEVICE_PREFIX: &str = "/pinna.v1.DeviceService/";
const ASSISTANT_PREFIX: &str = "/pinna.v1.AssistantService/";

/// The `DeviceService` calls that need no session: they say who may call them
/// themselves.
const PUBLIC_METHODS: [&str; 5] = [
    "GetSecurity",
    "StartConfirmation",
    "WatchConfirmation",
    "CancelConfirmation",
    "SetDashboardPassword",
];

const SIGN_IN: &str = "Sign in to this satellite's dashboard.";
const AGENT_TOKEN: &str = "This satellite needs its agent token. Create one on its System page.";
const METRICS_TOKEN: &str =
    "This satellite needs its metrics token. Create one on its System page.\n";
const BEARER: &str = "Bearer realm=\"pinna\"";
const MAX_LOGIN_BODY: usize = 4096;

/// Set on every request that passes the check: whether it carries a valid
/// session. `DeviceService` reads it for `GetSecurity`.
#[derive(Debug, Clone, Copy)]
pub struct Session {
    pub signed_in: bool,
}

/// Set on requests a session let through: it changes when every session ends.
/// A socket that outlives its response (the VM's) watches it.
#[derive(Debug, Clone)]
pub struct SessionEnd(pub watch::Receiver<u64>);

enum Class {
    Public,
    Files,
    Peer,
    Mcp,
    Metrics,
    Session,
}

fn classify(req: &Request) -> Class {
    // A request no route matched is the dashboard's files.
    let Some(matched) = req.extensions().get::<MatchedPath>() else {
        return if matches!(*req.method(), Method::GET | Method::HEAD) {
            Class::Files
        } else {
            Class::Session
        };
    };
    match matched.as_str() {
        LOGIN | LOGOUT => Class::Public,
        DEVICE_ROUTE
            if req
                .uri()
                .path()
                .strip_prefix(DEVICE_PREFIX)
                .is_some_and(|m| PUBLIC_METHODS.contains(&m)) =>
        {
            Class::Public
        }
        PEER_ROUTE => Class::Peer,
        mcp::ROUTE => Class::Mcp,
        METRICS => Class::Metrics,
        _ => Class::Session,
    }
}

/// The check. Runs after `connection` (a Wi-Fi trial is kept before anything
/// is refused, ADR 0008) and after the Host allowlist.
pub async fn guard(State(auth): State<Arc<Auth>>, mut req: Request, next: Next) -> Response {
    // Before the credentials are read: a key that changes in between ends
    // the stream this one opens.
    let end = auth.subscribe_end();
    let protected = super::credentials::has_password(&auth.credentials());
    let signed_in = protected && auth.signed_in(req.headers());
    req.extensions_mut().insert(Session { signed_in });
    match classify(&req) {
        Class::Public | Class::Files | Class::Peer => next.run(req).await,
        Class::Mcp if !protected || auth.bearer_ok(TokenKind::Agent, req.headers()) => {
            next.run(req).await
        }
        Class::Mcp => {
            let mut res = mcp::refusal(StatusCode::UNAUTHORIZED, Id::Null, AGENT_TOKEN);
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(BEARER));
            res
        }
        Class::Metrics if !protected || auth.bearer_ok(TokenKind::Metrics, req.headers()) => {
            auth.note_metrics_read();
            next.run(req).await
        }
        Class::Metrics => (
            StatusCode::UNAUTHORIZED,
            [
                (header::WWW_AUTHENTICATE, BEARER),
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            ],
            METRICS_TOKEN,
        )
            .into_response(),
        Class::Session if protected && !signed_in => unauthenticated(req.uri().path()),
        Class::Session => {
            req.extensions_mut().insert(SessionEnd(end.clone()));
            until_sessions_end(next.run(req).await, end)
        }
    }
}

/// Connect's `unauthenticated` for the Connect services, plain 401 elsewhere.
fn unauthenticated(path: &str) -> Response {
    if path.starts_with(DEVICE_PREFIX) || path.starts_with(ASSISTANT_PREFIX) {
        let error = ConnectError::unauthenticated(SIGN_IN);
        return (
            error.http_status(),
            [(header::CONTENT_TYPE, "application/json")],
            error.to_json(),
        )
            .into_response();
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("{SIGN_IN}\n"),
    )
        .into_response()
}

/// The response, cut off with an error when every session ends, so a stream
/// (`WatchStatus`, `WatchRing`) doesn't keep sending to a session that is
/// gone. A one-shot answer is whole already and isn't touched: the call that
/// ends the sessions (`SignOutEverywhere`) still gets its answer.
fn until_sessions_end(res: Response, ended: watch::Receiver<u64>) -> Response {
    use axum::body::HttpBody as _;
    // Connect's unary calls answer in plain JSON or proto; its streams use
    // `application/connect+...`.
    let one_shot = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            matches!(
                v.split(';').next(),
                Some("application/json" | "application/proto")
            )
        });
    if one_shot || res.body().size_hint().exact().is_some() {
        return res;
    }
    let (parts, body) = res.into_parts();
    let stream =
        futures::stream::unfold(Some((body.into_data_stream(), ended)), |state| async move {
            let (mut data, mut ended) = state?;
            tokio::select! {
                _ = ended.changed() => {
                    Some((Err(io::Error::other("the session ended")), None))
                }
                next = data.next() => {
                    next.map(|chunk| (chunk.map_err(io::Error::other), Some((data, ended))))
                }
            }
        });
    Response::from_parts(parts, Body::from_stream(stream))
}

/// What `/auth/login` and `/auth/logout` need.
#[derive(Clone)]
pub struct Routes {
    pub auth: Arc<Auth>,
    pub hosts: Hosts,
}

fn json(status: StatusCode, body: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// The checks `/mcp` makes before it reads a body: a browser Origin the
/// dashboard answers to, and JSON, so a web page can't post a form here.
fn refused(hosts: &Hosts, req: &Request) -> Option<Response> {
    if !mcp::origin_allowed(hosts, req.headers()) {
        return Some(json(
            StatusCode::FORBIDDEN,
            json!({"error": "Web pages from this origin may not sign in to this satellite."}),
        ));
    }
    if !mcp::is_json(req.headers()) {
        return Some(json(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            json!({"error": "Send the request with Content-Type application/json."}),
        ));
    }
    None
}

#[derive(Deserialize)]
struct Credentials {
    password: String,
}

pub async fn login(State(routes): State<Routes>, req: Request) -> Response {
    if let Some(refusal) = refused(&routes.hosts, &req) {
        return refusal;
    }
    let from = req
        .extensions()
        .get::<ConnectInfo<Conn>>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| {
            c.0.peer.ip().to_canonical()
        });
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_LOGIN_BODY).await else {
        return json(
            StatusCode::BAD_REQUEST,
            json!({"error": "The request body couldn't be read, or is larger than 4 KiB."}),
        );
    };
    let Ok(Credentials { password }) = serde_json::from_slice(&body) else {
        return json(
            StatusCode::BAD_REQUEST,
            json!({"error": "Send {\"password\": \"...\"} as JSON."}),
        );
    };
    match routes.auth.login(from, password).await {
        Ok(value) => (
            StatusCode::NO_CONTENT,
            [(header::SET_COOKIE, routes.auth.session_cookie(&value))],
        )
            .into_response(),
        Err(LoginError::NoPassword) => json(
            StatusCode::CONFLICT,
            json!({"error": "This satellite has no password."}),
        ),
        Err(LoginError::Wrong) => json(
            StatusCode::UNAUTHORIZED,
            json!({"error": "Wrong password."}),
        ),
        Err(LoginError::Wait(seconds)) => {
            let mut res = json(
                StatusCode::TOO_MANY_REQUESTS,
                json!({
                    "error": format!("Too many attempts. Try again in {seconds} s."),
                    "retry_after_s": seconds,
                }),
            );
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
            res
        }
    }
}

pub async fn logout(State(routes): State<Routes>, req: Request) -> Response {
    if let Some(refusal) = refused(&routes.hosts, &req) {
        return refusal;
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, routes.auth.clear_session_cookie())],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Bytes;
    use axum::middleware;
    use axum::routing::{any, get, post};
    use tower::ServiceExt;

    use super::super::tests::{Rig, rig};
    use super::*;

    const HOST: &str = "pinna.local";

    /// One route for each class of request, and a made-up one nobody listed.
    fn app(auth: &Arc<Auth>) -> axum::Router {
        let routes = Routes {
            auth: auth.clone(),
            hosts: Hosts::new(Some("pinna"), &[]),
        };
        axum::Router::new()
            .route(DEVICE_ROUTE, post(|| async { "device" }))
            .route(PEER_ROUTE, post(|| async { "peer" }))
            .route(mcp::ROUTE, any(|| async { "mcp" }))
            .route(METRICS, get(|| async { "metrics" }))
            .route("/made-up", get(|| async { "secret" }))
            .route(
                "/stream",
                get(|| async {
                    Body::from_stream(futures::stream::pending::<Result<Bytes, io::Error>>())
                }),
            )
            .route(LOGIN, post(login).with_state(routes.clone()))
            .route(LOGOUT, post(logout).with_state(routes))
            .fallback(|| async { "file" })
            .layer(middleware::from_fn_with_state(auth.clone(), guard))
    }

    async fn send(app: &axum::Router, req: axum::http::request::Builder) -> Response {
        app.clone()
            .oneshot(req.header("host", HOST).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn req(method: &str, uri: &str) -> axum::http::request::Builder {
        Request::builder().method(method).uri(uri)
    }

    async fn text(res: Response) -> String {
        let body = axum::body::to_bytes(res.into_body(), 1 << 16)
            .await
            .unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    async fn signed_in(rig: &Rig) -> String {
        rig.auth.limiter.lock().unwrap().reset();
        let from = "192.168.1.9".parse().unwrap();
        let value = rig.auth.login(from, "correct horse".into()).await.unwrap();
        format!("pinna_session_aabbcc={value}")
    }

    #[tokio::test]
    async fn without_a_password_everything_stays_open() {
        let rig = rig();
        let app = app(&rig.auth);
        for (method, uri) in [
            ("GET", "/made-up"),
            ("POST", "/pinna.v1.DeviceService/Reboot"),
            ("POST", "/mcp"),
            ("GET", "/metrics"),
            ("GET", "/"),
        ] {
            let res = send(&app, req(method, uri)).await;
            assert_eq!(res.status(), StatusCode::OK, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn a_route_nobody_listed_is_refused_without_a_session() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        let res = send(&app, req("GET", "/made-up")).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(text(res).await, format!("{SIGN_IN}\n"));

        let cookie = signed_in(&rig).await;
        let res = send(&app, req("GET", "/made-up").header("cookie", &cookie)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(text(res).await, "secret");
        // A bad cookie is no cookie.
        let bad = req("GET", "/made-up").header("cookie", "pinna_session_aabbcc=x");
        assert_eq!(send(&app, bad).await.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_dashboards_files_are_public_to_read_only() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        for uri in ["/", "/assets/app-1234.js", "/favicon.svg", "/anything/else"] {
            for method in ["GET", "HEAD"] {
                let res = send(&app, req(method, uri)).await;
                assert_eq!(res.status(), StatusCode::OK, "{method} {uri}");
            }
        }
        // A write to a path no route has is not a file.
        let res = send(&app, req("POST", "/anything/else")).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn only_five_device_calls_are_public_and_the_rest_say_to_sign_in() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        let call = |m: &str| req("POST", &format!("/pinna.v1.DeviceService/{m}"));
        for method in PUBLIC_METHODS {
            assert_eq!(send(&app, call(method)).await.status(), StatusCode::OK);
        }
        for method in [
            "GetStatus",
            "Reboot",
            "FactoryReset",
            "RemoveDashboardPassword",
            "CreateToken",
            "SignOutEverywhere",
            "LeaveDashboardOpen",
            "WatchStatus",
            "WatchRing",
            // Near misses of the public names.
            "GetSecurity/",
            "getsecurity",
            "GetSecurityX",
        ] {
            let res = send(&app, call(method)).await;
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method}");
            let body: serde_json::Value = serde_json::from_str(&text(res).await).unwrap();
            assert_eq!(body["code"], "unauthenticated", "{method}");
            assert_eq!(body["message"], SIGN_IN);
        }
        let res = send(&app, req("POST", "/pinna.v1.AssistantService/GetAssistant")).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_group_has_its_own_key() {
        let mut rig = rig();
        rig.with_password().await;
        let res = send(&app(&rig.auth), req("POST", "/pinna.v1.PeerService/Call")).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mcp_needs_the_agent_token_and_says_so() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        let res = send(&app, req("POST", "/mcp")).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            res.headers()[header::WWW_AUTHENTICATE],
            "Bearer realm=\"pinna\""
        );
        let body: serde_json::Value = serde_json::from_str(&text(res).await).unwrap();
        assert_eq!(body["error"]["message"], AGENT_TOKEN);
        assert_eq!(
            AGENT_TOKEN,
            "This satellite needs its agent token. Create one on its System page."
        );

        // A session is not the agent token, and the metrics token isn't either.
        let cookie = signed_in(&rig).await;
        let with_session = req("POST", "/mcp").header("cookie", cookie);
        assert_eq!(
            send(&app, with_session).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let metrics = rig.auth.create_token(TokenKind::Metrics).await.unwrap();
        let agent = rig.auth.create_token(TokenKind::Agent).await.unwrap();
        let bearer = |t: &str| format!("Bearer {t}");
        let status = |method: &str, uri: &str, token: &str| {
            send(
                &app,
                req(method, uri).header("authorization", bearer(token)),
            )
        };
        assert_eq!(
            status("POST", "/mcp", &metrics).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status("POST", "/mcp", &agent).await.status(),
            StatusCode::OK
        );
        // And the agent token opens nothing else.
        for uri in ["/made-up", "/metrics"] {
            assert_eq!(
                status("GET", uri, &agent).await.status(),
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn metrics_need_the_metrics_token() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        let res = send(&app, req("GET", "/metrics")).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(text(res).await.lines().count(), 1);
        assert!(
            !rig.auth.security(true).metrics_read,
            "a refusal is no read"
        );

        let token = rig.auth.create_token(TokenKind::Metrics).await.unwrap();
        let read = req("GET", "/metrics").header("authorization", format!("Bearer {token}"));
        assert_eq!(send(&app, read).await.status(), StatusCode::OK);
        assert!(rig.auth.security(true).metrics_read);
    }

    #[tokio::test]
    async fn login_takes_json_from_the_dashboards_own_origin() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        let login = |origin: Option<&str>, content_type: &str, body: &str| {
            let mut r = req("POST", LOGIN)
                .header("host", HOST)
                .header("content-type", content_type);
            if let Some(o) = origin {
                r = r.header("origin", o);
            }
            app.clone()
                .oneshot(r.body(Body::from(body.to_owned())).unwrap())
        };
        let ok = r#"{"password":"correct horse"}"#;
        let res = login(Some("http://evil.example"), "application/json", ok)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        for ct in ["text/plain", "application/x-www-form-urlencoded", ""] {
            let res = login(None, ct, ok).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct:?}");
        }
        for body in ["", "{}", "not json", r#"{"password":1}"#] {
            let res = login(None, "application/json", body).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body:?}");
        }
        // None of those was an attempt.
        assert_eq!(rig.auth.security(false).failures_since_login, 0);

        let res = login(Some("http://pinna.local"), "application/json", ok)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            send(&app, req("GET", LOGIN)).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn a_login_sets_the_cookie_that_opens_the_dashboard() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        let attempt = |password: &str| {
            let r = req("POST", LOGIN)
                .header("host", HOST)
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"password":"{password}"}}"#)))
                .unwrap();
            app.clone().oneshot(r)
        };
        let res = attempt("wrong").await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(text(res).await, r#"{"error":"Wrong password."}"#);

        // The next attempt, in the same second, is turned away with the wait.
        rig.take_the_turn();
        let res = attempt("correct horse").await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res.headers()[header::RETRY_AFTER], "1");
        let body: serde_json::Value = serde_json::from_str(&text(res).await).unwrap();
        assert_eq!(body["retry_after_s"], 1);
        assert_eq!(body["error"], "Too many attempts. Try again in 1 s.");

        rig.auth.limiter.lock().unwrap().reset();
        let res = attempt("correct horse").await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let set = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        let (pair, attributes) = set.split_once("; ").unwrap();
        assert!(pair.starts_with("pinna_session_aabbcc="), "{set}");
        assert_eq!(
            attributes,
            "HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000"
        );
        let res = send(&app, req("GET", "/made-up").header("cookie", pair)).await;
        assert_eq!(res.status(), StatusCode::OK);

        let logout = req("POST", LOGOUT).header("content-type", "application/json");
        let res = send(&app, logout).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let cleared = res.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(cleared.contains("Max-Age=0"), "{cleared}");
    }

    #[tokio::test]
    async fn ending_every_session_ends_the_streams_they_hold() {
        let mut rig = rig();
        rig.with_password().await;
        let app = app(&rig.auth);
        let cookie = signed_in(&rig).await;
        let res = send(&app, req("GET", "/stream").header("cookie", &cookie)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let mut body = res.into_body().into_data_stream();

        rig.auth.sign_out_everywhere().await.unwrap();
        let end = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("the stream ended");
        assert!(matches!(end, Some(Err(_))), "cut off with an error");
    }

    #[tokio::test]
    async fn a_stream_opened_while_the_dashboard_was_open_ends_when_a_password_is_set() {
        let mut rig = rig();
        let app = app(&rig.auth);
        let res = send(&app, req("GET", "/stream")).await;
        let mut body = res.into_body().into_data_stream();
        rig.with_password().await;
        let end = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("the stream ended");
        assert!(matches!(end, Some(Err(_))));
    }
}
