//! ElevenLabs speech-to-text: `POST {base}/v1/speech-to-text`
//! (ADR 0017 §"The pipeline", architecture §7).
//!
//! The WAV goes up with the Scribe model and an optional `language_code`; the
//! key goes in `xi-api-key`, not a bearer header.

use bytes::Bytes;
use futures_core::future::BoxFuture;
use pinna_http::reqwest::multipart;

use super::{STT_BODY_CAP, Stt, SttSetup, Transcript, join, wav_part};
use crate::{
    error::{ProviderError, Reason, Slot},
    http,
};

/// The ElevenLabs speech-to-text API.
pub struct ElevenLabsStt {
    setup: SttSetup,
    url: String,
}

impl ElevenLabsStt {
    pub fn new(setup: SttSetup) -> Self {
        let url = join(&setup.base_url, "/v1/speech-to-text");
        Self { setup, url }
    }
}

impl Stt for ElevenLabsStt {
    fn transcribe<'a>(
        &'a self,
        wav: Bytes,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            let preset = self.setup.preset;
            let mut form = multipart::Form::new()
                .part("file", wav_part(wav, preset)?)
                .text("model_id", self.setup.model.clone());
            if let Some(language) = language {
                form = form.text("language_code", language.to_string());
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

    const PATH: &str = "/v1/speech-to-text";

    fn setup(server: &FakeServer, model: &str) -> SttSetup {
        SttSetup {
            http: client(ClientKind::Provider {
                read: Some(Duration::from_secs(5)),
            }),
            base_url: server.url(""),
            model: model.into(),
            auth: Auth::ApiKey("xi-api-key", pinna_http::Secret::new("el-key")),
            preset: PresetId::ElevenLabs,
            clock_synced: || true,
        }
    }

    #[tokio::test]
    async fn the_form_carries_the_file_the_model_and_the_language() {
        let server = FakeServer::start().await;
        server.script(
            pinna_http::reqwest::Method::POST,
            PATH,
            FakeResponse::new(StatusCode::OK, "{\"text\":\"hallo welt\"}"),
        );

        let transcript = ElevenLabsStt::new(setup(&server, "scribe_v1"))
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
        let transcript = ElevenLabsStt::new(setup(&server, "scribe_v1"))
            .transcribe(Bytes::from_static(b"RIFFdata"), None)
            .await
            .unwrap();
        assert_eq!(transcript, "hi");
        let requests = server.requests();
        let body = String::from_utf8_lossy(&requests.last().unwrap().body);
        assert!(!body.contains("name=\"language_code\""), "{body}");
    }
}
