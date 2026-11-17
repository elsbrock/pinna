//! The OpenAI-compatible text-to-speech adapter: `POST {base}/audio/speech`
//! (architecture §7).
//!
//! The JSON body carries the model, the voice, the text and `mp3`. The audio
//! is streamed, so the request only waits for the response headers.

use futures_core::future::BoxFuture;
use pinna_http::reqwest;
use serde::Serialize;

use crate::{
    error::{ProviderError, Slot},
    http::{Auth, send},
    presets::PresetId,
    reply::{AudioBody, AudioKind, SpeechAudio},
    tts::{Tts, body_stream},
};

/// One OpenAI-compatible speech endpoint.
pub struct OpenAiTts {
    client: reqwest::Client,
    base_url: String,
    model: String,
    voice: String,
    preset: PresetId,
    auth: Auth,
    clock_synced: fn() -> bool,
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    voice: &'a str,
    input: &'a str,
    response_format: &'static str,
}

impl OpenAiTts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        model: impl Into<String>,
        voice: impl Into<String>,
        preset: PresetId,
        auth: Auth,
        clock_synced: fn() -> bool,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            model: model.into(),
            voice: voice.into(),
            preset,
            auth,
            clock_synced,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/audio/speech", self.base_url)
    }
}

impl Tts for OpenAiTts {
    fn speak<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<SpeechAudio, ProviderError>> {
        Box::pin(async move {
            let request = self.client.post(self.endpoint()).json(&Request {
                model: &self.model,
                voice: &self.voice,
                input: text,
                response_format: "mp3",
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
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::StreamExt;
    use pinna_http::{
        ClientKind, Secret, client,
        reqwest::{Method, StatusCode},
    };

    use super::*;
    use crate::{
        error::Reason,
        testing::{FakeResponse, FakeServer},
    };

    fn adapter(server: &FakeServer) -> OpenAiTts {
        OpenAiTts::new(
            client(ClientKind::Provider { read: None }),
            server.url(""),
            "tts-1",
            "nova",
            PresetId::OpenAi,
            Auth::Bearer(Secret::new("sk-tts")),
            || true,
        )
    }

    #[tokio::test]
    async fn the_body_carries_the_model_voice_and_mp3() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/audio/speech",
            FakeResponse::new(StatusCode::OK, "audio").header("content-type", "audio/mpeg"),
        );

        adapter(&server).speak("Hello there.").await.unwrap();

        let requests = server.requests();
        let request = requests.last().unwrap();
        assert_eq!(request.path, "/audio/speech");
        assert_eq!(request.header("authorization"), Some("Bearer sk-tts"));
        let sent: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(sent["model"], "tts-1");
        assert_eq!(sent["voice"], "nova");
        assert_eq!(sent["input"], "Hello there.");
        assert_eq!(sent["response_format"], "mp3");
    }

    #[tokio::test]
    async fn a_chunked_body_streams_in_order() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/audio/speech",
            FakeResponse::chunked(
                StatusCode::OK,
                ["one", "two", "three"],
                Duration::from_millis(1),
            ),
        );

        let audio = adapter(&server).speak("Hi").await.unwrap();
        assert_eq!(audio.kind, AudioKind::Sentence);
        assert_eq!(audio.mime.as_deref(), Some("audio/mpeg"));

        let AudioBody::Stream(stream) = audio.body else {
            panic!("text-to-speech returns a stream");
        };
        let chunks: Vec<Bytes> = stream.map(|chunk| chunk.unwrap()).collect().await;
        let mut body = Vec::new();
        for chunk in chunks {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, b"onetwothree");
    }

    #[tokio::test]
    async fn a_429_is_an_http_error() {
        let server = FakeServer::start().await;
        server.script_status(Method::POST, "/audio/speech", StatusCode::TOO_MANY_REQUESTS);

        let error = adapter(&server).speak("Hi").await.err().unwrap();
        assert_eq!(
            error,
            ProviderError::new(Slot::Tts, PresetId::OpenAi, Reason::Http(429))
        );
    }
}
