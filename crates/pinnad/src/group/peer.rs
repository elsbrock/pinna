//! `pinna.v1.PeerService` (ADR 0026): the calls other members of the group
//! make, served on the dashboard's listener beside `DeviceService` and
//! `AssistantService` (ADR 0005). Its callers are satellites, not browsers.
//!
//! `Join` is the only unsigned call. Every other call is an envelope whose
//! tag is checked before its body is read, and every answer is signed. A
//! refusal is an unsigned Connect error, so it never makes the caller forget
//! the group.
//!
//! `Deliver` hands a message to the Brain, which says it now or holds it.
//! A message this satellite won't take is a signed answer with the reason,
//! for the sender's brain to say; only a call that fails the checks above
//! is a Connect error.
//!
//! `Call` runs one of the device tools that act on the satellite, as this
//! satellite's `/mcp` would, within a time limit that keeps the answer
//! inside the caller's. It never passes a call on. A tool it won't run is a
//! signed result with the reason, as for a message.
//!
//! With a dashboard password (ADR 0029), `Join` is accepted only inside the
//! window the owner opens on this satellite, and `Call` and `Deliver` are
//! refused from a member that has none, so nobody reaches a protected
//! satellite through an unprotected member.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use buffa::MessageField;
use connectrpc::{ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult};
use pinna_api::peer_delivery::Outcome;
use pinna_api::{
    PeerDelivery, PeerEnvelope, PeerHello, PeerJoinRequest, PeerJoinResponse, PeerMemberList,
    PeerMessage, PeerSender, PeerService, PeerToolCall, PeerToolResult,
};
use pinna_mcp::ToolOutput;
use serde_json::{Map, Value};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use super::client::To;
use super::envelope::{self, Parts, Rpc, Unopened};
use super::model::same_mac;
use super::persisted::Joined;
use super::runtime::{Change, Group, from_wire, is_member, to_wire};
use super::seal::{KEY_LEN, Key};
use crate::auth::{self, Auth};
use crate::mcp::McpServer;
use crate::settings::Mode;
use crate::standalone::service::Refusal;
use crate::tools::{self, SATELLITE};
use crate::voice::{Delivery, Input, Taken, messages};

/// The refusal of a call for a group this satellite isn't in. A member reads
/// this exact refusal of its `GetGroup`, with the search, as the sign that
/// the other satellite left (ADR 0026).
pub const NOT_IN_THIS_GROUP: &str = "This satellite is not in this group.";
const BAD_TAG: &str = "The call isn't signed with the group's key.";
const BAD_BODY: &str = "The call's body isn't the message the call takes.";
const NOT_A_MEMBER: &str = "The caller isn't a member of this group.";
const BAD_KEY: &str = "The group's key isn't 32 bytes.";
const ADDING: &str = "This satellite is adding another one. Try again in a moment.";
const JOINING: &str = "This satellite is joining a group. Try again in a moment.";
const JOIN_CLOSED: &str = "This satellite has a dashboard password. On its dashboard, open Satellite group and choose Allow adding to a group, then add it again.";
const EMPTY: &str = "The message is empty.";
const NOT_JSON: &str = "The call's arguments aren't JSON.";

/// Requests and messages are a member list at most: far below the default
/// 4 MiB, and `Join` is open to anyone on the LAN.
const MAX_BODY: usize = 64 * 1024;

pub struct PeerApi {
    group: Arc<Group>,
    /// The Brain's queue, for the messages it says.
    brain: Option<mpsc::Sender<Input>>,
    /// The device tools as `/mcp` runs them, for the other members' calls.
    tools: Option<Arc<McpServer>>,
    /// Where the owner allows a `Join` (ADR 0029). Without it, a satellite
    /// with a password accepts none.
    auth: Option<Arc<Auth>>,
}

impl PeerApi {
    pub fn new(group: Arc<Group>) -> Self {
        Self {
            group,
            brain: None,
            tools: None,
            auth: None,
        }
    }

    /// Take the owner's say on a `Join` from `auth` (ADR 0029).
    pub fn with_auth(mut self, auth: Arc<Auth>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Whether this satellite takes a `Join` now: without a password the
    /// switch is consent, with one the owner has to allow it first.
    fn join_allowed(&self) -> bool {
        !auth::has_password(&self.group.service().dashboard())
            || self
                .auth
                .as_ref()
                .is_some_and(|auth| auth.group_join_open())
    }

    /// Why a call from a member whose facts are `sender` is refused: this
    /// satellite has a password and the member doesn't (ADR 0029). Worded
    /// for the caller's owner, who is the one who can fix it.
    fn password_refusal(&self, sender: &PeerSender) -> Option<String> {
        (auth::has_password(&self.group.service().dashboard()) && !sender.has_password).then(|| {
            format!(
                "{} has a dashboard password and this satellite doesn't. Set one on this satellite first.",
                self.group.own_name()
            )
        })
    }

    /// Hand messages from the other members to the Brain on `inputs`.
    /// Without it, every message is refused.
    pub fn with_brain(mut self, inputs: mpsc::Sender<Input>) -> Self {
        self.brain = Some(inputs);
        self
    }

    /// Run the other members' tool calls as `/mcp` runs its own. Without
    /// it, every call is refused.
    pub fn with_tools(mut self, tools: Arc<McpServer>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// What this satellite does with a call of `tool` from the member named
    /// `from`, which may take `within`: the tool's result, or why it won't
    /// run it. The checks come in the order the owner can act on them.
    async fn run(
        &self,
        from: &str,
        sender: &PeerSender,
        tool: &str,
        arguments: &str,
        within: Duration,
    ) -> ToolOutput {
        let name = self.group.own_name();
        // A name from the wire is repeated only when it is a tool's.
        let shown = tools::device_tool_name(tool).unwrap_or("that tool");
        let refused = |why: String| {
            tracing::info!("satellite group: {from} asked for {shown}: refused: {why}");
            ToolOutput::error(why)
        };
        if let Some(why) = self.password_refusal(sender) {
            return refused(why);
        }
        let mode = self.group.status().mode;
        if mode == Mode::HomeAssistant && tools::is_timer_tool(tool) {
            return refused(format!(
                "Timers on {name} are Home Assistant's: it is in Home Assistant mode."
            ));
        }
        if !self.group.service().group().control {
            return refused(format!("Control from the group is off on {name}."));
        }
        if !tools::runs_for_group(tool) {
            return refused(format!("{name} doesn't run {shown} for another satellite."));
        }
        let args = if arguments.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            match serde_json::from_str::<Value>(arguments) {
                Ok(args) => args,
                Err(_) => return refused(NOT_JSON.into()),
            }
        };
        // A member acts on itself only: a call is never passed on.
        if args.get(SATELLITE).is_some() {
            return refused(format!(
                "{name} doesn't pass calls on to another satellite."
            ));
        }
        let Some(tools) = &self.tools else {
            return refused(format!("{name} can't run tools for the group now."));
        };
        // Even a tool that times out at once may have reached the Brain.
        if within.is_zero() {
            return refused(format!(
                "{name} didn't run {shown}: checking that {from} is in the group took too long. \
                 Try again."
            ));
        }
        tools.call_for_group(tool, args, within).await
    }

    /// What this satellite does with a message from the member named
    /// `from`: the Brain's answer, or why it won't take it. The checks come
    /// in the order the owner can act on them, and the text is never part
    /// of a reason.
    async fn take(&self, from: &str, sender: &PeerSender, text: &str) -> Taken {
        let name = self.group.own_name();
        if let Some(why) = self.password_refusal(sender) {
            return Taken::Refused(why);
        }
        let refused = {
            let status = self.group.status();
            if !status.settings.group_messages() {
                Some(messages::switched_off(&name))
            } else if !status.assist.slots.tts {
                Some(messages::no_voice(&name))
            } else {
                None
            }
        };
        if let Some(why) = refused {
            return Taken::Refused(why);
        }
        let text = text.trim();
        if text.is_empty() {
            return Taken::Refused(EMPTY.into());
        }
        if text.chars().count() > crate::checks::SPEAK_MAX {
            return Taken::Refused(format!(
                "A message is at most {} characters.",
                crate::checks::SPEAK_MAX
            ));
        }
        let Some(brain) = &self.brain else {
            return Taken::Refused(format!("{name} can't take messages now."));
        };
        // A message never waits for room in the Brain's queue, and leaves
        // room for the wake word and the buttons.
        let busy = || Taken::Refused(format!("{name} is busy. Try again in a moment."));
        let stopping = || Taken::Refused(format!("{name} is restarting. Try again in a moment."));
        if brain.capacity() < crate::tools::BRAIN_RESERVE {
            return busy();
        }
        let (reply, taken) = oneshot::channel();
        let delivery = Delivery {
            from: from.to_owned(),
            text: text.to_owned(),
            reply,
        };
        match brain.try_send(Input::Message(delivery)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return busy(),
            Err(TrySendError::Closed(_)) => return stopping(),
        }
        match tokio::time::timeout(self.group.limits().deliver, taken).await {
            Ok(Ok(taken)) => taken,
            Ok(Err(_)) => stopping(),
            Err(_) => Taken::Refused(format!(
                "{name} didn't take the message in time. Try again in a moment."
            )),
        }
    }

    /// The group an envelope is for and its body, once its tag verifies.
    fn open<M: buffa::Message>(
        &self,
        rpc: Rpc,
        parts: Parts<'_>,
    ) -> Result<(Joined, M), ConnectError> {
        let joined = self
            .group
            .service()
            .group()
            .joined
            .ok_or_else(|| ConnectError::failed_precondition(NOT_IN_THIS_GROUP))?;
        match envelope::open(&joined, rpc.call(), parts) {
            Ok(body) => Ok((joined, body)),
            Err(Unopened::Group) => Err(ConnectError::failed_precondition(NOT_IN_THIS_GROUP)),
            Err(Unopened::Tag) => Err(ConnectError::unauthenticated(BAD_TAG)),
            Err(Unopened::Body) => Err(ConnectError::invalid_argument(BAD_BODY)),
        }
    }

    /// An answer, signed as this satellite.
    fn answer<M: buffa::Message>(&self, joined: &Joined, rpc: Rpc, body: &M) -> PeerEnvelope {
        envelope::seal(joined, self.group.own_mac(), rpc.answer(), body)
    }

    /// The group, once the signed call from `caller` with `sender`'s facts
    /// came from a member: a caller this satellite doesn't list is learnt
    /// first, and refused if it still isn't one.
    async fn verified_member(
        &self,
        joined: Joined,
        caller: &str,
        sender: &PeerSender,
        from: Option<SocketAddr>,
    ) -> Result<Joined, ConnectError> {
        self.group.heard(caller, sender, from.map(|a| a.ip()));
        if is_member(&joined, caller) {
            return Ok(joined);
        }
        self.learn_caller(&joined, caller, sender, from).await?;
        // The merge brought the caller's entry, with its name.
        Ok(self.group.service().group().joined.unwrap_or(joined))
    }

    /// A caller this satellite doesn't list as a member may be a newcomer
    /// whose Add hasn't reached it: fetch the caller's list once, at the
    /// address the call came from, and merge it. The fetch is a `GetGroup`,
    /// which never starts one of its own, so two newcomers calling each
    /// other don't loop.
    async fn learn_caller(
        &self,
        joined: &Joined,
        caller: &str,
        sender: &PeerSender,
        from: Option<SocketAddr>,
    ) -> Result<(), ConnectError> {
        let port = u16::try_from(sender.web_port).ok().filter(|p| *p != 0);
        if let (Some(from), Some(port)) = (from, port) {
            let to = To::at(SocketAddr::new(from.ip().to_canonical(), port));
            let limit = self.group.limits().exchange;
            match self.group.get_group(&to, joined, caller, limit).await {
                Ok(list) => {
                    if let Err(e) = self.group.merge(&joined.id, &list, &[]).await {
                        tracing::info!("satellite group: merging the list of {caller}: {e}");
                    }
                }
                Err(e) => self
                    .group
                    .failed(caller, caller, "asking for the list of", &e),
            }
        }
        match self.group.service().group().joined {
            Some(now) if now.id == joined.id && is_member(&now, caller) => Ok(()),
            Some(now) if now.id == joined.id => Err(ConnectError::permission_denied(NOT_A_MEMBER)),
            _ => Err(ConnectError::failed_precondition(NOT_IN_THIS_GROUP)),
        }
    }
}

/// The routes, for the dashboard listener. Never on the loopback API.
pub fn router(api: Arc<PeerApi>) -> Router {
    Router::new().add_service(api)
}

/// The limits for the listener's PeerService routes.
pub fn limits() -> connectrpc::Limits {
    connectrpc::Limits::default()
        .with_max_request_body_size(MAX_BODY)
        .with_max_message_size(MAX_BODY)
}

/// The name `group`'s list has for the member `mac`, or its MAC.
fn name_in(group: &Joined, mac: &str) -> String {
    group
        .members
        .iter()
        .find(|m| same_mac(&m.mac, mac))
        .map(|m| m.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(mac)
        .to_owned()
}

/// `limit` to the nearest tenth of a second, as a result that ran out of
/// time says it.
fn tenths(limit: Duration) -> Duration {
    let tenths = (limit.as_millis() + 50) / 100;
    Duration::from_millis(u64::try_from(tenths * 100).unwrap_or(u64::MAX))
}

/// The sender's facts in a body; a body without them reads as Home
/// Assistant mode on port 0.
fn sender_of(field: Option<&PeerSender>) -> PeerSender {
    field.cloned().unwrap_or_default()
}

#[allow(refining_impl_trait)]
impl PeerService for PeerApi {
    async fn join(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, PeerJoinRequest>,
    ) -> ServiceResult<PeerJoinResponse> {
        if !self.join_allowed() {
            return Err(ConnectError::failed_precondition(JOIN_CLOSED));
        }
        // A satellite adding another one refuses to be added itself: two
        // that add each other at once both fail, and try again.
        let _joining = self.group.begin(Change::Joining).map_err(|busy| {
            ConnectError::unavailable(match busy {
                Change::Adding => ADDING,
                Change::Joining => JOINING,
            })
        })?;
        let request = request.to_owned_message();
        let key: [u8; KEY_LEN] = request
            .key
            .as_slice()
            .try_into()
            .map_err(|_| ConnectError::invalid_argument(BAD_KEY))?;
        let group = Joined {
            id: request.group_id,
            key: Key::new(key),
            members: request.members.iter().map(from_wire).collect(),
        };
        let own = self
            .group
            .service()
            .join_group(group, self.group.own_mac(), &self.group.own_name())
            .await
            .map_err(crate::assistant_api::refused)?;
        // What the members refused before is of no group this one is in.
        self.group.reset_outside();
        Response::ok(PeerJoinResponse {
            member: MessageField::some(to_wire(&own)),
            ..Default::default()
        })
    }

    async fn get_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        let parts = Parts {
            group_id: request.group_id,
            sender_mac: request.sender_mac,
            body: request.body,
            tag: request.tag,
        };
        // A valid tag is enough, even from a satellite this one doesn't
        // know: a removed member learns of its removal this way.
        let (joined, hello): (_, PeerHello) = self.open(Rpc::GetGroup, parts)?;
        let from = ctx.peer_addr().map(|a| a.ip());
        self.group.heard(
            request.sender_mac,
            &sender_of(hello.sender.as_option()),
            from,
        );
        let list = PeerMemberList {
            sender: MessageField::some(self.group.sender()),
            members: joined.members.iter().map(to_wire).collect(),
            ..Default::default()
        };
        Response::ok(self.answer(&joined, Rpc::GetGroup, &list))
    }

    async fn put_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        let parts = Parts {
            group_id: request.group_id,
            sender_mac: request.sender_mac,
            body: request.body,
            tag: request.tag,
        };
        let (joined, list): (_, PeerMemberList) = self.open(Rpc::PutGroup, parts)?;
        let caller = request.sender_mac.to_owned();
        let sender = &sender_of(list.sender.as_option());
        self.group
            .heard(&caller, sender, ctx.peer_addr().map(|a| a.ip()));
        if !is_member(&joined, &caller) {
            self.learn_caller(&joined, &caller, sender, ctx.peer_addr())
                .await?;
        }
        let members: Vec<_> = list.members.iter().map(from_wire).collect();
        match self.group.merge(&joined.id, &members, &[]).await {
            Ok(_) => {}
            Err(Refusal::Conflict(_)) => {
                return Err(ConnectError::failed_precondition(NOT_IN_THIS_GROUP));
            }
            Err(other) => return Err(ConnectError::unavailable(other.sentence())),
        }
        let hello = PeerHello {
            sender: MessageField::some(self.group.sender()),
            ..Default::default()
        };
        Response::ok(self.answer(&joined, Rpc::PutGroup, &hello))
    }

    async fn deliver(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        let parts = Parts {
            group_id: request.group_id,
            sender_mac: request.sender_mac,
            body: request.body,
            tag: request.tag,
        };
        let (joined, message): (_, PeerMessage) = self.open(Rpc::Deliver, parts)?;
        let caller = request.sender_mac.to_owned();
        let sender = &sender_of(message.sender.as_option());
        let joined = self
            .verified_member(joined, &caller, sender, ctx.peer_addr())
            .await?;
        let from = name_in(&joined, &caller);
        let taken = self.take(&from, sender, &message.text).await;
        let (outcome, reason) = match taken {
            Taken::Played => (Outcome::OUTCOME_PLAYED, String::new()),
            Taken::Held => (Outcome::OUTCOME_HELD, String::new()),
            Taken::Refused(why) => {
                tracing::info!(
                    "satellite group: a message from {from}, {} characters, refused: {why}",
                    message.text.chars().count()
                );
                (Outcome::OUTCOME_REFUSED, why)
            }
        };
        let delivery = PeerDelivery {
            sender: MessageField::some(self.group.sender()),
            outcome: outcome.into(),
            reason,
            ..Default::default()
        };
        Response::ok(self.answer(&joined, Rpc::Deliver, &delivery))
    }

    async fn call(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        // The call-back for a caller this satellite doesn't list counts
        // against the tool's limit, so the answer is still back inside the
        // caller's (ADR 0026).
        let started = Instant::now();
        let parts = Parts {
            group_id: request.group_id,
            sender_mac: request.sender_mac,
            body: request.body,
            tag: request.tag,
        };
        let (joined, call): (_, PeerToolCall) = self.open(Rpc::Call, parts)?;
        let caller = request.sender_mac.to_owned();
        let sender = &sender_of(call.sender.as_option());
        let joined = self
            .verified_member(joined, &caller, sender, ctx.peer_addr())
            .await?;
        let from = name_in(&joined, &caller);
        let left = self.group.limits().run.saturating_sub(started.elapsed());
        let output = self
            .run(
                &from,
                sender,
                &call.tool,
                &call.arguments_json,
                tenths(left),
            )
            .await;
        let result = PeerToolResult {
            sender: MessageField::some(self.group.sender()),
            text: output.text,
            is_error: output.is_error,
            ..Default::default()
        };
        Response::ok(self.answer(&joined, Rpc::Call, &result))
    }
}
