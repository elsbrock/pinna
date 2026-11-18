//! The text-to-speech slot: one adapter per wire protocol.
//!
//! [`Tts::speak`] resolves once the provider's audio response headers arrive
//! and hands the body back as a byte stream, so the reply player can start
//! before synthesis has finished.

use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_core::{Stream, future::BoxFuture, stream::BoxStream};
use pinna_http::reqwest;

use crate::{error::ProviderError, http::Endpoint, presets::SpeechProtocol, reply::SpeechAudio};

pub mod elevenlabs;
pub mod openai;

pub use elevenlabs::ElevenLabsTts;
pub use openai::OpenAiTts;

/// The adapter for a protocol: its own model, the owner's voice and, for
/// ElevenLabs, the language.
/// `model` is the slot's own text-to-speech model; `None` is the preset's.
pub fn adapter(
    protocol: SpeechProtocol,
    endpoint: Endpoint,
    voice: String,
    model: Option<String>,
    language: Option<String>,
) -> Arc<dyn Tts> {
    let model = model.unwrap_or_else(|| protocol.tts_model().to_owned());
    match protocol {
        SpeechProtocol::OpenAi => Arc::new(OpenAiTts::new(endpoint, model, voice)),
        SpeechProtocol::ElevenLabs => {
            Arc::new(ElevenLabsTts::new(endpoint, model, voice, language))
        }
    }
}

/// Synthesise one phrase into audio for the voice lane.
pub trait Tts: Send + Sync {
    fn speak<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<SpeechAudio, ProviderError>>;
}

type ProviderBody = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// The audio body as an `io` stream, which is what the speaker takes.
fn body_stream(response: reqwest::Response) -> BoxStream<'static, io::Result<Bytes>> {
    Box::pin(BodyStream {
        inner: Box::pin(response.bytes_stream()),
    })
}

struct BodyStream {
    inner: ProviderBody,
}

impl Stream for BodyStream {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.inner
            .as_mut()
            .poll_next(cx)
            .map(|item| item.map(|result| result.map_err(io::Error::other)))
    }
}
