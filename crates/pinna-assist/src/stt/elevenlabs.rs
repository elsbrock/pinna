//! ElevenLabs speech-to-text: `POST {base}/v1/speech-to-text`
//! (ADR 0017 §"The pipeline", architecture §7).
//!
//! The WAV goes up with the Scribe model and an optional `language_code`; the
//! key goes in `xi-api-key`, not a bearer header.

use bytes::Bytes;
use futures_core::future::BoxFuture;
use pinna_http::reqwest::multipart;

use super::{STT_BODY_CAP, Stt, Transcript, wav_part};
use crate::{
    error::{ProviderError, Reason, Slot},
    http::Endpoint,
};

/// The ElevenLabs speech-to-text API.
pub struct ElevenLabsStt {
    endpoint: Endpoint,
    model: String,
    url: String,
}

impl ElevenLabsStt {
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Self {
        let url = endpoint.url("/v1/speech-to-text");
        Self {
            endpoint,
            model: model.into(),
            url,
        }
    }
}

impl Stt for ElevenLabsStt {
    fn transcribe<'a>(
        &'a self,
        wav: Bytes,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            let preset = self.endpoint.preset;
            let mut form = multipart::Form::new()
                .part("file", wav_part(wav, preset)?)
                .text("model_id", self.model.clone());
            if let Some(language) = language {
                form = form.text("language_code", language.to_string());
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pinna_http::{ClientKind, client, reqwest::StatusCode};

    use super::*;
    use crate::{
        http::Auth,
        presets::PresetId,
        testing::{FakeResponse, FakeServer},
    };

    const PATH: &str = "/v1/speech-to-text";

    fn endpoint(server: &FakeServer) -> Endpoint {
        Endpoint::new(
            client(ClientKind::Provider {
                read: Some(Duration::from_secs(5)),
            }),
            &server.url(""),
            Auth::ApiKey("xi-api-key", pinna_http::Secret::new("el-key")),
            PresetId::ElevenLabs,
            || true,
        )
    }

    fn stt(server: &FakeServer, model: &str) -> ElevenLabsStt {
        ElevenLabsStt::new(endpoint(server), model)
    }

    #[tokio::test]
    async fn the_form_carries_the_file_the_model_and_the_language() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::new(StatusCode::OK, "{\"text\":\"hallo welt\"}"),
        );

        let transcript = stt(&server, "scribe_v1")
            .transcribe(Bytes::from_static(b"RIFFdata"), Some("de"))
            .await
            .unwrap();
        assert_eq!(transcript, "hallo welt");

        let requests = server.requests();
        let request = requests.last().unwrap();
        assert_eq!(request.path, PATH);
        assert_eq!(request.header("xi-api-key"), Some("el-key"));
        assert_eq!(request.header("authorization"), None);

        let body = String::from_utf8_lossy(&request.body);
        assert!(body.contains("name=\"file\""), "{body}");
        assert!(body.contains("filename=\"audio.wav\""), "{body}");
        assert!(body.contains("name=\"model_id\""), "{body}");
        assert!(body.contains("scribe_v1"), "{body}");
        assert!(body.contains("name=\"language_code\""), "{body}");
        assert!(body.contains("\r\n\r\nde\r\n"), "{body}");
    }

    #[tokio::test]
    async fn the_language_code_is_left_out_when_unset() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::new(StatusCode::OK, "{\"text\":\"hi\"}"),
        );
        let transcript = stt(&server, "scribe_v1")
            .transcribe(Bytes::from_static(b"RIFFdata"), None)
            .await
            .unwrap();
        assert_eq!(transcript, "hi");
        let requests = server.requests();
        let body = String::from_utf8_lossy(&requests.last().unwrap().body);
        assert!(!body.contains("name=\"language_code\""), "{body}");
    }
}
