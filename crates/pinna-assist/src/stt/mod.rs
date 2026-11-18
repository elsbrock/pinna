//! The speech-to-text slot: one adapter per wire protocol (ADR 0017
//! §"The pipeline").
//!
//! An adapter is a small IO shell around the request helpers in [`crate::http`].
//! Provider clients follow no redirects, so a key stays on the origin its
//! connection names. A failed save-time check or a run both use the same
//! adapter, so the transcripts and errors are identical.

mod elevenlabs;
mod guard;
mod openai;

pub use elevenlabs::ElevenLabsStt;
pub use guard::is_invented;
pub use openai::OpenAiStt;

use std::sync::Arc;

use bytes::Bytes;
use futures_core::future::BoxFuture;
use pinna_http::reqwest::multipart;

use crate::{
    error::{ProviderError, Reason, Slot},
    http::Endpoint,
    presets::{PresetId, SpeechProtocol},
};

/// The most a transcript body may be. A 15 s utterance becomes at most a few
/// kilobytes of text; anything larger is not a transcript.
pub const STT_BODY_CAP: usize = 256 * 1024;

/// The adapter for a protocol, with the model to ask for.
pub fn adapter(protocol: SpeechProtocol, endpoint: Endpoint, model: String) -> Arc<dyn Stt> {
    match protocol {
        SpeechProtocol::OpenAi => Arc::new(OpenAiStt::new(endpoint, model)),
        SpeechProtocol::ElevenLabs => Arc::new(ElevenLabsStt::new(endpoint, model)),
    }
}

/// One speech-to-text provider: send WAV, get a transcript.
pub trait Stt: Send + Sync {
    fn transcribe<'a>(
        &'a self,
        wav: Bytes,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, ProviderError>>;
}

/// The transcript shared by the OpenAI-compatible and ElevenLabs responses.
/// Both wrap the text in a JSON object with other fields the adapter ignores.
#[derive(serde::Deserialize)]
pub(crate) struct Transcript {
    pub text: String,
}

/// The WAV file part every form sends.
pub(crate) fn wav_part(wav: Bytes, preset: PresetId) -> Result<multipart::Part, ProviderError> {
    // A `Bytes` body has a known length and is sent without another copy.
    multipart::Part::stream(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|_| ProviderError::new(Slot::Stt, preset, Reason::Internal))
}
