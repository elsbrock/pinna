//! Save-time checks for a provider connection (ADR 0017, architecture §7).
//!
//! A check is the cheapest call that proves a connection works: list the
//! models, transcribe half a second of silence, or synthesise one word. No
//! check ever sends a prompt, and a hand-off brain is only asked for its model
//! list. A failure becomes one owner sentence through [`refusal`]; it never
//! carries a URL, a key or a response body.

use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use pinna_http::{NetError, classify, reqwest};

use crate::{
    audio,
    error::{ProviderError, Reason, Slot},
    http::{self, Auth},
    presets::{ChatProtocol, Delegation, PresetId, SpeechProtocol, preset},
    reply::AudioBody,
    stt::{ElevenLabsStt, OpenAiStt, Stt, SttSetup},
    tts::{ElevenLabsTts, OpenAiTts, Tts},
};

/// A check gives up after this long (architecture §7).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The model-list body may be this large. Beyond it the status alone passes and
/// the model isn't confirmed (architecture §7).
pub const MODELS_CAP: usize = 2 * 1024 * 1024;

/// Half a second of 16 kHz mono signed 16-bit silence, the smallest utterance
/// that proves a speech-to-text server without a model list.
const SILENCE_BYTES: usize = 16_000 * 500 / 1000 * 2;

/// Everything one save-time check needs. The caller fills in the fields the
/// slot uses; `voice` only matters for [`Slot::Tts`].
pub struct CheckSetup {
    pub slot: Slot,
    pub http: reqwest::Client,
    pub preset: PresetId,
    pub base_url: String,
    /// The model id: the brain's model, the speech-to-text model, or the
    /// text-to-speech model.
    pub model: String,
    /// The voice id for the text-to-speech slot.
    pub voice: Option<String>,
    pub auth: Auth,
    pub clock_synced: fn() -> bool,
}

/// Check one connection, returning an owner sentence when it fails.
pub async fn check(setup: &CheckSetup) -> Result<(), String> {
    match tokio::time::timeout(CHECK_TIMEOUT, run_check(setup)).await {
        Ok(result) => result,
        Err(_) => Err(refusal(&ProviderError::new(
            setup.slot,
            setup.preset,
            Reason::Timeout,
        ))),
    }
}

async fn run_check(setup: &CheckSetup) -> Result<(), String> {
    match setup.slot {
        Slot::Stt => stt_check(setup).await,
        Slot::Chat => chat_check(setup).await,
        Slot::Tts => tts_check(setup).await,
        // A tool is not a connection; there is nothing to check.
        Slot::Tool => Ok(()),
    }
}

/// List the models, or `None` when the body was too large to confirm one.
async fn models(setup: &CheckSetup, slot: Slot) -> Result<Option<Bytes>, ProviderError> {
    let url = format!(
        "{}{}",
        setup.base_url.trim_end_matches('/'),
        models_path(setup.preset)
    );
    let response = http::send(
        setup.http.get(url),
        &setup.auth,
        slot,
        setup.preset,
        setup.clock_synced,
    )
    .await?;
    match http::read_capped(response, MODELS_CAP, slot, setup.preset, setup.clock_synced).await {
        Ok(body) => Ok(Some(body)),
        Err(error) if error.reason == Reason::TooLarge => Ok(None),
        Err(error) => Err(error),
    }
}

async fn stt_check(setup: &CheckSetup) -> Result<(), String> {
    match models(setup, Slot::Stt).await {
        Ok(_) => Ok(()),
        // Some self-hosted servers have no model list. A short silence is the
        // smallest transcription that still proves the key and the address.
        Err(error) if error.reason == Reason::Http(404) => stt_adapter(setup)
            .transcribe(silence_wav(), None)
            .await
            .map(|_| ())
            .map_err(|error| refusal(&error)),
        Err(error) => Err(refusal(&error)),
    }
}

async fn chat_check(setup: &CheckSetup) -> Result<(), String> {
    let body = models(setup, Slot::Chat)
        .await
        .map_err(|error| refusal(&error))?;
    // A hand-off brain owns its own model names; a successful list is enough.
    if preset(setup.preset).behaviour.delegation == Delegation::HandOff {
        return Ok(());
    }
    let Some(body) = body else {
        return Ok(());
    };
    if let Some(ids) = offered_models(&body)
        && !ids.iter().any(|id| id == &setup.model)
    {
        return Err(format!(
            "The model {} isn't offered by this service.",
            setup.model
        ));
    }
    Ok(())
}

async fn tts_check(setup: &CheckSetup) -> Result<(), String> {
    let audio = tts_adapter(setup)
        .speak("OK.")
        .await
        .map_err(|error| refusal(&error))?;
    let AudioBody::Stream(mut stream) = audio.body else {
        return Ok(());
    };
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) if !bytes.is_empty() => return Ok(()),
            Ok(_) => continue,
            Err(error) => {
                let reason = body_error_reason(&error, (setup.clock_synced)());
                return Err(refusal(&ProviderError::new(
                    Slot::Tts,
                    setup.preset,
                    reason,
                )));
            }
        }
    }
    Err("The text-to-speech service sent no audio.".to_owned())
}

fn stt_adapter(setup: &CheckSetup) -> Box<dyn Stt + '_> {
    let inner = SttSetup {
        http: setup.http.clone(),
        base_url: setup.base_url.clone(),
        model: setup.model.clone(),
        auth: setup.auth.clone(),
        preset: setup.preset,
        clock_synced: setup.clock_synced,
    };
    match preset(setup.preset).speech_protocol {
        SpeechProtocol::ElevenLabs => Box::new(ElevenLabsStt::new(inner)),
        SpeechProtocol::OpenAi => Box::new(OpenAiStt::new(inner)),
    }
}

fn tts_adapter(setup: &CheckSetup) -> Box<dyn Tts + '_> {
    let voice = setup.voice.clone().unwrap_or_default();
    match preset(setup.preset).speech_protocol {
        SpeechProtocol::ElevenLabs => Box::new(ElevenLabsTts::new(
            setup.http.clone(),
            setup.base_url.clone(),
            setup.model.clone(),
            voice,
            None,
            setup.preset,
            setup.auth.clone(),
            setup.clock_synced,
        )),
        SpeechProtocol::OpenAi => Box::new(OpenAiTts::new(
            setup.http.clone(),
            setup.base_url.clone(),
            setup.model.clone(),
            voice,
            setup.preset,
            setup.auth.clone(),
            setup.clock_synced,
        )),
    }
}

fn silence_wav() -> Bytes {
    audio::wav(&[Bytes::from(vec![0u8; SILENCE_BYTES])])
}

/// The model-list path for a protocol. The base URL already carries the API
/// version for an OpenAI-compatible connection.
fn models_path(id: PresetId) -> &'static str {
    match preset(id).chat_protocol {
        ChatProtocol::Anthropic => "/v1/models",
        ChatProtocol::Gemini => "/v1beta/models",
        ChatProtocol::OpenAi => "/models",
    }
}

#[derive(serde::Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

#[derive(serde::Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

/// The ids in a model list, or `None` when the body isn't an authoritative
/// list: it didn't parse, or it named no models.
fn offered_models(body: &Bytes) -> Option<Vec<String>> {
    let list: ModelList = serde_json::from_slice(body).ok()?;
    let mut ids: Vec<String> = list
        .data
        .into_iter()
        .map(|entry| entry.id)
        .filter(|id| !id.is_empty())
        .collect();
    ids.extend(
        list.models
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| !name.is_empty())
            .map(|name| name.strip_prefix("models/").unwrap_or(&name).to_owned()),
    );
    (!ids.is_empty()).then_some(ids)
}

fn network_reason(error: &reqwest::Error, clock_synced: bool) -> Reason {
    match classify(error) {
        NetError::Tls if !clock_synced => Reason::ClockNotSet,
        NetError::Timeout => Reason::Timeout,
        NetError::Unreachable => Reason::Unreachable,
        NetError::Tls => Reason::Tls,
        NetError::Protocol => Reason::Protocol,
    }
}

/// The body stream of a provider response wraps the request error in an
/// [`std::io::Error`]; classify the inner error when it is still there.
fn body_error_reason(error: &std::io::Error, clock_synced: bool) -> Reason {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
    {
        Some(inner) => network_reason(inner, clock_synced),
        None => Reason::Protocol,
    }
}

/// One owner sentence for a failed check. It names the cause and the fix, and
/// never a URL, key or body.
pub fn refusal(error: &ProviderError) -> String {
    match error.reason {
        Reason::Http(401) | Reason::Http(403) => "The key is wrong. Enter it again.".to_owned(),
        Reason::Http(402) => "No credit is left on this account.".to_owned(),
        Reason::Http(404) if error.preset == PresetId::OpenClaw => {
            "The address is wrong. OpenClaw answers chat requests only after its chat endpoint is turned on; check the address, then turn it on.".to_owned()
        }
        Reason::Http(404) => "The address is wrong. Check the connection's address.".to_owned(),
        Reason::Http(status) => format!(
            "The service answered {status}. Check the key, the address and the account."
        ),
        Reason::Timeout => {
            "The service didn't answer in time. Check the address and that the server is running."
                .to_owned()
        }
        Reason::Unreachable => {
            "The service can't be reached. Check the address and that the server is running."
                .to_owned()
        }
        Reason::Tls => {
            "The secure connection failed. Check the address and the server's certificate."
                .to_owned()
        }
        Reason::ClockNotSet => {
            "The clock isn't set yet. Connect the satellite to the network, then try again."
                .to_owned()
        }
        Reason::Protocol => {
            "The service didn't answer the way this check expects. Check the address and the model."
                .to_owned()
        }
        Reason::TooLarge => {
            "The service sent too much data to check. Check the address.".to_owned()
        }
        Reason::Internal => "The check couldn't run. Try again.".to_owned(),
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use pinna_http::{
        ClientKind, Secret, client,
        reqwest::{Method, StatusCode},
    };

    use super::*;
    use crate::testing::{FakeResponse, FakeServer};

    fn setup(server: &FakeServer, slot: Slot, preset: PresetId, model: &str) -> CheckSetup {
        CheckSetup {
            slot,
            http: client(ClientKind::Provider {
                read: Some(Duration::from_secs(5)),
            }),
            preset,
            base_url: server.url(""),
            model: model.into(),
            voice: Some("nova".into()),
            auth: Auth::Bearer(Secret::new("sk-test")),
            clock_synced: || true,
        }
    }

    const MODELS: &str = "/models";

    #[tokio::test]
    async fn a_wrong_key_names_the_key() {
        let server = FakeServer::start().await;
        server.script_status(Method::GET, MODELS, StatusCode::UNAUTHORIZED);
        let error = check(&setup(&server, Slot::Chat, PresetId::OpenAi, "gpt-4o"))
            .await
            .unwrap_err();
        assert_eq!(error, "The key is wrong. Enter it again.");
    }

    #[tokio::test]
    async fn a_forbidden_key_is_the_same_sentence() {
        let server = FakeServer::start().await;
        server.script_status(Method::GET, MODELS, StatusCode::FORBIDDEN);
        let error = check(&setup(&server, Slot::Chat, PresetId::OpenAi, "gpt-4o"))
            .await
            .unwrap_err();
        assert_eq!(error, "The key is wrong. Enter it again.");
    }

    #[tokio::test]
    async fn no_credit_has_its_own_sentence() {
        let server = FakeServer::start().await;
        server.script_status(Method::GET, MODELS, StatusCode::PAYMENT_REQUIRED);
        let error = check(&setup(&server, Slot::Chat, PresetId::OpenAi, "gpt-4o"))
            .await
            .unwrap_err();
        assert_eq!(error, "No credit is left on this account.");
    }

    #[tokio::test]
    async fn a_wrong_address_has_its_own_sentence() {
        let server = FakeServer::start().await;
        server.script_status(Method::GET, MODELS, StatusCode::NOT_FOUND);
        let error = check(&setup(&server, Slot::Chat, PresetId::OpenAi, "gpt-4o"))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "The address is wrong. Check the connection's address."
        );
    }

    #[tokio::test]
    async fn openclaw_404_explains_its_endpoint() {
        let server = FakeServer::start().await;
        server.script_status(Method::GET, MODELS, StatusCode::NOT_FOUND);
        let error = check(&setup(
            &server,
            Slot::Chat,
            PresetId::OpenClaw,
            "openclaw/default",
        ))
        .await
        .unwrap_err();
        assert!(error.contains("OpenClaw"), "{error}");
        assert!(error.contains("endpoint"), "{error}");
    }

    #[tokio::test]
    async fn a_slow_service_is_a_timeout() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            MODELS,
            FakeResponse::chunked(
                StatusCode::OK,
                ["{\"data\":[", "{\"id\":\"gpt-4o\"}]}"],
                Duration::from_millis(200),
            ),
        );
        let mut slow = setup(&server, Slot::Chat, PresetId::OpenAi, "gpt-4o");
        slow.http = client(ClientKind::Provider {
            read: Some(Duration::from_millis(5)),
        });
        let error = check(&slow).await.unwrap_err();
        assert_eq!(
            error,
            "The service didn't answer in time. Check the address and that the server is running."
        );
    }

    #[tokio::test]
    async fn a_hand_off_preset_lists_models_without_a_chat_post() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            MODELS,
            FakeResponse::new(StatusCode::OK, "{\"data\":[{\"id\":\"hermes-3\"}]}"),
        );
        check(&setup(&server, Slot::Chat, PresetId::Hermes, "hermes-3"))
            .await
            .unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, Method::GET);
        assert_eq!(requests[0].path, MODELS);
    }

    #[tokio::test]
    async fn the_brain_check_confirms_a_model_in_an_authoritative_list() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            MODELS,
            FakeResponse::new(StatusCode::OK, "{\"data\":[{\"id\":\"gpt-4o\"}]}"),
        );
        check(&setup(&server, Slot::Chat, PresetId::OpenAi, "gpt-4o"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_brain_check_refuses_a_model_the_list_does_not_offer() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            MODELS,
            FakeResponse::new(StatusCode::OK, "{\"data\":[{\"id\":\"other\"}]}"),
        );
        let error = check(&setup(&server, Slot::Chat, PresetId::OpenAi, "gpt-4o"))
            .await
            .unwrap_err();
        assert_eq!(error, "The model gpt-4o isn't offered by this service.");
    }

    #[tokio::test]
    async fn a_gemini_name_is_stripped_of_its_prefix() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1beta/models",
            FakeResponse::new(
                StatusCode::OK,
                "{\"models\":[{\"name\":\"models/gemini-2.0-flash\"}]}",
            ),
        );
        check(&setup(
            &server,
            Slot::Chat,
            PresetId::Gemini,
            "gemini-2.0-flash",
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_model_list_past_the_cap_is_not_confirmed() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            MODELS,
            FakeResponse::new(StatusCode::OK, vec![b'x'; MODELS_CAP + 1]),
        );
        // The status alone passes, so a model that isn't in the oversized body
        // is still accepted.
        check(&setup(
            &server,
            Slot::Chat,
            PresetId::OpenAi,
            "not-in-the-body",
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_stt_check_falls_back_to_silence_without_a_model_list() {
        let server = FakeServer::start().await;
        server.script_status(Method::GET, MODELS, StatusCode::NOT_FOUND);
        server.script(
            Method::POST,
            "/audio/transcriptions",
            FakeResponse::new(StatusCode::OK, "{\"text\":\"\"}"),
        );
        check(&setup(&server, Slot::Stt, PresetId::OpenAi, "whisper-1"))
            .await
            .unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, Method::GET);
        assert_eq!(requests[1].method, Method::POST);
        assert_eq!(requests[1].path, "/audio/transcriptions");
        let body = String::from_utf8_lossy(&requests[1].body);
        assert!(body.contains("filename=\"audio.wav\""), "{body}");
        assert!(body.contains("RIFF"), "{body}");
    }

    #[test]
    fn the_silence_is_half_a_second() {
        assert_eq!(silence_wav().len(), 44 + SILENCE_BYTES);
    }

    #[tokio::test]
    async fn the_tts_check_requires_audio_bytes() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/audio/speech",
            FakeResponse::new(StatusCode::OK, "audio"),
        );
        check(&setup(&server, Slot::Tts, PresetId::OpenAi, "tts-1"))
            .await
            .unwrap();
        let requests = server.requests();
        assert_eq!(requests[0].path, "/audio/speech");
        let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(sent["model"], "tts-1");
        assert_eq!(sent["voice"], "nova");
        assert_eq!(sent["input"], "OK.");
    }

    #[tokio::test]
    async fn empty_audio_fails_the_tts_check() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/audio/speech",
            FakeResponse::empty(StatusCode::OK),
        );
        let error = check(&setup(&server, Slot::Tts, PresetId::OpenAi, "tts-1"))
            .await
            .unwrap_err();
        assert_eq!(error, "The text-to-speech service sent no audio.");
    }

    #[test]
    fn every_reason_has_a_sentence() {
        for reason in [
            Reason::Http(401),
            Reason::Http(402),
            Reason::Http(403),
            Reason::Http(404),
            Reason::Http(429),
            Reason::Timeout,
            Reason::Unreachable,
            Reason::Tls,
            Reason::ClockNotSet,
            Reason::Protocol,
            Reason::TooLarge,
            Reason::Internal,
        ] {
            let error = ProviderError::new(Slot::Chat, PresetId::OpenAi, reason);
            let sentence = refusal(&error);
            assert!(!sentence.is_empty(), "{reason:?}");
            assert!(!sentence.contains("sk-"), "{sentence}");
            assert!(!sentence.contains("://"), "{sentence}");
        }
    }
}
