//! OpenAI-compatible speech-to-text: `POST {base}/audio/transcriptions`
//! (ADR 0017 §"The pipeline", architecture §7).
//!
//! This covers OpenAI, Groq and every self-hosted server that copies the audio
//! API. The WAV goes up as a multipart file with the model, an optional
//! language hint and `response_format=json`; the transcript comes back as JSON.

use bytes::Bytes;
use futures_core::future::BoxFuture;
use pinna_http::reqwest::multipart;

use super::{STT_BODY_CAP, Stt, SttSetup, Transcript, join, wav_part};
use crate::{
    error::{ProviderError, Reason, Slot},
    http,
};

/// An OpenAI-compatible speech-to-text server.
pub struct OpenAiStt {
    setup: SttSetup,
    url: String,
}

impl OpenAiStt {
    pub fn new(setup: SttSetup) -> Self {
        let url = join(&setup.base_url, "/audio/transcriptions");
        Self { setup, url }
    }
}

impl Stt for OpenAiStt {
    fn transcribe<'a>(
        &'a self,
        wav: Bytes,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            let preset = self.setup.preset;
            let mut form = multipart::Form::new()
                .part("file", wav_part(wav, preset)?)
                .text("model", self.setup.model.clone())
                .text("response_format", "json");
            if let Some(language) = language {
                form = form.text("language", language.to_string());
            }

            let request = self.setup.http.post(&self.url).multipart(form);
            let response = http::send(
                request,
                &self.setup.auth,
                Slot::Stt,
                preset,
                self.setup.clock_synced,
            )
            .await?;
            let body = http::read_capped(
                response,
                STT_BODY_CAP,
                Slot::Stt,
                preset,
                self.setup.clock_synced,
            )
            .await?;
            let transcript: Transcript = serde_json::from_slice(&body)
                .map_err(|_| ProviderError::new(Slot::Stt, preset, Reason::Protocol))?;
            Ok(transcript.text)
        })
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use std::time::Duration;

    use pinna_http::{ClientKind, client, reqwest::StatusCode};

    use super::*;
    use crate::{
        http::Auth,
        presets::PresetId,
        testing::{FakeResponse, FakeServer},
    };

    const PATH: &str = "/audio/transcriptions";

    fn setup(server: &FakeServer, model: &str) -> SttSetup {
        SttSetup {
            http: client(ClientKind::Provider {
                read: Some(Duration::from_secs(5)),
            }),
            base_url: server.url(""),
            model: model.into(),
            auth: Auth::Bearer(pinna_http::Secret::new("sk-test")),
            preset: PresetId::OpenAi,
            clock_synced: || true,
        }
    }

    #[tokio::test]
    async fn the_form_carries_the_file_the_model_and_the_language() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::new(StatusCode::OK, "{\"text\":\"hello world\"}"),
        );

        let transcript = OpenAiStt::new(setup(&server, "whisper-1"))
            .transcribe(Bytes::from_static(b"RIFFdata"), Some("en"))
            .await
            .unwrap();
        assert_eq!(transcript, "hello world");

        let requests = server.requests();
        let request = requests.last().unwrap();
        assert_eq!(request.path, PATH);
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));

        let body = String::from_utf8_lossy(&request.body);
        assert!(body.contains("name=\"file\""), "{body}");
        assert!(body.contains("filename=\"audio.wav\""), "{body}");
        assert!(body.contains("audio/wav"), "{body}");
        assert!(body.contains("name=\"model\""), "{body}");
        assert!(body.contains("whisper-1"), "{body}");
        assert!(body.contains("name=\"response_format\""), "{body}");
        assert!(body.contains("name=\"language\""), "{body}");
        assert!(body.contains("\r\n\r\nen\r\n"), "{body}");
    }

    #[tokio::test]
    async fn the_language_field_is_left_out_when_unset() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::new(StatusCode::OK, "{\"text\":\"hi\"}"),
        );
        let transcript = OpenAiStt::new(setup(&server, "whisper-1"))
            .transcribe(Bytes::from_static(b"RIFFdata"), None)
            .await
            .unwrap();
        assert_eq!(transcript, "hi");
        let requests = server.requests();
        let body = String::from_utf8_lossy(&requests.last().unwrap().body);
        assert!(!body.contains("name=\"language\""), "{body}");
        assert!(body.contains("name=\"response_format\""), "{body}");
    }

    #[tokio::test]
    async fn a_wrong_status_is_an_http_reason() {
        let server = FakeServer::start().await;
        server.script_status(
            pinna_http::reqwest::Method::POST,
            PATH,
            StatusCode::UNAUTHORIZED,
        );
        let error = OpenAiStt::new(setup(&server, "whisper-1"))
            .transcribe(Bytes::from_static(b"RIFFdata"), None)
            .await
            .unwrap_err();
        assert_eq!(error.reason, Reason::Http(401));
        assert_eq!(error.slot, Slot::Stt);
        assert_eq!(error.preset, PresetId::OpenAi);
        assert!(!error.to_string().contains("sk-test"));
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_a_protocol_error() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::new(StatusCode::OK, "not json"),
        );
        let error = OpenAiStt::new(setup(&server, "whisper-1"))
            .transcribe(Bytes::from_static(b"RIFFdata"), None)
            .await
            .unwrap_err();
        assert_eq!(error.reason, Reason::Protocol);
    }

    #[tokio::test]
    async fn a_slow_body_is_a_timeout() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::chunked(
                StatusCode::OK,
                ["{\"text\":\"", "hi\"}"],
                Duration::from_millis(200),
            ),
        );
        let mut slow = setup(&server, "whisper-1");
        slow.http = client(ClientKind::Provider {
            read: Some(Duration::from_millis(5)),
        });
        let error = OpenAiStt::new(slow)
            .transcribe(Bytes::from_static(b"RIFFdata"), None)
            .await
            .unwrap_err();
        assert_eq!(error.reason, Reason::Timeout);
    }
}
