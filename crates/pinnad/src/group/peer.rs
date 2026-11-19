//! `pinna.v1.PeerService` (ADR 0026): the calls other members of the group
//! make, served on the dashboard's listener beside `DeviceService` and
//! `AssistantService` (ADR 0005). Its callers are satellites, not browsers.
//!
//! `Join` is the only unsigned call. Every other call is an envelope whose
//! tag is checked before its body is read, and every answer is signed. A
//! refusal is an unsigned Connect error, so it never makes the caller forget
//! the group.

use std::net::SocketAddr;
use std::sync::Arc;

use buffa::MessageField;
use connectrpc::{ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult};
use pinna_api::{
    PeerEnvelope, PeerHello, PeerJoinRequest, PeerJoinResponse, PeerMemberList, PeerSender,
    PeerService,
};

use super::client::To;
use super::envelope::{self, Parts, Rpc, Unopened};
use super::persisted::Joined;
use super::runtime::{Change, Group, from_wire, is_member, to_wire};
use super::seal::{KEY_LEN, Key};
use crate::standalone::service::Refusal;

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
const NO_MESSAGES: &str = "This satellite doesn't take messages from the group yet.";
const NO_CALLS: &str = "This satellite doesn't run tools for the group yet.";

/// Requests and messages are a member list at most: far below the default
/// 4 MiB, and `Join` is open to anyone on the LAN.
const MAX_BODY: usize = 64 * 1024;

pub struct PeerApi {
    group: Arc<Group>,
}

impl PeerApi {
    pub fn new(group: Arc<Group>) -> Self {
        Self { group }
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
                    let own = self.group.own_mac();
                    if let Err(e) = self
                        .group
                        .service()
                        .merge_members(&joined.id, &list, own)
                        .await
                    {
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
        match self
            .group
            .service()
            .merge_members(&joined.id, &members, self.group.own_mac())
            .await
        {
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
        _ctx: RequestContext,
        _request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        Err(ConnectError::unimplemented(NO_MESSAGES))
    }

    async fn call(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        Err(ConnectError::unimplemented(NO_CALLS))
    }
}
