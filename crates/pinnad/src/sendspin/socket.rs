//! The player's endpoint (ADR 0027 §2, §3, §6): an HTTP listener that serves only a WebSocket
//! upgrade at `GET /sendspin`, and one task per WebSocket that only moves frames between the
//! socket and the service.
//!
//! The specification's rules for the upgrade: a request that carries an `Origin` header is
//! refused (a browser page can't reach the player), and at most two connections are open at
//! once, one admitted and one provisional; a third is refused, as the specification lets a
//! player refuse a server of lower priority. Each message is stamped with the monotonic time it
//! arrived, as close to the socket as the task can, because the clock filter measures the
//! network with it. WebSocket ping and pong are the connection's liveness: a server that stops
//! answering for 30 s is dropped.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::PATH;
use super::clock::monotonic_us;

/// Connections open at once: one admitted and one provisional (ADR 0027 §3).
pub const OPEN_MAX: usize = 2;
/// The largest WebSocket message: a Noise transport message is at most 65535 bytes, and the
/// handshake's text messages are JSON of at most 16 KiB. Anything larger is refused before
/// tungstenite buffers it.
const MESSAGE_MAX: usize = 65_535;
/// How often the server is pinged.
const PING_EVERY: Duration = Duration::from_secs(10);
/// How long a server may stay silent, pongs included, or take to accept a message.
const SILENCE_MAX: Duration = Duration::from_secs(30);

/// What a socket task tells the service.
#[derive(Debug)]
pub enum FromSocket {
    Opened {
        id: u64,
        out: mpsc::UnboundedSender<ToSocket>,
    },
    /// A text message and when it arrived, in monotonic µs.
    Text {
        id: u64,
        text: String,
        at: i64,
    },
    Binary {
        id: u64,
        bytes: Bytes,
        at: i64,
    },
    Closed {
        id: u64,
    },
}

/// What the service has a socket task send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToSocket {
    Text(String),
    Binary(Vec<u8>),
    /// Close the socket after what came before.
    Close,
}

/// What every listener and socket task shares, across switching off and on.
#[derive(Debug)]
pub struct Shared {
    to_service: mpsc::Sender<FromSocket>,
    open: AtomicUsize,
    next_id: AtomicU64,
}

impl Shared {
    pub fn new(to_service: mpsc::Sender<FromSocket>) -> Arc<Self> {
        Arc::new(Self {
            to_service,
            open: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
        })
    }

    fn take_slot(self: &Arc<Self>) -> Option<Slot> {
        self.open
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < OPEN_MAX).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot(self.clone()))
    }
}

/// One of the [`OPEN_MAX`] connections, held until its socket closes.
struct Slot(Arc<Shared>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why the listener couldn't open.
#[derive(Debug)]
pub enum BindError {
    InUse(u16),
    Other { port: u16, error: std::io::Error },
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The status shows it as it is.
            Self::InUse(port) => write!(f, "port {port} is in use"),
            Self::Other { port, error } => write!(f, "could not listen on port {port}: {error}"),
        }
    }
}

/// A listener that runs until it is dropped. Connections it accepted run on.
#[derive(Debug)]
pub struct Listener {
    addr: SocketAddr,
    _stop: oneshot::Sender<()>,
}

impl Listener {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Listens on the first of `addrs` that binds: `[::]` takes IPv4 too, and `0.0.0.0` is for a
/// host without IPv6. A port in use is not tried elsewhere.
pub async fn listen(addrs: &[SocketAddr], shared: Arc<Shared>) -> Result<Listener, BindError> {
    let mut failed = None;
    let mut bound = None;
    for addr in addrs {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                bound = Some(listener);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                return Err(BindError::InUse(addr.port()));
            }
            Err(error) => {
                failed = Some(BindError::Other {
                    port: addr.port(),
                    error,
                })
            }
        }
    }
    let Some(listener) = bound else {
        return Err(failed.unwrap_or(BindError::Other {
            port: super::PORT,
            error: std::io::ErrorKind::AddrNotAvailable.into(),
        }));
    };
    let addr = listener.local_addr().map_err(|error| BindError::Other {
        port: super::PORT,
        error,
    })?;
    let (stop, stopped) = oneshot::channel::<()>();
    let app = router(shared);
    tokio::spawn(async move {
        // Dropping the listener's handle stops it: the sender goes, the receiver resolves.
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = stopped.await;
        });
        if let Err(e) = serve.await {
            tracing::warn!("sendspin: the listener stopped: {e}");
        }
    });
    Ok(Listener { addr, _stop: stop })
}

fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route(PATH, any(upgrade))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(shared)
}

async fn upgrade(
    State(shared): State<Arc<Shared>>,
    method: Method,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if method != Method::GET {
        return StatusCode::NOT_FOUND.into_response();
    }
    // The specification: a server never sends one, a browser always does.
    if headers.contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let Some(slot) = shared.take_slot() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    // An upgrade that fails drops the callback, and the slot with it.
    ws.max_message_size(MESSAGE_MAX)
        .max_frame_size(MESSAGE_MAX)
        .on_upgrade(move |socket| run(socket, id, slot))
}

/// Moves frames until either side closes. Dropping the socket without a close frame, when the
/// service goes away, is what the specification calls a restart: the server reconnects.
async fn run(mut ws: WebSocket, id: u64, slot: Slot) {
    let to_service = slot.0.to_service.clone();
    let (out, mut outgoing) = mpsc::unbounded_channel();
    if to_service
        .send(FromSocket::Opened { id, out })
        .await
        .is_err()
    {
        return;
    }
    let mut ping = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            message = ws.recv() => {
                let at = monotonic_us();
                heard = Instant::now();
                let event = match message {
                    Some(Ok(Message::Text(text))) => FromSocket::Text {
                        id,
                        text: text.as_str().to_owned(),
                        at,
                    },
                    Some(Ok(Message::Binary(bytes))) => FromSocket::Binary { id, bytes, at },
                    // tungstenite answers pings itself.
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        tracing::debug!("sendspin: connection {id}: {e}");
                        break;
                    }
                };
                if to_service.send(event).await.is_err() {
                    break;
                }
            }
            next = outgoing.recv() => {
                let (message, last) = match next {
                    Some(ToSocket::Text(text)) => (Message::text(text), false),
                    Some(ToSocket::Binary(bytes)) => (Message::binary(bytes), false),
                    Some(ToSocket::Close) => (Message::Close(None), true),
                    None => break,
                };
                match tokio::time::timeout(SILENCE_MAX, ws.send(message)).await {
                    Ok(Ok(())) if !last => {}
                    _ => break,
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() >= SILENCE_MAX {
                    tracing::debug!("sendspin: connection {id}: no answer for 30 s; dropping it");
                    break;
                }
                let sent = tokio::time::timeout(SILENCE_MAX, ws.send(Message::Ping(Bytes::new())));
                if !matches!(sent.await, Ok(Ok(()))) {
                    break;
                }
            }
        }
    }
    drop(ws);
    drop(slot);
    let _ = to_service.send(FromSocket::Closed { id }).await;
}
