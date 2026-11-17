//! The ElevenLabs text-to-speech adapter:
//! `POST {base}/v1/text-to-speech/{voice}/stream?output_format=mp3_44100_64`
//! (architecture §7).
//!
//! The voice id is a path segment, so it is built through [`url::Url`] and
//! percent-encoded. Authentication is the `xi-api-key` header.

use futures_core::future::BoxFuture;
use pinna_http::reqwest;
use serde::Serialize;

use crate::{
    error::{ProviderError, Reason, Slot},
    http::{Auth, send},
    presets::PresetId,
    reply::{AudioBody, AudioKind, SpeechAudio},
    tts::{Tts, body_stream},
};

/// One ElevenLabs speech endpoint.
pub struct ElevenLabsTts {
    client: reqwest::Client,
    base_url: String,
    model: String,
    voice: String,
    language: Option<String>,
    preset: PresetId,
    auth: Auth,
    clock_synced: fn() -> bool,
}

#[derive(Serialize)]
struct Request<'a> {
    text: &'a str,
    model_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_code: Option<&'a str>,
}

impl ElevenLabsTts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        model: impl Into<String>,
        voice: impl Into<String>,
        language: Option<String>,
        preset: PresetId,
        auth: Auth,
        clock_synced: fn() -> bool,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            model: model.into(),
            voice: voice.into(),
            language,
            preset,
            auth,
            clock_synced,
        }
    }

    fn endpoint(&self) -> Result<url::Url, ProviderError> {
        let mut url = url::Url::parse(&self.base_url).map_err(|_| self.fail())?;
        url.path_segments_mut().map_err(|_| self.fail())?.extend([
            "v1",
            "text-to-speech",
            self.voice.as_str(),
            "stream",
        ]);
        url.query_pairs_mut()
            .append_pair("output_format", "mp3_44100_64");
        Ok(url)
    }

    fn fail(&self) -> ProviderError {
        ProviderError::new(Slot::Tts, self.preset, Reason::Protocol)
    }
}

impl Tts for ElevenLabsTts {
    fn speak<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<SpeechAudio, ProviderError>> {
        Box::pin(async move {
            let request = self.client.post(self.endpoint()?).json(&Request {
                text,
                model_id: &self.model,
                language_code: self.language.as_deref(),
            });
            let response = send(
                request,
                &self.auth,
                Slot::Tts,
                self.preset,
                self.clock_synced,
            )
            .await?;
            Ok(SpeechAudio {
                kind: AudioKind::Sentence,
                mime: Some("audio/mpeg".into()),
                body: AudioBody::Stream(body_stream(response)),
            })
        })
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use bytes::Bytes;
    use futures_util::StreamExt;
    use pinna_http::{
        ClientKind, Secret, client,
        reqwest::{Method, StatusCode},
    };

    use super::*;
    use crate::testing::{FakeResponse, FakeServer};

    fn adapter(server: &FakeServer, voice: &str) -> ElevenLabsTts {
        ElevenLabsTts::new(
            client(ClientKind::Provider { read: None }),
            server.url(""),
            "eleven_multilingual_v2",
            voice,
            Some("de".into()),
            PresetId::ElevenLabs,
            Auth::ApiKey("xi-api-key", Secret::new("xi-secret")),
            || true,
        )
    }

    #[tokio::test]
    async fn the_path_header_and_body_match() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/text-to-speech/nova/stream?output_format=mp3_44100_64",
            FakeResponse::new(StatusCode::OK, "audio").header("content-type", "audio/mpeg"),
        );

        adapter(&server, "nova").speak("Hallo.").await.unwrap();

        let requests = server.requests();
        let request = requests.last().unwrap();
        assert_eq!(
            request.path,
            "/v1/text-to-speech/nova/stream?output_format=mp3_44100_64"
        );
        assert_eq!(request.header("xi-api-key"), Some("xi-secret"));
        assert_eq!(request.header("authorization"), None);
        let sent: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(sent["text"], "Hallo.");
        assert_eq!(sent["model_id"], "eleven_multilingual_v2");
        assert_eq!(sent["language_code"], "de");
    }

    #[tokio::test]
    async fn the_voice_id_is_escaped() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/text-to-speech/a%2Fb/stream?output_format=mp3_44100_64",
            FakeResponse::chunked(
                StatusCode::OK,
                ["au", "dio"],
                std::time::Duration::from_millis(1),
            ),
        );

        let audio = adapter(&server, "a/b").speak("Hi").await.unwrap();
        let AudioBody::Stream(stream) = audio.body else {
            panic!("text-to-speech returns a stream");
        };
        let chunks: Vec<Bytes> = stream.map(|chunk| chunk.unwrap()).collect().await;
        let mut body = Vec::new();
        for chunk in chunks {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, b"audio");

        let requests = server.requests();
        assert_eq!(
            requests.last().unwrap().path,
            "/v1/text-to-speech/a%2Fb/stream?output_format=mp3_44100_64"
        );
    }

    #[tokio::test]
    async fn no_language_leaves_the_field_out() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/text-to-speech/nova/stream?output_format=mp3_44100_64",
            FakeResponse::new(StatusCode::OK, "audio"),
        );

        let tts = ElevenLabsTts::new(
            client(ClientKind::Provider { read: None }),
            server.url(""),
            "eleven_multilingual_v2",
            "nova",
            None,
            PresetId::ElevenLabs,
            Auth::ApiKey("xi-api-key", Secret::new("xi-secret")),
            || true,
        );
        tts.speak("Hi").await.unwrap();

        let requests = server.requests();
        let sent: serde_json::Value =
            serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        assert!(sent.get("language_code").is_none());
    }
}
