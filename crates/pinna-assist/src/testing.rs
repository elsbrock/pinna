//! A scripted local HTTP server for adapter tests. Available with `testing`.

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::Response,
    routing::any,
};
use bytes::Bytes;
use tokio::{net::TcpListener, task::JoinHandle};

const REQUEST_BODY_CAP: usize = 2 * 1024 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("fake server mutex")
}

/// A recorded request, including its body and the headers an adapter sent.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: Method,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }
}

/// One scripted response. Multiple chunks are sent in order with `delay`
/// between them, which lets tests exercise SSE decoders across chunk edges.
#[derive(Clone, Debug)]
pub struct FakeResponse {
    status: StatusCode,
    headers: HeaderMap,
    chunks: Vec<Bytes>,
    delay: Duration,
}

impl FakeResponse {
    pub fn new(status: StatusCode, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            chunks: vec![body.into()],
            delay: Duration::ZERO,
        }
    }

    pub fn empty(status: StatusCode) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            chunks: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    pub fn chunked<I, B>(status: StatusCode, chunks: I, delay: Duration) -> Self
    where
        I: IntoIterator<Item = B>,
        B: Into<Bytes>,
    {
        Self {
            status,
            headers: HeaderMap::new(),
            chunks: chunks.into_iter().map(Into::into).collect(),
            delay,
        }
    }

    pub fn sse<I, B>(chunks: I, delay: Duration) -> Self
    where
        I: IntoIterator<Item = B>,
        B: Into<Bytes>,
    {
        Self::chunked(StatusCode::OK, chunks, delay).header("content-type", "text/event-stream")
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(value).expect("valid fake response header"),
        );
        self
    }

    fn into_response(self) -> Response {
        let Self {
            status,
            headers,
            chunks,
            delay,
        } = self;
        let body = match chunks.len() {
            0 => Body::empty(),
            1 if delay.is_zero() => Body::from(chunks.into_iter().next().unwrap()),
            _ => {
                let stream = futures_util::stream::unfold(
                    (chunks, 0_usize),
                    move |(chunks, index)| async move {
                        let chunk = chunks.get(index)?.clone();
                        if index > 0 && !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        Some((Ok::<Bytes, Infallible>(chunk), (chunks, index + 1)))
                    },
                );
                Body::from_stream(stream)
            }
        };
        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        response
    }
}

type Scripts = Arc<Mutex<HashMap<(Method, String), VecDeque<FakeResponse>>>>;

#[derive(Clone, Default)]
struct ServerState {
    scripts: Scripts,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

/// A server bound to an ephemeral loopback port. Responses are consumed in
/// the order they were scripted for each method and path.
pub struct FakeServer {
    address: SocketAddr,
    state: ServerState,
    task: JoinHandle<()>,
}

impl FakeServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = ServerState::default();
        let app = Router::new()
            .fallback(any(handle))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            address,
            state,
            task,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn script(&self, method: Method, path: &str, response: FakeResponse) {
        lock(&self.state.scripts)
            .entry((method, path.to_owned()))
            .or_default()
            .push_back(response);
    }

    pub fn script_status(&self, method: Method, path: &str, status: StatusCode) {
        self.script(method, path, FakeResponse::empty(status));
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state.requests).clone()
    }

    pub fn take_requests(&self) -> Vec<RecordedRequest> {
        std::mem::take(&mut *lock(&self.state.requests))
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(state): State<ServerState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map_or(parts.uri.path(), |path| path.as_str())
        .to_owned();
    let body = match to_bytes(body, REQUEST_BODY_CAP).await {
        Ok(body) => body,
        Err(_) => return FakeResponse::empty(StatusCode::PAYLOAD_TOO_LARGE).into_response(),
    };
    lock(&state.requests).push(RecordedRequest {
        method: parts.method.clone(),
        path: path.clone(),
        headers: parts.headers,
        body,
    });
    lock(&state.scripts)
        .get_mut(&(parts.method, path))
        .and_then(VecDeque::pop_front)
        .unwrap_or_else(|| FakeResponse::empty(StatusCode::NOT_FOUND))
        .into_response()
}
