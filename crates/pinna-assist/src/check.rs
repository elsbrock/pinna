//! Save-time checks for a provider connection (ADR 0017 §"The pipeline").
//!
//! A check is the cheapest call that proves a connection works: list the
//! models, transcribe half a second of silence, or synthesise one word. No
//! check ever sends a prompt, and a hand-off brain is only asked for its model
//! list. A failure becomes one owner sentence through [`refusal`]; it never
//! carries a URL, a key or a response body.

use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;

use crate::{
    audio,
    error::{ProviderError, Reason, Slot},
    http::{self, Endpoint},
    presets::{ChatProtocol, PresetId, SpeechProtocol, preset},
    reply::AudioBody,
    stt, tts,
};

/// A check gives up after this long.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The model-list body may be this large. Beyond it the status alone passes and
/// the model isn't confirmed.
const MODELS_CAP: usize = 2 * 1024 * 1024;

/// Half a second of 16 kHz mono signed 16-bit silence, the smallest utterance
/// that proves a speech-to-text server without a model list.
const SILENCE_BYTES: usize = 16_000 * 500 / 1000 * 2;

/// Everything one save-time check needs.
pub struct CheckSetup {
    pub slot: Slot,
    /// The connection, with the key in its protocol's header
    /// ([`crate::presets::auth`]).
    pub endpoint: Endpoint,
    /// What the slot names: the speech-to-text or brain model, or the
    /// text-to-speech voice.
    pub model: String,
    /// The brain hands requests off (the preset's delegation after the
    /// owner's Advanced override): its check never sends a prompt or names a
    /// model.
    pub hand_off: bool,
}

/// Check one connection, returning an owner sentence when it fails.
pub async fn check(setup: &CheckSetup) -> Result<(), String> {
    match tokio::time::timeout(CHECK_TIMEOUT, run_check(setup)).await {
        Ok(result) => result,
        Err(_) => Err(refused(&setup.endpoint.error(setup.slot, Reason::Timeout))),
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
    let endpoint = &setup.endpoint;
    let protocol = preset(endpoint.preset).chat_protocol;
    let mut request = endpoint.http.get(endpoint.url(protocol.models_path()));
    if let Some((name, value)) = protocol.version_header() {
        request = request.header(name, value);
    }
    let response = endpoint.send(request, slot).await?;
    match endpoint.read_capped(response, MODELS_CAP, slot).await {
        Ok(body) => Ok(Some(body)),
        Err(error) if error.reason == Reason::TooLarge => Ok(None),
        Err(error) => Err(error),
    }
}

async fn stt_check(setup: &CheckSetup) -> Result<(), String> {
    // ElevenLabs lists its voice models, not Scribe: only a transcription
    // proves the model, and a wrong one is refused with the ones it has.
    if preset(setup.endpoint.preset).speech_protocol == SpeechProtocol::ElevenLabs {
        return transcribe_silence(setup).await;
    }
    match models(setup, Slot::Stt).await {
        Ok(_) => Ok(()),
        // Some self-hosted servers have no model list. A short silence is the
        // smallest transcription that still proves the key and the address.
        Err(error) if error.reason == Reason::Http(404) => transcribe_silence(setup).await,
        Err(error) => Err(refused(&error)),
    }
}

async fn transcribe_silence(setup: &CheckSetup) -> Result<(), String> {
    stt::adapter(
        preset(setup.endpoint.preset).speech_protocol,
        setup.endpoint.clone(),
        setup.model.clone(),
    )
    .transcribe(silence_wav(), None)
    .await
    .map(|_| ())
    .map_err(|error| refused(&error))
}

async fn chat_check(setup: &CheckSetup) -> Result<(), String> {
    let body = models(setup, Slot::Chat)
        .await
        .map_err(|error| refused(&error))?;
    // A hand-off brain owns its own model names; a successful list is enough.
    if setup.hand_off {
        return Ok(());
    }
    let Some(body) = body else {
        return Ok(());
    };
    // A preset that names its own model is checked for that one.
    let model = preset(setup.endpoint.preset).model.unwrap_or(&setup.model);
    if let Some(ids) = offered_models(&body)
        && !ids.iter().any(|id| names_model(id, model))
    {
        return Err(format!("The model {model} isn't offered by this service."));
    }
    Ok(())
}

async fn tts_check(setup: &CheckSetup) -> Result<(), String> {
    let audio = tts::adapter(
        preset(setup.endpoint.preset).speech_protocol,
        setup.endpoint.clone(),
        setup.model.clone(),
        None,
    )
    .speak("OK.")
    .await
    .map_err(|error| refused(&error))?;
    let AudioBody::Stream(mut stream) = audio.body else {
        return Ok(());
    };
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) if !bytes.is_empty() => return Ok(()),
            Ok(_) => continue,
            Err(error) => {
                let reason = http::body_reason(&error, (setup.endpoint.clock_synced)());
                return Err(refused(&setup.endpoint.error(Slot::Tts, reason)));
            }
        }
    }
    Err("The text-to-speech service sent no audio.".to_owned())
}

fn silence_wav() -> Bytes {
    audio::wav(&[Bytes::from(vec![0u8; SILENCE_BYTES])])
}

#[derive(serde::Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
    #[serde(default)]
    models: Vec<ModelEntry>,
    /// Anthropic: more pages follow.
    #[serde(default)]
    has_more: bool,
    /// Gemini: more pages follow.
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

/// The ids in a model list, or `None` when the body isn't an authoritative
/// list: it didn't parse, named no models, or is one page of several.
fn offered_models(body: &Bytes) -> Option<Vec<String>> {
    let list: ModelList = serde_json::from_slice(body).ok()?;
    if list.has_more || list.next_page_token.is_some_and(|t| !t.is_empty()) {
        return None;
    }
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

/// Whether a listed id is the model the owner entered. Anthropic lists
/// dated ids (`claude-haiku-4-5-20251001`) for the alias owners type
/// (`claude-haiku-4-5`); a bare prefix would also match `claude-opus-4-8`
/// for `claude-opus-4`, so only an 8-digit date suffix counts. Ollama lists
/// `llama3.2:latest` for the `llama3.2` it answers to.
fn names_model(id: &str, model: &str) -> bool {
    id == model
        || id.strip_suffix(":latest") == Some(model)
        || id
            .strip_prefix(model)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
}

/// One owner sentence for a request that failed: a check's, except that a
/// brain that ran out of time during a request is more likely slow than down,
/// and its limit is the owner's to raise.
pub fn request_failure(error: &ProviderError) -> String {
    match (error.slot, error.reason) {
        (Slot::Chat, Reason::Timeout) => "The brain didn't answer in time. If the server is \
            running, the model may need more Time to answer, under Services, Brain, Advanced."
            .to_owned(),
        // Some reasoning models take tools only at a set effort: gpt-6-luna
        // over Chat Completions only with `none`.
        (Slot::Chat, Reason::Http(400))
            if preset(error.preset).chat_protocol == ChatProtocol::OpenAi =>
        {
            "The service refused the request (400). Check the model's name. A reasoning model \
            may need Thinking set, such as None, under Services, Brain, Advanced."
                .to_owned()
        }
        _ => refusal(error),
    }
}

/// The owner's sentence for a failed check, with the failure and what the
/// service said in the log. A refused request (400, 422) adds the service's
/// own words, which name what it didn't take, such as a model it doesn't
/// have; they are masked and capped like the log's.
fn refused(error: &ProviderError) -> String {
    tracing::info!(
        "the {} check failed: {error}{}",
        error.slot.label(),
        error.said_suffix()
    );
    match (error.reason, &error.said) {
        (Reason::Http(400 | 422), Some(said)) => {
            format!("{} The service said: “{said}”", refusal(error))
        }
        _ => refusal(error),
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
        // An address the owner typed in full: the usual mistake is the path.
        Reason::Http(404) if preset(error.preset).base_url.is_empty() => {
            "Nothing answered at this address. An OpenAI-compatible server's address usually ends in /v1."
                .to_owned()
        }
        Reason::Http(404) if error.slot == Slot::Tts && error.preset == PresetId::ElevenLabs => {
            "ElevenLabs has no voice with this ID. Enter the voice's ID from your voice library, \
             not its name."
                .to_owned()
        }
        Reason::Http(404) => "The address is wrong. Check the connection's address.".to_owned(),
        // Provider requests don't follow redirects, so a key never reaches
        // another host; a moved service needs its new address.
        Reason::Http(300..=399) => {
            "The service moved to another address. Enter its new address.".to_owned()
        }
        // The service's own fault: no key or address is wrong.
        Reason::Http(status @ 500..=599) => format!(
            "The service had an error ({status}). Try again later, or check the server's log."
        ),
        // A key that is wrong is 401 or 403; a refused request is usually a
        // name the service doesn't know.
        Reason::Http(status @ (400 | 422)) => format!(
            "The service refused the request ({status}). Check the model's name."
        ),
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

#[cfg(test)]
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
            endpoint: Endpoint::new(
                client(ClientKind::Provider {
                    read: Some(Duration::from_secs(5)),
                }),
                &server.url(""),
                crate::presets::auth(preset, slot, Some(Secret::new("sk-test"))),
                preset,
                || true,
            ),
            model: model.into(),
            hand_off: crate::presets::preset(preset).behaviour.delegation
                == crate::presets::Delegation::HandOff,
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
        slow.endpoint.http = client(ClientKind::Provider {
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
    async fn an_anthropic_model_list_names_the_api_version() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1/models?limit=1000",
            FakeResponse::new(StatusCode::OK, "{\"data\":[{\"id\":\"claude-sonnet\"}]}"),
        );
        check(&setup(
            &server,
            Slot::Chat,
            PresetId::Anthropic,
            "claude-sonnet",
        ))
        .await
        .unwrap();
        let requests = server.requests();
        assert_eq!(requests[0].header("anthropic-version"), Some("2023-06-01"));
    }

    #[tokio::test]
    async fn a_gemini_name_is_stripped_of_its_prefix() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1beta/models?pageSize=1000",
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

    #[tokio::test]
    async fn elevenlabs_speech_to_text_is_checked_by_a_transcription_that_names_a_wrong_model() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/speech-to-text",
            FakeResponse::new(
                StatusCode::BAD_REQUEST,
                r#"{"detail": {"status": "invalid_model_id", "message": "'whisper-1' is not a valid model_id. Available models: 'scribe_v1', 'scribe_v2'"}}"#,
            ),
        );
        let error = check(&setup(&server, Slot::Stt, PresetId::ElevenLabs, "whisper-1"))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "The service refused the request (400). Check the model's name. The service said: \
             “'whisper-1' is not a valid model_id. Available models: 'scribe_v1', 'scribe_v2'”"
        );
        // No model list first: ElevenLabs' doesn't have Scribe.
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/v1/speech-to-text");
    }

    #[tokio::test]
    async fn an_elevenlabs_voice_name_is_not_an_address_problem() {
        let server = FakeServer::start().await;
        server.script_status(
            Method::POST,
            "/v1/text-to-speech/Rachel/stream?output_format=mp3_44100_64",
            StatusCode::NOT_FOUND,
        );
        let error = check(&setup(&server, Slot::Tts, PresetId::ElevenLabs, "Rachel"))
            .await
            .unwrap_err();
        assert!(error.starts_with("ElevenLabs has no voice with this ID."), "{error}");
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
        check(&setup(&server, Slot::Tts, PresetId::OpenAi, "nova"))
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
        let error = check(&setup(&server, Slot::Tts, PresetId::OpenAi, "nova"))
            .await
            .unwrap_err();
        assert_eq!(error, "The text-to-speech service sent no audio.");
    }

    #[test]
    fn a_server_error_and_a_slow_brain_get_their_own_advice() {
        let down = ProviderError::new(Slot::Chat, PresetId::Ollama, Reason::Http(500));
        assert!(refusal(&down).starts_with("The service had an error (500)."));
        let slow = ProviderError::new(Slot::Chat, PresetId::Ollama, Reason::Timeout);
        assert!(request_failure(&slow).contains("Time to answer"));
        assert!(!refusal(&slow).contains("Time to answer"));
        let stt = ProviderError::new(Slot::Stt, PresetId::OpenAi, Reason::Timeout);
        assert_eq!(request_failure(&stt), refusal(&stt));
    }

    #[test]
    fn a_refused_brain_request_names_the_thinking_setting_where_it_applies() {
        let openai = ProviderError::new(Slot::Chat, PresetId::OpenAi, Reason::Http(400));
        assert!(request_failure(&openai).contains("Thinking"));
        let anthropic = ProviderError::new(Slot::Chat, PresetId::Anthropic, Reason::Http(400));
        assert_eq!(request_failure(&anthropic), refusal(&anthropic));
        // The check lists models and never sends the setting.
        assert!(!refusal(&openai).contains("Thinking"));
    }

    #[test]
    fn a_missing_path_is_named_for_an_address_typed_in_full() {
        let custom = ProviderError::new(Slot::Chat, PresetId::Custom, Reason::Http(404));
        assert!(refusal(&custom).contains("/v1"));
        let hosted = ProviderError::new(Slot::Chat, PresetId::OpenAi, Reason::Http(404));
        assert!(!refusal(&hosted).contains("/v1"));
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

    #[tokio::test]
    async fn one_page_of_several_doesnt_refuse_a_model() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1/models?limit=1000",
            FakeResponse::new(
                StatusCode::OK,
                "{\"data\":[{\"id\":\"claude-other\"}],\"has_more\":true}",
            ),
        );
        check(&setup(
            &server,
            Slot::Chat,
            PresetId::Anthropic,
            "claude-sonnet",
        ))
        .await
        .unwrap();
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1beta/models?pageSize=1000",
            FakeResponse::new(
                StatusCode::OK,
                "{\"models\":[{\"name\":\"models/other\"}],\"nextPageToken\":\"x\"}",
            ),
        );
        check(&setup(&server, Slot::Chat, PresetId::Gemini, "gemini-pro"))
            .await
            .unwrap();
    }

    #[test]
    fn an_alias_matches_its_dated_id_only() {
        assert!(names_model("claude-haiku-4-5-20251001", "claude-haiku-4-5"));
        assert!(names_model("gpt-4o", "gpt-4o"));
        assert!(names_model("llama3.2:latest", "llama3.2"));
        assert!(!names_model("llama3.2:1b", "llama3.2"));
        assert!(!names_model("claude-opus-4-8", "claude-opus-4"));
        assert!(!names_model("claude-opus-4-latest", "claude-opus-4"));
    }
}
