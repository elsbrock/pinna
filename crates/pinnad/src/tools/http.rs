//! The capped JSON GET the weather and radio tools share (ADR 0017
//! §"The pipeline").

use pinna_http::{NetError, classify, reqwest};
use serde::de::DeserializeOwned;

/// A forecast or a directory search is a few kilobytes; anything far larger
/// isn't one.
pub(super) const BODY_CAP: usize = 256 * 1024;

/// Why a lookup failed, for a sentence that names the service.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Timeout,
    Unreachable,
    Http(u16),
    /// Not JSON in the expected shape, or too large to be it.
    Unreadable,
}

impl Failure {
    fn net(e: reqwest::Error) -> Self {
        match classify(&e) {
            NetError::Timeout => Self::Timeout,
            NetError::Unreachable | NetError::Tls => Self::Unreachable,
            NetError::Protocol => Self::Unreadable,
        }
    }

    pub(super) fn sentence(&self, service: &str) -> String {
        match self {
            Self::Timeout => format!("{service} didn't answer in time. Try again later."),
            Self::Unreachable => format!("{service} can't be reached. Try again later."),
            Self::Http(status) => format!("{service} answered {status}. Try again later."),
            Self::Unreadable => format!("{service} sent an answer that can't be read."),
        }
    }
}

/// Send `request` and read at most [`BODY_CAP`] bytes of JSON.
pub(super) async fn get_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, Failure> {
    let mut response = pinna_http::trace::send(pinna_http::trace::Kind::Tool, request)
        .await
        .map_err(Failure::net)?;
    if !response.status().is_success() {
        return Err(Failure::Http(response.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Failure::net)? {
        if chunk.len() > BODY_CAP - body.len() {
            return Err(Failure::Unreadable);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| Failure::Unreadable)
}
