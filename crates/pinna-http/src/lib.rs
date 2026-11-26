//! HTTP client policies and the satellite's shared TLS configuration (ADR 0017).

mod client;
mod error;
mod secret;
mod sse;
mod tls;
pub mod trace;

#[cfg(feature = "blocking")]
pub use client::blocking_client;
pub use client::{ClientKind, client};
pub use error::{NetError, classify};
pub use reqwest;
pub use secret::Secret;
pub use sse::{SseDecoder, SseError, SseEvent};
pub use tls::{CA_BUNDLE, tls, tls_from};
