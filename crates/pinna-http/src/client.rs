use std::time::Duration;

use reqwest::{Client, ClientBuilder, redirect::Policy};

#[derive(Clone, Copy, Debug)]
pub enum ClientKind {
    Media,
    Update,
    Provider { read: Option<Duration> },
    Tool,
    Mcp,
}

fn builder(kind: ClientKind, tls: rustls::ClientConfig) -> ClientBuilder {
    let b = Client::builder().tls_backend_preconfigured(tls);
    let secs = Duration::from_secs;
    match kind {
        ClientKind::Media => b
            .user_agent("pinna")
            .connect_timeout(secs(10))
            .read_timeout(secs(20)),
        ClientKind::Update => b
            .user_agent(concat!("pinna-update/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(secs(30)),
        ClientKind::Provider { .. } | ClientKind::Mcp => {
            let b = b
                .redirect(Policy::none())
                .connect_timeout(secs(5))
                .pool_idle_timeout(secs(300));
            match kind {
                ClientKind::Provider { read: Some(d) } => b.read_timeout(d),
                _ => b,
            }
        }
        ClientKind::Tool => b
            .redirect(Policy::limited(3))
            .connect_timeout(secs(5))
            .timeout(secs(10)),
    }
}

pub fn client(kind: ClientKind) -> Client {
    builder(kind, crate::tls()).build().expect("HTTP client")
}

#[cfg(feature = "blocking")]
pub fn blocking_client(kind: ClientKind) -> reqwest::blocking::Client {
    let timeout = matches!(kind, ClientKind::Update).then(|| Duration::from_secs(60));
    reqwest::blocking::ClientBuilder::from(builder(kind, crate::tls()))
        .timeout(timeout)
        .build()
        .expect("blocking HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NetError, classify};
    use axum::{Router, http::StatusCode, routing::get};
    use tokio::net::TcpListener;

    #[test]
    fn missing_bundle_still_builds_a_client() {
        let tls = crate::tls_from(std::path::Path::new("/dev/null/missing"));
        builder(ClientKind::Media, tls).build().unwrap();
    }

    #[tokio::test]
    async fn redirects_and_errors() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", socket.local_addr().unwrap());
        let routes = Router::new()
            .route(
                "/",
                get(|| async { (StatusCode::FOUND, [("location", "/ok")]) }),
            )
            .route("/ok", get(|| async { "ok" }))
            .route("/slow", get(std::future::pending::<()>));
        let server = tokio::spawn(async move { axum::serve(socket, routes).await.unwrap() });
        for (kind, status) in [
            (ClientKind::Provider { read: None }, 302),
            (ClientKind::Mcp, 302),
            (ClientKind::Media, 200),
        ] {
            assert_eq!(
                client(kind).get(&url).send().await.unwrap().status(),
                status
            );
        }
        let err = client(ClientKind::Provider {
            read: Some(Duration::from_millis(1)),
        })
        .get(format!("{url}/slow"))
        .send()
        .await
        .unwrap_err();
        assert_eq!(classify(&err), NetError::Timeout);
        // A TLS handshake against a plain HTTP listener fails in rustls the
        // way an expired or not-yet-valid certificate does: the error the
        // "clock not set" sentence depends on.
        let err = client(ClientKind::Mcp)
            .get(url.replace("http://", "https://"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(classify(&err), NetError::Tls);
        server.abort();
        let _ = server.await;
        let err = client(ClientKind::Media).get(url).send().await.unwrap_err();
        assert_eq!(classify(&err), NetError::Unreachable);
    }
}
