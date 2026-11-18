//! OpenAI-compatible speech-to-text: `POST {base}/audio/transcriptions`
//! (ADR 0017 §"The pipeline", architecture §7).
//!
//! This covers OpenAI, Groq and every self-hosted server that copies the audio
//! API. The WAV goes up as a multipart file with the model, an optional
//! language hint and `response_format=json`; the transcript comes back as JSON.

use bytes::Bytes;
use futures_core::future::BoxFuture;
use pinna_http::reqwest::multipart;

use super::{STT_BODY_CAP, Stt, Transcript, wav_part};
use crate::{
    error::{ProviderError, Reason, Slot},
    http::Endpoint,
};

/// An OpenAI-compatible speech-to-text server.
pub struct OpenAiStt {
    endpoint: Endpoint,
    model: String,
    url: String,
}

impl OpenAiStt {
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Self {
        let url = endpoint.url("/audio/transcriptions");
        Self {
            endpoint,
            model: model.into(),
            url,
        }
    }
}

impl Stt for OpenAiStt {
    fn transcribe<'a>(
        &'a self,
        wav: Bytes,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            let preset = self.endpoint.preset;
            let mut form = multipart::Form::new()
                .part("file", wav_part(wav, preset)?)
                .text("model", self.model.clone())
                .text("response_format", "json");
            if let Some(language) = language {
                form = form.text("language", language.to_string());
            }

            let request = self.endpoint.http.post(&self.url).multipart(form);
            let response = self.endpoint.send(request, Slot::Stt).await?;
            let body = self
                .endpoint
                .read_capped(response, STT_BODY_CAP, Slot::Stt)
                .await?;
            let transcript: Transcript = serde_json::from_slice(&body)
                .map_err(|_| self.endpoint.error(Slot::Stt, Reason::Protocol))?;
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

    fn endpoint(server: &FakeServer) -> Endpoint {
        Endpoint::new(
            client(ClientKind::Provider {
                read: Some(Duration::from_secs(5)),
            }),
            &server.url(""),
            Auth::Bearer(pinna_http::Secret::new("sk-test")),
            PresetId::OpenAi,
            || true,
        )
    }

    fn stt(server: &FakeServer, model: &str) -> OpenAiStt {
        OpenAiStt::new(endpoint(server), model)
    }

    #[tokio::test]
    async fn the_form_carries_the_file_the_model_and_the_language() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::new(StatusCode::OK, "{\"text\":\"hello world\"}"),
        );

        let transcript = stt(&server, "whisper-1")
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
        let transcript = stt(&server, "whisper-1")
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
        let error = stt(&server, "whisper-1")
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
        let error = stt(&server, "whisper-1")
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
        let mut slow = endpoint(&server);
        slow.http = client(ClientKind::Provider {
            read: Some(Duration::from_millis(5)),
        });
        let error = OpenAiStt::new(slow, "whisper-1")
            .transcribe(Bytes::from_static(b"RIFFdata"), None)
            .await
            .unwrap_err();
        assert_eq!(error.reason, Reason::Timeout);
    }
}
