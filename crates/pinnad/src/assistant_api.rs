//! Standalone mode's setup API: ConnectRPC `pinna.v1.AssistantService`
//! (ADR 0017), served with the dashboard beside `DeviceService`.
//!
//! The handlers only translate: [`Service`] validates, checks, stores and
//! publishes every edit. Keys and MCP server addresses come in and never go
//! out; the view the service renders has neither.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot, watch};

use buffa::{EnumValue, Inline, MessageField};
use connectrpc::{ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult};
use pinna_api::{
    Acknowledgment, AddGroupMemberRequest, AddGroupMemberResponse, AskTextRequest, AskTextResponse,
    Assistant, AssistantService, AssistantSlot, BrainOverride, ChoiceKind, DeleteConnectionRequest,
    DeleteConnectionResponse, DeleteMcpServerRequest, DeleteMcpServerResponse, DeviceTool,
    GetAssistantRequest, GetAssistantResponse, GetSatelliteGroupRequest, GetSatelliteGroupResponse,
    ListChoicesRequest, ListChoicesResponse, McpAgent, PlaySampleRequest, PlaySampleResponse,
    ProviderConnection, RemoveGroupMemberRequest, RemoveGroupMemberResponse,
    SatelliteGroupCandidate, SatelliteGroupMember, SatelliteGroupView, SetConnectionRequest,
    SetConnectionResponse, SetMcpServerRequest, SetMcpServerResponse, SetSatelliteGroupRequest,
    SetSatelliteGroupResponse, SetSlotRequest, SetSlotResponse, SlotChoice, UpdateAssistantRequest,
    UpdateAssistantResponse,
};
use pinna_assist::choices::Wanted;
use pinna_assist::config::{Ack, Station};
use pinna_assist::presets::{
    self, ChatOverride, ChatProtocol, Delegation, HistoryPolicy, PresetId, Reasoning,
};
use pinna_assist::prompt::VOICE_RULES;
use pinna_http::Secret;

use crate::checks;
use crate::group::Group;
use crate::group::view::{CandidateState, GroupView};
use crate::satellites::Seen;
use crate::standalone::service::{
    AssistantPatch, AssistantView, ConnectionEdit, KeyEdit, McpEdit, Refusal, Service, SlotEdit,
    SlotKind, SlotView,
};
use crate::tools;
use crate::voice::{ApiCommand, Input};

pub struct AssistantApi {
    service: Arc<Service>,
    /// The Brain's input queue, for what plays or runs on the satellite.
    inputs: mpsc::Sender<Input>,
    /// What the background search last found (satellites::run), for the
    /// satellite group's candidates.
    satellites: watch::Receiver<Seen>,
    /// This satellite's MAC, to find its own entry in the group.
    own_mac: String,
    /// The satellite group's calls to the other members, and what is known
    /// of them.
    group: Option<Arc<Group>>,
}

impl AssistantApi {
    pub fn new(service: Arc<Service>, inputs: mpsc::Sender<Input>) -> Self {
        Self {
            service,
            inputs,
            satellites: watch::channel(Seen::default()).1,
            own_mac: String::new(),
            group: None,
        }
    }

    /// The search's results and this satellite's MAC, for the satellite
    /// group (ADR 0026).
    pub fn with_satellites(mut self, satellites: watch::Receiver<Seen>, own_mac: String) -> Self {
        self.satellites = satellites;
        self.own_mac = own_mac;
        self
    }

    /// Calls to the other members of the satellite group: Add, Remove and
    /// leaving tell them, and the view shows what they last said (ADR 0026).
    pub fn with_group(mut self, group: Arc<Group>) -> Self {
        self.group = Some(group);
        self
    }

    fn group_view(&self) -> MessageField<SatelliteGroupView, Inline<SatelliteGroupView>> {
        let seen = self.satellites.borrow().clone();
        let group = self.group.as_deref();
        let view = self.service.group_view(&self.own_mac, &seen, |mac| {
            group.is_some_and(|group| group.answered_outside(mac))
        });
        MessageField::some(render_group(view, group))
    }

    fn group(&self) -> Result<&Arc<Group>, ConnectError> {
        self.group
            .as_ref()
            .ok_or_else(|| ConnectError::unavailable("The satellite group isn't running."))
    }

    /// Hand a command to the Brain and wait for its answer. A refusal is a
    /// sentence for the owner.
    async fn brain<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, String>>) -> ApiCommand,
    ) -> Result<T, ConnectError> {
        let (tx, rx) = oneshot::channel();
        self.inputs
            .send(Input::Api(make(tx)))
            .await
            .map_err(|_| ConnectError::unavailable("pinnad is shutting down"))?;
        rx.await
            .map_err(|_| ConnectError::unavailable("pinnad is shutting down"))?
            .map_err(ConnectError::failed_precondition)
    }
}

/// The routes, for the dashboard listener.
pub fn router(api: Arc<AssistantApi>) -> Router {
    Router::new().add_service(api)
}

pub(crate) fn refused(r: Refusal) -> ConnectError {
    match r {
        Refusal::Invalid(s) => ConnectError::invalid_argument(s),
        Refusal::Precondition(s) => ConnectError::failed_precondition(s),
        Refusal::Conflict(s) => ConnectError::aborted(s),
    }
}

fn render(v: AssistantView) -> Assistant {
    Assistant {
        ready: v.ready,
        connections: v
            .connections
            .into_iter()
            .map(|c| ProviderConnection {
                id: c.id,
                preset: c.preset.label().into(),
                base_url: c.base_url,
                revision: c.revision,
                key_set: c.key_set,
                key_last4: c.key_last4.unwrap_or_default(),
                ..Default::default()
            })
            .collect(),
        stt: slot_choice(v.stt),
        brain: slot_choice(v.chat),
        tts: slot_choice(v.tts),
        brain_override: MessageField::some(brain_override(&v.chat_override)),
        language: v.language.unwrap_or_default(),
        place: v.place.map_or_else(MessageField::none, |p| {
            MessageField::some(pinna_api::Place {
                name: p.name,
                latitude: p.lat,
                longitude: p.lon,
                time_zone: p.time_zone.unwrap_or_default(),
                ..Default::default()
            })
        }),
        prompt: v.prompt,
        prompt_revision: v.prompt_revision,
        follow_ups: v.follow_ups,
        ack: MessageField::some(Acknowledgment {
            enabled: v.ack.enabled,
            phrase: v.ack.phrase,
            ..Default::default()
        }),
        tools: v.tools.into_iter().collect(),
        stations: v
            .stations
            .into_iter()
            .map(|s| pinna_api::Station {
                name: s.name,
                url: s.url,
                ..Default::default()
            })
            .collect(),
        mcp_servers: v
            .mcp_servers
            .into_iter()
            .map(|s| pinna_api::McpServer {
                id: s.id,
                name: s.name,
                host: s.host,
                enabled: s.enabled,
                revision: s.revision,
                tools: s
                    .listing
                    .as_ref()
                    .and_then(|l| l.as_ref().ok())
                    .map_or(0, |tools| u32::try_from(tools.len()).unwrap_or(u32::MAX)),
                listed: s
                    .listing
                    .as_ref()
                    .and_then(|l| l.as_ref().ok())
                    .map(|tools| {
                        tools
                            .iter()
                            .map(|t| pinna_api::McpServerTool {
                                name: t.name.clone(),
                                description: t.description.clone(),
                                ..Default::default()
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                problem: s.listing.and_then(Result::err).unwrap_or_default(),
                ..Default::default()
            })
            .collect(),
        agents: v
            .agents
            .into_iter()
            .map(|a| McpAgent {
                name: a.name,
                address: a.peer.to_string(),
                last_seen_unix_ms: unix_ms(a.last_seen),
                ..Default::default()
            })
            .collect(),
        presets: presets::presets()
            .iter()
            .map(|p| pinna_api::Preset {
                id: p.id.label().into(),
                base_url: p.base_url.into(),
                stt: p.slots.stt,
                brain: p.slots.chat,
                tts: p.slots.tts,
                model: p.model.unwrap_or_default().into(),
                hand_off: p.behaviour.delegation == Delegation::HandOff,
                thinking: p.chat_protocol == ChatProtocol::OpenAi,
                tts_model: if p.slots.tts {
                    p.speech_protocol.tts_model().into()
                } else {
                    String::new()
                },
                ..Default::default()
            })
            .collect(),
        ack_spoken: v.ack_spoken,
        voice_rules: VOICE_RULES.into(),
        device_tools: tools::catalogue()
            .map(|t| DeviceTool {
                name: t.name.into(),
                description: t.description,
                brain: t.brain,
                mcp: t.mcp,
                off_by_default: t.off_by_default,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// The group as the dashboard shows it, with each other member's mode and
/// when it was last seen from `group`. The key isn't part of the view.
fn render_group(v: GroupView, group: Option<&Group>) -> SatelliteGroupView {
    use pinna_api::satellite_group_candidate::State;
    SatelliteGroupView {
        on: v.on,
        in_group: v.in_group,
        control: v.control,
        members: v
            .members
            .into_iter()
            .map(|m| {
                let peer = group
                    .filter(|_| !m.this_satellite)
                    .and_then(|g| g.peer(&m.mac))
                    .unwrap_or_default();
                SatelliteGroupMember {
                    mode: peer.mode.map(|mode| crate::api::api_mode(mode).into()),
                    has_password: peer.has_password,
                    last_seen_unix_ms: peer.last_seen.map_or(0, unix_ms),
                    mac: m.mac,
                    name: m.name,
                    this_satellite: m.this_satellite,
                    ..Default::default()
                }
            })
            .collect(),
        candidates: v
            .candidates
            .into_iter()
            .map(|c| SatelliteGroupCandidate {
                satellite: MessageField::some(crate::api::satellite(c.found)),
                state: EnumValue::Known(match c.state {
                    CandidateState::Addable => State::STATE_ADDABLE,
                    CandidateState::InAnotherGroup => State::STATE_IN_ANOTHER_GROUP,
                    CandidateState::OffOrOlder => State::STATE_OFF_OR_OLDER,
                    CandidateState::LeftTheGroup => State::STATE_LEFT_THE_GROUP,
                }),
                addable: c.addable,
                default_name: c.default_name,
                name_refusal: c.name_refusal.unwrap_or_default(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn assistant(v: AssistantView) -> MessageField<Assistant, Inline<Assistant>> {
    MessageField::some(render(v))
}

fn slot_choice(slot: Option<SlotView>) -> MessageField<SlotChoice, Inline<SlotChoice>> {
    slot.map_or_else(MessageField::none, |s| {
        MessageField::some(SlotChoice {
            connection: s.connection,
            model: s.model,
            ready: s.ready,
            speech_model: s.speech_model,
            ..Default::default()
        })
    })
}

fn brain_override(o: &ChatOverride) -> BrainOverride {
    use pinna_api::{Delegation as D, History as H, Reasoning as R};
    let (history, history_limit) = match o.history {
        None => (H::HISTORY_UNSPECIFIED, 0),
        Some(HistoryPolicy::Turns(n)) => (H::HISTORY_TURNS, n),
        Some(HistoryPolicy::Bytes(n)) => (H::HISTORY_BYTES, n),
        Some(HistoryPolicy::NewTurnOnly) => (H::HISTORY_NEW_TURN_ONLY, 0),
    };
    BrainOverride {
        delegation: EnumValue::Known(match o.delegation {
            None => D::DELEGATION_UNSPECIFIED,
            Some(Delegation::ToolLoop) => D::DELEGATION_TOOL_LOOP,
            Some(Delegation::HandOff) => D::DELEGATION_HAND_OFF,
        }),
        history: EnumValue::Known(history),
        history_limit: u32::try_from(history_limit).unwrap_or(u32::MAX),
        first_text_seconds: o.first_text_s.map(|s| u32::try_from(s).unwrap_or(u32::MAX)),
        reasoning: EnumValue::Known(match o.reasoning {
            None => R::REASONING_UNSPECIFIED,
            Some(Reasoning::None) => R::REASONING_NONE,
            Some(Reasoning::Low) => R::REASONING_LOW,
            Some(Reasoning::Medium) => R::REASONING_MEDIUM,
            Some(Reasoning::High) => R::REASONING_HIGH,
        }),
        ..Default::default()
    }
}

fn chat_override(o: &BrainOverride) -> Result<ChatOverride, ConnectError> {
    use pinna_api::{Delegation as D, History as H, Reasoning as R};
    let delegation = match o.delegation.as_known() {
        Some(D::DELEGATION_UNSPECIFIED) => None,
        Some(D::DELEGATION_TOOL_LOOP) => Some(Delegation::ToolLoop),
        Some(D::DELEGATION_HAND_OFF) => Some(Delegation::HandOff),
        None => return Err(ConnectError::invalid_argument("Choose who runs the tools.")),
    };
    let limit = o.history_limit as usize;
    let history = match o.history.as_known() {
        Some(H::HISTORY_UNSPECIFIED) => None,
        Some(H::HISTORY_TURNS) => Some(HistoryPolicy::Turns(limit)),
        Some(H::HISTORY_BYTES) => Some(HistoryPolicy::Bytes(limit)),
        Some(H::HISTORY_NEW_TURN_ONLY) => Some(HistoryPolicy::NewTurnOnly),
        None => {
            return Err(ConnectError::invalid_argument(
                "Choose how much of the conversation the brain gets.",
            ));
        }
    };
    let reasoning = match o.reasoning.as_known() {
        Some(R::REASONING_UNSPECIFIED) => None,
        Some(R::REASONING_NONE) => Some(Reasoning::None),
        Some(R::REASONING_LOW) => Some(Reasoning::Low),
        Some(R::REASONING_MEDIUM) => Some(Reasoning::Medium),
        Some(R::REASONING_HIGH) => Some(Reasoning::High),
        None => {
            return Err(ConnectError::invalid_argument(
                "Choose how long the brain thinks.",
            ));
        }
    };
    Ok(ChatOverride {
        delegation,
        history,
        first_text_s: o.first_text_seconds.map(u64::from),
        reasoning,
    })
}

fn preset_id(id: &str) -> Result<PresetId, ConnectError> {
    if id.is_empty() {
        return Err(ConnectError::invalid_argument("Choose a preset."));
    }
    presets::presets()
        .iter()
        .map(|p| p.id)
        .find(|p| p.label() == id)
        .ok_or_else(|| ConnectError::invalid_argument(format!("There is no preset {id:?}.")))
}

fn slot_kind(slot: &EnumValue<AssistantSlot>) -> Result<SlotKind, ConnectError> {
    match slot.as_known() {
        Some(AssistantSlot::ASSISTANT_SLOT_STT) => Ok(SlotKind::Stt),
        Some(AssistantSlot::ASSISTANT_SLOT_BRAIN) => Ok(SlotKind::Chat),
        Some(AssistantSlot::ASSISTANT_SLOT_TTS) => Ok(SlotKind::Tts),
        _ => Err(ConnectError::invalid_argument(
            "Choose speech-to-text, the brain or text-to-speech.",
        )),
    }
}

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// An empty id asks for a new entry.
fn existing(id: &str) -> Option<String> {
    (!id.is_empty()).then(|| id.to_owned())
}

#[allow(refining_impl_trait)]
impl AssistantService for AssistantApi {
    async fn get_assistant(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, GetAssistantRequest>,
    ) -> ServiceResult<GetAssistantResponse> {
        Response::ok(GetAssistantResponse {
            assistant: assistant(self.service.view()),
            ..Default::default()
        })
    }

    async fn set_connection(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetConnectionRequest>,
    ) -> ServiceResult<SetConnectionResponse> {
        let key = match (request.key, request.clear_key) {
            (Some(_), true) => {
                return Err(ConnectError::invalid_argument(
                    "Enter a key or remove it, not both.",
                ));
            }
            (Some(key), false) => KeyEdit::Set(Secret::new(key)),
            (None, true) => KeyEdit::Clear,
            (None, false) => KeyEdit::Keep,
        };
        let edit = ConnectionEdit {
            id: existing(request.id),
            revision: request.revision,
            preset: preset_id(request.preset)?,
            base_url: request.base_url.to_owned(),
            key,
        };
        let view = self.service.set_connection(edit).await.map_err(refused)?;
        Response::ok(SetConnectionResponse {
            assistant: assistant(view),
            ..Default::default()
        })
    }

    async fn delete_connection(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, DeleteConnectionRequest>,
    ) -> ServiceResult<DeleteConnectionResponse> {
        let view = self
            .service
            .delete_connection(request.id, request.revision)
            .await
            .map_err(refused)?;
        Response::ok(DeleteConnectionResponse {
            assistant: assistant(view),
            ..Default::default()
        })
    }

    async fn set_slot(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetSlotRequest>,
    ) -> ServiceResult<SetSlotResponse> {
        let kind = slot_kind(&request.slot)?;
        let edit = SlotEdit {
            connection: request.connection.to_owned(),
            model: request.model.to_owned(),
            speech_model: request.speech_model.to_owned(),
        };
        let view = self.service.set_slot(kind, edit).await.map_err(refused)?;
        Response::ok(SetSlotResponse {
            assistant: assistant(view),
            ..Default::default()
        })
    }

    async fn update_assistant(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, UpdateAssistantRequest>,
    ) -> ServiceResult<UpdateAssistantResponse> {
        let r = request.to_owned_message();
        let prompt = match (r.prompt, r.default_prompt) {
            (Some(_), true) => {
                return Err(ConnectError::invalid_argument(
                    "Enter a prompt or go back to the voice rules, not both.",
                ));
            }
            (Some(prompt), false) => Some(Some(prompt)),
            (None, true) => Some(None),
            (None, false) => None,
        };
        let patch = AssistantPatch {
            // An empty language or place clears it.
            language: r.language,
            prompt,
            prompt_revision: (!r.prompt_revision.is_empty()).then_some(r.prompt_revision),
            follow_ups: r.follow_ups,
            ack: r.ack.into_option().map(|a| Ack {
                enabled: a.enabled,
                phrase: a.phrase,
            }),
            tools: r.tools.into_option().map(|t| t.on.into_iter().collect()),
            chat_override: r
                .brain_override
                .as_option()
                .map(chat_override)
                .transpose()?,
            place: r.place,
            stations: r.stations.into_option().map(|list| {
                list.stations
                    .into_iter()
                    .map(|s| Station {
                        name: s.name,
                        url: s.url,
                    })
                    .collect()
            }),
        };
        let view = self.service.update(patch).await.map_err(refused)?;
        Response::ok(UpdateAssistantResponse {
            assistant: assistant(view),
            ..Default::default()
        })
    }

    async fn set_mcp_server(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetMcpServerRequest>,
    ) -> ServiceResult<SetMcpServerResponse> {
        let edit = McpEdit {
            id: existing(request.id),
            revision: request.revision,
            name: request.name.to_owned(),
            url: request.url.map(Secret::new),
            enabled: request.enabled,
        };
        let view = self.service.set_mcp_server(edit).await.map_err(refused)?;
        Response::ok(SetMcpServerResponse {
            assistant: assistant(view),
            ..Default::default()
        })
    }

    async fn delete_mcp_server(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, DeleteMcpServerRequest>,
    ) -> ServiceResult<DeleteMcpServerResponse> {
        let view = self
            .service
            .delete_mcp_server(request.id, request.revision)
            .await
            .map_err(refused)?;
        Response::ok(DeleteMcpServerResponse {
            assistant: assistant(view),
            ..Default::default()
        })
    }

    async fn play_sample(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, PlaySampleRequest>,
    ) -> ServiceResult<PlaySampleResponse> {
        let text = checks::speak_text(request.text).map_err(ConnectError::invalid_argument)?;
        self.brain(|reply| ApiCommand::Speak(text, reply)).await?;
        Response::ok(PlaySampleResponse::default())
    }

    async fn ask_text(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, AskTextRequest>,
    ) -> ServiceResult<AskTextResponse> {
        let question = checks::question(request.text).map_err(ConnectError::invalid_argument)?;
        self.brain(|reply| ApiCommand::AskText(question, reply))
            .await?;
        Response::ok(AskTextResponse::default())
    }

    async fn list_choices(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ListChoicesRequest>,
    ) -> ServiceResult<ListChoicesResponse> {
        let kind = slot_kind(&request.slot)?;
        let wanted = match request.kind.as_known() {
            Some(ChoiceKind::CHOICE_KIND_MODEL) => Wanted::Models,
            Some(ChoiceKind::CHOICE_KIND_VOICE) => Wanted::Voices,
            _ => return Err(ConnectError::invalid_argument("Choose models or voices.")),
        };
        let list = self
            .service
            .choices(kind, request.connection, wanted)
            .await
            .map_err(refused)?;
        Response::ok(ListChoicesResponse {
            choices: list
                .into_iter()
                .map(|c| pinna_api::Choice {
                    id: c.id,
                    label: c.label,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    async fn get_satellite_group(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, GetSatelliteGroupRequest>,
    ) -> ServiceResult<GetSatelliteGroupResponse> {
        Response::ok(GetSatelliteGroupResponse {
            group: self.group_view(),
            ..Default::default()
        })
    }

    async fn set_satellite_group(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetSatelliteGroupRequest>,
    ) -> ServiceResult<SetSatelliteGroupResponse> {
        if let Some(on) = request.on {
            // Off leaves the group, and tells the other members first.
            match &self.group {
                Some(group) => group.set_switch(on).await,
                None => self.service.set_group_switch(on).await.map(|_| ()),
            }
            .map_err(refused)?;
        }
        if let Some(control) = request.control {
            self.service
                .set_group_control(control)
                .await
                .map_err(refused)?;
        }
        Response::ok(SetSatelliteGroupResponse {
            group: self.group_view(),
            ..Default::default()
        })
    }

    async fn add_group_member(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, AddGroupMemberRequest>,
    ) -> ServiceResult<AddGroupMemberResponse> {
        self.group()?.add(request.mac).await.map_err(refused)?;
        Response::ok(AddGroupMemberResponse {
            group: self.group_view(),
            ..Default::default()
        })
    }

    async fn remove_group_member(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, RemoveGroupMemberRequest>,
    ) -> ServiceResult<RemoveGroupMemberResponse> {
        self.group()?.remove(request.mac).await.map_err(refused)?;
        Response::ok(RemoveGroupMemberResponse {
            group: self.group_view(),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use buffa::Message as _;
    use connectrpc::client::{ClientConfig, HttpClient};
    use connectrpc::{ErrorCode, Server};
    use pinna_api::{
        AssistantServiceClient, DeviceServiceClient, GetStatusRequest, Status,
        UpdateSettingsRequest,
    };
    use pinna_assist::testing::{FakeResponse, FakeServer};
    use pinna_esphome::{DeviceEvent, EsphomeHandle};
    use pinna_http::ClientKind;
    use pinna_http::reqwest::{Method, StatusCode};
    use tokio::sync::{mpsc, watch};

    use super::*;
    use crate::api::{DeviceApi, Info};
    use crate::leds::LedState;
    use crate::settings::Settings;
    use crate::standalone::service::Paths;
    use crate::voice::{Brain, Defaults, LocalOptions, NoopSpawner, Options, Shared, Snapshot};

    const KEY: &str = "sk-test-abcdef123456";
    const MODELS: &str = r#"{"data":[{"id":"whisper-1"},{"id":"gpt-test"}]}"#;

    /// The response's `Assistant`, or the refusal.
    macro_rules! assistant {
        ($call:expr) => {
            $call.await.map(|r| {
                r.into_view()
                    .to_owned_message()
                    .assistant
                    .into_option()
                    .expect("an edit answers with the assistant")
            })
        };
    }

    /// A real Brain reading the service's snapshot, with both APIs on one
    /// ephemeral port, as the dashboard listener serves them.
    struct Device {
        device: DeviceServiceClient<HttpClient>,
        assistant: AssistantServiceClient<HttpClient>,
        url: String,
        service: Arc<Service>,
        /// What the search found, as the satellite group lists it.
        satellites: watch::Sender<Seen>,
        _events: mpsc::Receiver<DeviceEvent>,
        _dir: tempfile::TempDir,
    }

    /// This satellite's MAC in the tests.
    const OWN_MAC: &str = "02:00:00:aa:bb:cc";

    impl Device {
        async fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let settings_file = dir.path().join("settings.toml");
            let (esp, events) = EsphomeHandle::detached(256);
            let (status_tx, status_rx) = watch::channel(Snapshot::default());
            let (inputs, rx) = mpsc::channel(64);
            let service = Service::load(Paths::beside(&settings_file), status_rx.clone());
            let brain = Brain::new_with_local(
                esp,
                None,
                Arc::new(Shared::default()),
                watch::channel(LedState::default()).0,
                status_tx,
                inputs.clone(),
                Options {
                    key_file: dir.path().join("key"),
                    settings_file,
                    wake_words: crate::wakewords::test_catalogue(),
                    active_wake_words: vec!["alexa_v0.1".into()],
                    keywords: None,
                    defaults: Defaults {
                        node_name: "pinna-test".into(),
                        friendly_name: "Test".into(),
                        default_name: true,
                        wake_sound: false,
                        update_channel: crate::updates::UpdateChannel::Off,
                    },
                    updates: None,
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    privacy_trigger: Vec::new(),
                    mute_lamp: None,
                    airplay: false,
                    bluetooth: false,
                    sendspin: None,
                    settings: Settings::default(),
                },
                LocalOptions {
                    live: service.live(),
                    spawner: Arc::new(NoopSpawner),
                    timers_file: dir.path().join("timers.toml"),
                },
            );
            tokio::spawn(brain.run(rx));
            let info = Info {
                version: "0.1.0".into(),
                mac: "02:00:00:AA:BB:CC".into(),
                time_zones: Arc::new(vec!["UTC".into()]),
                audio_backend: "none".into(),
                led_backend: "none".into(),
                model: "echo-dot-2".into(),
                virtual_device: false,
                last_boot: crate::boot::LastBoot::cold(),
                key_from_image: false,
            };
            let device = Arc::new(DeviceApi::new(info, status_rx, inputs.clone()));
            let (satellites, seen) = watch::channel(Seen::default());
            let router = crate::api::router(device).add_service(Arc::new(
                AssistantApi::new(service.clone(), inputs).with_satellites(seen, OWN_MAC.into()),
            ));
            let server = Server::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", server.local_addr().unwrap());
            tokio::spawn(async move { server.serve(router).await });
            let config = || ClientConfig::new(url.parse().unwrap());
            Self {
                device: DeviceServiceClient::new(HttpClient::plaintext(), config()),
                assistant: AssistantServiceClient::new(HttpClient::plaintext(), config()),
                url,
                service,
                satellites,
                _events: events,
                _dir: dir,
            }
        }

        /// Switched on, in a group with one other satellite. Returns the
        /// group's key in every form it could leak in: base64 (how JSON
        /// carries bytes) and hex.
        async fn in_group(&self) -> [String; 2] {
            use crate::group::model::Member;
            self.service.set_group_switch(true).await.unwrap();
            let own = Member {
                mac: OWN_MAC.into(),
                name: "Test".into(),
                version: 1,
                removed: false,
            };
            let other = Member {
                mac: "02:00:00:aa:bb:cd".into(),
                name: "Kitchen".into(),
                ..own.clone()
            };
            let group = crate::group::Joined::start(own, other).unwrap();
            self.service
                .start_group(group.clone(), OWN_MAC)
                .await
                .unwrap();
            [
                group.key.to_base64(),
                group
                    .key
                    .bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
            ]
        }

        async fn status(&self) -> Status {
            self.device
                .get_status(GetStatusRequest::default())
                .await
                .unwrap()
                .into_view()
                .to_owned_message()
                .status
                .into_option()
                .unwrap()
        }

        /// One OpenAI-compatible connection to `server`, with `KEY`.
        async fn connect(&self, server: &FakeServer) -> ProviderConnection {
            let a = assistant!(self.assistant.set_connection(SetConnectionRequest {
                preset: "openai".into(),
                base_url: server.url(""),
                key: Some(KEY.into()),
                ..Default::default()
            }))
            .unwrap();
            a.connections.last().unwrap().clone()
        }

        /// All three slots on one connection to `server`, each checked.
        async fn ready(&self, server: &FakeServer) -> Assistant {
            let connection = self.connect(server).await;
            let mut last = None;
            for (slot, model) in [
                (AssistantSlot::ASSISTANT_SLOT_STT, "whisper-1"),
                (AssistantSlot::ASSISTANT_SLOT_BRAIN, "gpt-test"),
                (AssistantSlot::ASSISTANT_SLOT_TTS, "nova"),
            ] {
                passing(server, slot);
                last = Some(
                    assistant!(
                        self.assistant
                            .set_slot(slot_request(slot, &connection.id, model))
                    )
                    .unwrap(),
                );
            }
            last.unwrap()
        }

        /// A Connect unary call with a JSON body, as the dashboard makes it:
        /// the status and the body exactly as they went over the wire.
        async fn json(&self, procedure: &str, body: &str) -> (u16, String) {
            let response = pinna_http::client(ClientKind::Mcp)
                .post(format!("{}/pinna.v1.{procedure}", self.url))
                .header("content-type", "application/json")
                .body(body.to_owned())
                .send()
                .await
                .unwrap();
            (response.status().as_u16(), response.text().await.unwrap())
        }
    }

    fn slot_request(slot: AssistantSlot, connection: &str, model: &str) -> SetSlotRequest {
        SetSlotRequest {
            slot: EnumValue::Known(slot),
            connection: connection.into(),
            model: model.into(),
            ..Default::default()
        }
    }

    /// Script one passing check for `slot` on an OpenAI-compatible server.
    fn passing(server: &FakeServer, slot: AssistantSlot) {
        if slot == AssistantSlot::ASSISTANT_SLOT_TTS {
            server.script(
                Method::POST,
                "/audio/speech",
                FakeResponse::new(StatusCode::OK, "audio"),
            );
        } else {
            server.script(
                Method::GET,
                "/models",
                FakeResponse::new(StatusCode::OK, MODELS),
            );
        }
    }

    fn standalone() -> UpdateSettingsRequest {
        UpdateSettingsRequest {
            mode: Some(EnumValue::Known(pinna_api::Mode::MODE_STANDALONE)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_full_setup_lets_standalone_mode_switch_on() {
        let d = Device::start().await;
        let server = FakeServer::start().await;
        let before = d.status().await;
        let status = before.assistant.as_option().unwrap();
        assert!(!status.ready && !status.stt_ready && !status.brain_ready && !status.tts_ready);
        assert_eq!(status.label, "");

        let connection = d.connect(&server).await;
        assert_eq!(connection.id, "openai-1");
        assert_eq!(connection.preset, "openai");
        assert!(connection.key_set);
        assert_eq!(connection.key_last4, "3456");
        let refused = d.device.update_settings(standalone()).await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::FailedPrecondition);

        let a = d.ready(&server).await;

        assert!(a.ready);
        for slot in [&a.stt, &a.brain, &a.tts] {
            assert!(slot.as_option().unwrap().ready);
        }
        assert_eq!(a.brain.as_option().unwrap().model, "gpt-test");
        assert_eq!(a.tts.as_option().unwrap().model, "nova");
        let status = d
            .device
            .update_settings(standalone())
            .await
            .unwrap()
            .into_view()
            .to_owned_message()
            .status
            .into_option()
            .unwrap();
        assert_eq!(
            status.mode.as_known(),
            Some(pinna_api::Mode::MODE_STANDALONE)
        );
        let assistant = status.assistant.as_option().unwrap();
        assert!(assistant.ready);
        assert!(assistant.stt_ready && assistant.brain_ready && assistant.tts_ready);
        assert_eq!(assistant.label, "openai: gpt-test");
        assert_eq!(assistant.last_error, "");
    }

    #[tokio::test]
    async fn choices_come_from_the_connections_service() {
        let d = Device::start().await;
        let server = FakeServer::start().await;
        let connection = d.connect(&server).await;
        server.script(
            Method::GET,
            "/models",
            FakeResponse::new(StatusCode::OK, MODELS),
        );
        let ask = |slot, kind| pinna_api::ListChoicesRequest {
            slot: EnumValue::Known(slot),
            connection: connection.id.clone(),
            kind: EnumValue::Known(kind),
            ..Default::default()
        };
        let listed = d
            .assistant
            .list_choices(ask(
                AssistantSlot::ASSISTANT_SLOT_STT,
                ChoiceKind::CHOICE_KIND_MODEL,
            ))
            .await
            .unwrap()
            .into_view()
            .to_owned_message();
        let ids: Vec<String> = listed.choices.into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["whisper-1"]);
        // The key went along, and never comes back.
        assert_eq!(
            server.requests()[0].header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );

        let unknown = d
            .assistant
            .list_choices(pinna_api::ListChoicesRequest {
                connection: "gone-1".into(),
                ..ask(
                    AssistantSlot::ASSISTANT_SLOT_BRAIN,
                    ChoiceKind::CHOICE_KIND_MODEL,
                )
            })
            .await
            .unwrap_err();
        assert_eq!(unknown.code, ErrorCode::Aborted);
    }

    #[tokio::test]
    async fn no_response_carries_the_key() {
        let d = Device::start().await;
        let server = FakeServer::start().await;
        let mut bodies = Vec::new();

        let (status, body) = d
            .json(
                "AssistantService/SetConnection",
                &format!(
                    r#"{{"preset": "openai", "baseUrl": "{}", "key": "{KEY}"}}"#,
                    server.url("")
                ),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""keySet":true"#), "{body}");
        assert!(body.contains(r#""keyLast4":"3456""#), "{body}");
        bodies.push(body);
        let a = d.ready(&server).await;
        let group_key = d.in_group().await;
        let id = &a.connections[0].id;
        // Refusals don't echo it either.
        for request in [
            format!(r#"{{"id": "{id}", "revision": 1, "preset": "openai", "key": "{KEY} x"}}"#),
            format!(r#"{{"id": "{id}", "revision": 9, "preset": "openai", "key": "{KEY}"}}"#),
            format!(r#"{{"preset": "openai", "key": "{KEY}", "clearKey": true}}"#),
        ] {
            let (status, body) = d.json("AssistantService/SetConnection", &request).await;
            assert_ne!(status, 200, "{body}");
            bodies.push(body);
        }
        for procedure in [
            "AssistantService/GetAssistant",
            "DeviceService/GetStatus",
            "AssistantService/UpdateAssistant",
            "AssistantService/GetSatelliteGroup",
            "AssistantService/SetSatelliteGroup",
        ] {
            let (status, body) = d.json(procedure, "{}").await;
            assert_eq!(status, 200, "{body}");
            bodies.push(body);
        }
        let (status, body) = d
            .json(
                "AssistantService/SetSatelliteGroup",
                r#"{"control": false}"#,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""inGroup":true"#), "{body}");
        bodies.push(body);
        for body in &bodies {
            assert!(!body.contains(KEY), "{body}");
            for key in &group_key {
                assert!(!body.contains(key.as_str()), "{body}");
            }
        }

        // The binary encoding, as the typed clients receive it.
        let got = d
            .assistant
            .get_assistant(GetAssistantRequest::default())
            .await
            .unwrap()
            .into_view()
            .to_owned_message();
        let bytes = got.encode_to_vec();
        assert!(!bytes.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
        assert!(!format!("{got:?}").contains(KEY));
        assert!(got.assistant.as_option().unwrap().connections[0].key_set);
        let raw_key = d.service.group().joined.unwrap().key;
        let group = d
            .assistant
            .get_satellite_group(GetSatelliteGroupRequest::default())
            .await
            .unwrap()
            .into_view()
            .to_owned_message();
        let bytes = group.encode_to_vec();
        assert!(!bytes.windows(32).any(|w| w == raw_key.bytes()));
        for key in &group_key {
            assert!(!format!("{group:?}").contains(key.as_str()));
        }
        assert!(group.group.as_option().unwrap().in_group);
    }

    #[tokio::test]
    async fn the_satellite_group_switches_and_lists_the_search() {
        use pinna_api::satellite_group_candidate::State;

        let d = Device::start().await;
        d.satellites.send_replace(Seen {
            searched: true,
            on_network: true,
            satellites: vec![crate::satellites::Found {
                name: "kitchen".into(),
                host: "kitchen".into(),
                friendly_name: "Kitchen".into(),
                board: "Echo Dot (2nd Gen)".into(),
                mac: "02:00:00:aa:bb:cd".into(),
                version: "0.4.0".into(),
                web_port: 8080,
                url: String::new(),
                ipv4: vec!["192.168.1.21".parse().unwrap()],
                ipv6: Vec::new(),
                group: Some(String::new()),
                missed: 0,
            }],
            error: None,
            search: 1,
        });
        let group = |response: SetSatelliteGroupResponse| response.group.into_option().unwrap();

        let got = d
            .assistant
            .get_satellite_group(GetSatelliteGroupRequest::default())
            .await
            .unwrap()
            .into_view()
            .to_owned_message()
            .group
            .into_option()
            .unwrap();
        // Off by default, with Control from the group on, and nothing listed.
        assert!(!got.on && !got.in_group && got.control);
        assert!(got.candidates.is_empty());

        let on = group(
            d.assistant
                .set_satellite_group(SetSatelliteGroupRequest {
                    on: Some(true),
                    ..Default::default()
                })
                .await
                .unwrap()
                .into_view()
                .to_owned_message(),
        );
        assert!(on.on && !on.in_group && on.control);
        assert!(on.members.is_empty());
        let candidate = &on.candidates[0];
        assert_eq!(candidate.state, EnumValue::Known(State::STATE_ADDABLE));
        assert!(candidate.addable && !candidate.default_name);
        assert_eq!(candidate.name_refusal, "");
        let satellite = candidate.satellite.as_option().unwrap();
        assert_eq!(
            (
                satellite.friendly_name.as_str(),
                satellite.host.as_str(),
                satellite.web_port
            ),
            ("Kitchen", "kitchen", 8080)
        );
        assert_eq!(d.service.group().record(), Some(String::new()));

        let control = group(
            d.assistant
                .set_satellite_group(SetSatelliteGroupRequest {
                    control: Some(false),
                    ..Default::default()
                })
                .await
                .unwrap()
                .into_view()
                .to_owned_message(),
        );
        assert!(control.on && !control.control);

        let off = group(
            d.assistant
                .set_satellite_group(SetSatelliteGroupRequest {
                    on: Some(false),
                    ..Default::default()
                })
                .await
                .unwrap()
                .into_view()
                .to_owned_message(),
        );
        assert!(!off.on && !off.control);
        assert!(off.candidates.is_empty());
        assert_eq!(d.service.group().record(), None);
    }

    #[tokio::test]
    async fn refusals_carry_their_connect_code() {
        let d = Device::start().await;
        let server = FakeServer::start().await;
        let connection = d.connect(&server).await;
        let refused = |call: Result<Assistant, ConnectError>| {
            let e = call.unwrap_err();
            (e.code, e.message.unwrap_or_default())
        };

        // Wrong in itself, as the service or the handler finds it.
        let cases = [
            (
                SetConnectionRequest {
                    preset: "custom".into(),
                    base_url: "https://user:pw@example.com/v1".into(),
                    ..Default::default()
                },
                "Leave out the user name and password.",
            ),
            (
                SetConnectionRequest {
                    preset: "martian".into(),
                    ..Default::default()
                },
                "There is no preset \"martian\".",
            ),
            (
                SetConnectionRequest {
                    preset: "openai".into(),
                    key: Some("sk-new-0000".into()),
                    clear_key: true,
                    ..Default::default()
                },
                "Enter a key or remove it, not both.",
            ),
        ];
        for (request, sentence) in cases {
            assert_eq!(
                refused(assistant!(d.assistant.set_connection(request))),
                (ErrorCode::InvalidArgument, sentence.to_owned())
            );
        }
        assert_eq!(
            refused(assistant!(d.assistant.set_slot(slot_request(
                AssistantSlot::ASSISTANT_SLOT_UNSPECIFIED,
                &connection.id,
                "gpt-test",
            )))),
            (
                ErrorCode::InvalidArgument,
                "Choose speech-to-text, the brain or text-to-speech.".to_owned()
            )
        );
        assert_eq!(
            refused(assistant!(d.assistant.update_assistant(
                UpdateAssistantRequest {
                    brain_override: MessageField::some(BrainOverride {
                        history: EnumValue::Known(pinna_api::History::HISTORY_TURNS),
                        history_limit: 11,
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            ))),
            (
                ErrorCode::InvalidArgument,
                "Enter 1 to 10 requests.".to_owned()
            )
        );

        // The provider refuses it now.
        server.script_status(Method::GET, "/models", StatusCode::UNAUTHORIZED);
        assert_eq!(
            refused(assistant!(d.assistant.set_slot(slot_request(
                AssistantSlot::ASSISTANT_SLOT_BRAIN,
                &connection.id,
                "gpt-test",
            )))),
            (
                ErrorCode::FailedPrecondition,
                "The key is wrong. Enter it again.".to_owned()
            )
        );

        // Made on a revision that changed meanwhile.
        let stale = SetConnectionRequest {
            id: connection.id.clone(),
            revision: connection.revision,
            preset: "openai".into(),
            base_url: server.url("/v2"),
            ..Default::default()
        };
        assistant!(d.assistant.set_connection(stale.clone())).unwrap();
        assert_eq!(
            refused(assistant!(d.assistant.set_connection(stale))),
            (
                ErrorCode::Aborted,
                "This connection was changed meanwhile. Reload and try again.".to_owned()
            )
        );
        let gone = d
            .assistant
            .delete_connection(DeleteConnectionRequest {
                id: connection.id.clone(),
                revision: connection.revision,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(gone.code, ErrorCode::Aborted);

        // Empty text is the caller's mistake; the Brain refuses the rest with
        // a sentence (no text-to-speech slot behind these fakes).
        let e = d
            .assistant
            .play_sample(PlaySampleRequest::default())
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        let e = d
            .assistant
            .play_sample(PlaySampleRequest {
                text: "Hello.".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::FailedPrecondition);
        let e = d
            .assistant
            .ask_text(AskTextRequest::default())
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        let e = d
            .assistant
            .ask_text(AskTextRequest {
                text: "What time is it?".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::FailedPrecondition);
    }

    #[tokio::test]
    async fn mcp_servers_are_shown_by_host_and_never_by_address() {
        let d = Device::start().await;
        // A closed port on this machine: saving asks it for its tools, and
        // it fails at once, locally.
        let address = "http://127.0.0.1:1/private-token?key=s3cret";

        let (status, added) = d
            .json(
                "AssistantService/SetMcpServer",
                &format!(r#"{{"name": "home", "url": "{address}", "enabled": true}}"#),
            )
            .await;

        assert_eq!(status, 200, "{added}");
        assert!(added.contains(r#""host":"127.0.0.1:1""#), "{added}");
        assert!(
            added.contains(r#""problem":"The MCP server is unreachable.""#),
            "{added}"
        );
        let (_, got) = d.json("AssistantService/GetAssistant", "{}").await;
        let bad = format!(r#"{{"name": "home-2", "url": "{address}#x", "enabled": true}}"#);
        let (status, refused) = d.json("AssistantService/SetMcpServer", &bad).await;
        assert_eq!(status, 400, "{refused}");
        for body in [&added, &got, &refused] {
            assert!(!body.contains("private-token"), "{body}");
            assert!(!body.contains("s3cret"), "{body}");
        }

        let a = assistant!(d.assistant.get_assistant(GetAssistantRequest::default())).unwrap();
        let home = a.mcp_servers[0].clone();
        assert_eq!((home.name.as_str(), home.revision), ("home", 1));
        assert!(home.enabled);
        let rename = |revision| SetMcpServerRequest {
            id: home.id.clone(),
            revision,
            name: "house".into(),
            enabled: true,
            ..Default::default()
        };
        let a = assistant!(d.assistant.set_mcp_server(rename(home.revision))).unwrap();
        assert_eq!(a.mcp_servers[0].name, "house");
        assert_eq!(a.mcp_servers[0].host, "127.0.0.1:1");
        let stale = d
            .assistant
            .set_mcp_server(rename(home.revision))
            .await
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::Aborted);
        let add = |n| SetMcpServerRequest {
            name: format!("s{n}"),
            url: Some(format!("https://mcp{n}.example/mcp")),
            ..Default::default()
        };
        for n in 1..8 {
            assistant!(d.assistant.set_mcp_server(add(n))).unwrap();
        }
        let e = d.assistant.set_mcp_server(add(8)).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        assert_eq!(e.message.as_deref(), Some("At most 8 MCP servers."));

        let house = &a.mcp_servers[0];
        let a = assistant!(d.assistant.delete_mcp_server(DeleteMcpServerRequest {
            id: house.id.clone(),
            revision: house.revision,
            ..Default::default()
        }))
        .unwrap();
        assert!(a.mcp_servers.iter().all(|s| s.id != house.id));
    }

    #[tokio::test]
    async fn settings_without_checks_round_trip() {
        let d = Device::start().await;
        let a = assistant!(d.assistant.get_assistant(GetAssistantRequest::default())).unwrap();
        assert!(!a.ready);
        assert_eq!(a.prompt, None);
        assert!(a.follow_ups);
        let openclaw = a.presets.iter().find(|p| p.id == "openclaw").unwrap();
        assert!(openclaw.hand_off && openclaw.brain && !openclaw.stt);
        assert_eq!(openclaw.model, "openclaw/default");
        assert_eq!(openclaw.base_url, "http://<host>:<port>/v1");
        assert_eq!(a.presets.len(), presets::presets().len());
        assert_eq!(a.voice_rules, VOICE_RULES);
        assert_eq!(a.ack_spoken, "One moment.");
        let names: Vec<&str> = a.device_tools.iter().map(|t| t.name.as_str()).collect();
        let catalogue: Vec<&str> = tools::catalogue().map(|t| t.name).collect();
        assert_eq!(names, catalogue);
        let weather = a.device_tools.iter().find(|t| t.name == "weather").unwrap();
        assert!(weather.brain && !weather.mcp && !weather.description.is_empty());
        assert!(!weather.off_by_default);
        let install = a.device_tools.iter().find(|t| t.name == "install_update");
        assert!(install.is_some_and(|t| t.brain && !t.mcp && t.off_by_default));

        let override_ = BrainOverride {
            delegation: EnumValue::Known(pinna_api::Delegation::DELEGATION_HAND_OFF),
            history: EnumValue::Known(pinna_api::History::HISTORY_BYTES),
            history_limit: 4096,
            first_text_seconds: Some(90),
            reasoning: EnumValue::Known(pinna_api::Reasoning::REASONING_NONE),
            ..Default::default()
        };
        let a = assistant!(d.assistant.update_assistant(UpdateAssistantRequest {
            language: Some("de".into()),
            prompt: Some("Be brief.".into()),
            follow_ups: Some(false),
            ack: MessageField::some(Acknowledgment {
                enabled: true,
                phrase: "Moment.".into(),
                ..Default::default()
            }),
            tools: MessageField::some(pinna_api::ToolSwitches {
                on: [("weather".to_owned(), false)].into_iter().collect(),
                ..Default::default()
            }),
            brain_override: MessageField::some(override_.clone()),
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(a.language, "de");
        assert_eq!(a.prompt.as_deref(), Some("Be brief."));
        assert!(!a.follow_ups);
        assert_eq!(a.ack.as_option().unwrap().phrase, "Moment.");
        assert_eq!(a.ack_spoken, "Moment.");
        assert_eq!(a.tools.get("weather"), Some(&false));
        assert_eq!(a.brain_override.as_option(), Some(&override_));

        let a = assistant!(d.assistant.update_assistant(UpdateAssistantRequest {
            language: Some(String::new()),
            default_prompt: true,
            place: Some(String::new()),
            brain_override: MessageField::some(BrainOverride::default()),
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(a.language, "");
        assert_eq!(a.prompt, None);
        assert!(a.place.as_option().is_none());
        assert_eq!(
            a.brain_override.as_option(),
            Some(&brain_override(&ChatOverride::default()))
        );
        // Untouched fields keep their values.
        assert!(!a.follow_ups);

        let both = d
            .assistant
            .update_assistant(UpdateAssistantRequest {
                prompt: Some("x".into()),
                default_prompt: true,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(both.code, ErrorCode::InvalidArgument);
    }
}
