//! The speech-to-text slot: one adapter per wire protocol (ADR 0017
//! §"The pipeline", architecture §7).
//!
//! An adapter is a small IO shell around the request helpers in [`crate::http`].
//! Provider clients follow no redirects, so a key stays on the origin its
//! connection names. A failed save-time check or a run both use the same
//! adapter, so the transcripts and errors are identical.

mod guard;
mod openai;

pub use guard::is_invented;
pub use openai::OpenAiStt;

use bytes::Bytes;
use futures_core::future::BoxFuture;
use pinna_http::reqwest::{self, multipart};

use crate::{
    error::{ProviderError, Reason, Slot},
    http::Auth,
    presets::PresetId,
};

/// The most a transcript body may be. A 15 s utterance becomes at most a few
/// kilobytes of text; anything larger is not a transcript.
pub const STT_BODY_CAP: usize = 256 * 1024;

/// What every speech adapter needs: the connection's URL and key, the model,
/// and whether the clock is set, so a TLS failure before NTP can be named.
#[derive(Clone)]
pub struct SttSetup {
    pub http: reqwest::Client,
    pub base_url: String,
    pub model: String,
    pub auth: Auth,
    pub preset: PresetId,
    pub clock_synced: fn() -> bool,
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

/// Join a path to a base URL without doubling a trailing slash.
pub(crate) fn join(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// The WAV file part every form sends.
pub(crate) fn wav_part(wav: Bytes, preset: PresetId) -> Result<multipart::Part, ProviderError> {
    multipart::Part::bytes(wav.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|_| ProviderError::new(Slot::Stt, preset, Reason::Internal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joining_tolerates_a_trailing_slash() {
        assert_eq!(
            join("https://api.openai.com/v1", "/audio/transcriptions"),
            "https://api.openai.com/v1/audio/transcriptions"
        );
        assert_eq!(
            join("https://api.elevenlabs.io/", "/v1/speech-to-text"),
            "https://api.elevenlabs.io/v1/speech-to-text"
        );
    }
}
