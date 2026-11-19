//! The satellite group at run time (ADR 0026): what this satellite knows of
//! the other members that is never saved (each one's mode, when it was last
//! seen, where it called from), the client that calls them, and the changes
//! the dashboard makes that other members must hear of: Add, Remove, and
//! switching the group off.
//!
//! The saved group belongs to the standalone service. Each change here calls
//! the other satellite first and commits after, never while a commit runs.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use buffa::MessageField;
use futures::future::join_all;
use pinna_api::{PeerHello, PeerJoinRequest, PeerMember, PeerMemberList, PeerSender};
use tokio::sync::watch;

use super::client::{Client, Failed, To};
use super::envelope::Rpc;
use super::model::{self, Member, mac_form, same_mac};
use super::persisted::Joined;
use super::view::CandidateState;
use crate::satellites::{Found, Seen, mac_digits};
use crate::settings::Mode;
use crate::standalone::service::group::{
    IN_A_GROUP_THERE, Merging, OFF_HERE, OFF_THERE, next_version,
};
use crate::standalone::service::{Refusal, Service};
use crate::voice::Snapshot;

const ADDING_HERE: &str = "This satellite is adding or joining another one. Try again in a moment.";
const NOT_FOUND: &str =
    "The search doesn't list that satellite now. Reload the page, then try again.";
const MEMBER_ALREADY: &str = "That satellite is in this group already.";
const UNREACHABLE: &str = "The satellite being added didn't answer. Check that it is on and on \
     the same network, then try again.";
const BUSY_THERE: &str =
    "The satellite being added is adding or joining another one. Try again in a moment.";
const NO_RANDOM: &str = "This satellite couldn't draw a key for the group. Try again.";
const REMOVE_ITSELF: &str =
    "To take this satellite out of the group, switch Satellite group off here.";
const STOPPING: &str = "pinnad is shutting down. Try again once it runs.";

/// Every time limit of the calls between members, so tests can pass
/// milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// For each address the search heard to accept a connection: the first
    /// IPv4 address, then the first IPv6 address.
    pub connect: Duration,
    /// A call someone waits on: a tool call ends inside the brain's tool
    /// limit of 10 s (ADR 0017), and an Add inside the dashboard's patience.
    pub call: Duration,
    /// A call nobody waits on: the minute's exchange, the call-back for an
    /// unknown caller, and telling the members of a change.
    pub exchange: Duration,
    /// How long a member that is sent a message waits for its own Brain to
    /// take it, well inside the caller's limit: the Brain answers at once
    /// unless its queue is stuck.
    pub deliver: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(1),
            call: Duration::from_millis(9500),
            exchange: Duration::from_secs(5),
            deliver: Duration::from_secs(2),
        }
    }
}

/// What this satellite heard last from one other satellite of the group.
/// Kept in memory only: none of it is worth a write to flash every minute.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Peer {
    /// From its last signed call or answer.
    pub mode: Option<Mode>,
    /// When it last answered the search, a call, or called.
    pub last_seen: Option<SystemTime>,
    /// Searches in a row that listed it outside this group while it refused
    /// a `GetGroup` as not in this group.
    pub outside: u32,
    /// Where its last signed call came from, and the dashboard port it named.
    /// Nothing reads them yet: they are kept for the messages and control
    /// between members that come next (ADR 0026).
    pub address: Option<IpAddr>,
    pub web_port: Option<u16>,
    /// A call to it failed and that was logged: the next failure isn't,
    /// until it answers again. The exchange calls it every minute.
    failure_logged: bool,
}

/// A change to the group that calls another satellite. One at a time: a
/// satellite adding another refuses to be added itself (ADR 0026).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Adding,
    Joining,
}

/// Holds [`Change`] until dropped.
pub struct Changing<'a>(&'a Group);

impl Drop for Changing<'_> {
    fn drop(&mut self) {
        *self.0.change.lock().unwrap() = None;
    }
}

/// The satellite group's handle, shared by the dashboard's API, the
/// `PeerService` server and the minute's exchange.
pub struct Group {
    service: Arc<Service>,
    /// In lowercase colon form.
    own_mac: String,
    /// This satellite's dashboard port, which every signed call names.
    web_port: u16,
    status: watch::Receiver<Snapshot>,
    satellites: watch::Receiver<Seen>,
    client: Client,
    limits: Limits,
    /// By MAC digits.
    peers: Mutex<HashMap<String, Peer>>,
    change: Mutex<Option<Change>>,
}

impl Group {
    pub fn new(
        service: Arc<Service>,
        own_mac: &str,
        web_port: u16,
        status: watch::Receiver<Snapshot>,
        satellites: watch::Receiver<Seen>,
        limits: Limits,
    ) -> Self {
        Self {
            service,
            own_mac: mac_form(own_mac),
            web_port,
            status,
            satellites,
            client: Client::new(limits.connect),
            limits,
            peers: Mutex::new(HashMap::new()),
            change: Mutex::new(None),
        }
    }

    pub fn service(&self) -> &Service {
        &self.service
    }

    pub fn own_mac(&self) -> &str {
        &self.own_mac
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// This satellite's friendly name now.
    pub fn own_name(&self) -> String {
        self.status.borrow().friendly_name.clone()
    }

    /// What the Brain published last. Never held across an await.
    pub fn status(&self) -> watch::Ref<'_, Snapshot> {
        self.status.borrow()
    }

    /// This satellite's facts, which every signed call and answer carries.
    pub fn sender(&self) -> PeerSender {
        PeerSender {
            web_port: self.web_port.into(),
            mode: crate::api::api_mode(self.status.borrow().mode).into(),
            ..Default::default()
        }
    }

    /// What the search found last.
    pub fn seen(&self) -> Seen {
        self.satellites.borrow().clone()
    }

    /// The search's results as they come.
    pub fn satellites(&self) -> watch::Receiver<Seen> {
        self.satellites.clone()
    }

    /// Start a change, unless another one runs; then that one is returned.
    pub fn begin(&self, change: Change) -> Result<Changing<'_>, Change> {
        let mut current = self.change.lock().unwrap();
        if let Some(busy) = *current {
            return Err(busy);
        }
        *current = Some(change);
        Ok(Changing(self))
    }

    /// What this satellite heard last from the satellite with `mac`.
    pub fn peer(&self, mac: &str) -> Option<Peer> {
        self.peers.lock().unwrap().get(&mac_digits(mac)).cloned()
    }

    fn with_peer<T>(&self, mac: &str, f: impl FnOnce(&mut Peer) -> T) -> T {
        let mut peers = self.peers.lock().unwrap();
        f(peers.entry(mac_digits(mac)).or_default())
    }

    /// A signed call or answer from `mac` verified: keep its facts, and,
    /// for a call, where it came from.
    pub fn heard(&self, mac: &str, sender: &PeerSender, address: Option<IpAddr>) {
        let mode = match sender.mode.as_known() {
            Some(pinna_api::Mode::MODE_STANDALONE) => Mode::Standalone,
            _ => Mode::HomeAssistant,
        };
        self.with_peer(mac, |peer| {
            peer.mode = Some(mode);
            peer.last_seen = Some(SystemTime::now());
            peer.failure_logged = false;
            // Signed with the key: it is in this group.
            peer.outside = 0;
            if let Some(address) = address {
                peer.address = Some(address.to_canonical());
                peer.web_port = u16::try_from(sender.web_port).ok().filter(|p| *p != 0);
            }
        });
    }

    /// The search heard `mac` just now.
    pub fn seen_by_search(&self, mac: &str) {
        self.with_peer(mac, |peer| peer.last_seen = Some(SystemTime::now()));
    }

    /// Count one more search that found `mac` outside the group, or end its
    /// count. Returns the count.
    pub fn outside(&self, mac: &str, outside: bool) -> u32 {
        self.with_peer(mac, |peer| {
            peer.outside = if outside { peer.outside + 1 } else { 0 };
            peer.outside
        })
    }

    /// Whether the member with `mac` refused a `GetGroup` as not in this
    /// group after the latest search listed it outside the group. Only then
    /// does the page offer it as a satellite that left: its record alone
    /// may be older than its Add.
    pub fn answered_outside(&self, mac: &str) -> bool {
        self.peer(mac).is_some_and(|peer| peer.outside > 0)
    }

    /// Merge another member's lists and the names the search heard into the
    /// saved group (see [`Service::exchange_members`]). A member whose entry
    /// the merge raised and that isn't removed was added back somewhere
    /// else: its count of searches outside the group ends, since the search
    /// may still hold its record from before.
    pub async fn merge(
        &self,
        id: &str,
        remote: &[Member],
        heard: &[(String, String)],
    ) -> Result<Merging, Refusal> {
        let before = self.service.group().joined.filter(|j| j.id == id);
        let merging = self
            .service
            .exchange_members(id, remote, heard, &self.own_mac)
            .await?;
        if merging == Merging::Saved
            && let Some(now) = self.service.group().joined.filter(|j| j.id == id)
        {
            let version = |mac: &str| {
                before.as_ref().and_then(|b| {
                    b.members
                        .iter()
                        .find(|m| same_mac(&m.mac, mac))
                        .map(|m| m.version)
                })
            };
            for member in now.members.iter().filter(|m| !m.removed) {
                if version(&member.mac).is_none_or(|v| v < member.version) {
                    self.outside(&member.mac, false);
                }
            }
        }
        Ok(merging)
    }

    /// Forget every count of searches outside the group: what happened
    /// between them isn't known.
    pub fn reset_outside(&self) {
        for peer in self.peers.lock().unwrap().values_mut() {
            peer.outside = 0;
        }
    }

    /// Log a call to `name` that brought nothing, once until it answers
    /// again: the exchange may fail the same way every minute.
    pub fn failed(&self, mac: &str, name: &str, what: &str, failed: &Failed) {
        let first = self.with_peer(mac, |peer| {
            !std::mem::replace(&mut peer.failure_logged, true)
        });
        match failed {
            Failed::BadAnswer if first => {
                tracing::warn!("satellite group: {what} {name}: {failed}");
            }
            _ if first => tracing::info!("satellite group: {what} {name}: {failed}"),
            _ => tracing::debug!("satellite group: {what} {name}: {failed}"),
        }
    }

    /// The member list of `group` as the member `peer_mac` at `to` has it.
    pub async fn get_group(
        &self,
        to: &To,
        group: &Joined,
        peer_mac: &str,
        deadline: Duration,
    ) -> Result<Vec<Member>, Failed> {
        let hello = PeerHello {
            sender: MessageField::some(self.sender()),
            ..Default::default()
        };
        let list: PeerMemberList = self
            .client
            .signed(
                to,
                Rpc::GetGroup,
                group,
                &self.own_mac,
                peer_mac,
                &hello,
                deadline,
            )
            .await?;
        self.heard(
            peer_mac,
            list.sender.as_option().unwrap_or(&PeerSender::default()),
            None,
        );
        Ok(list.members.iter().map(from_wire).collect())
    }

    /// Hand `group`'s member list to the member `peer_mac` at `to`.
    pub async fn put_group(
        &self,
        to: &To,
        group: &Joined,
        peer_mac: &str,
        deadline: Duration,
    ) -> Result<(), Failed> {
        let list = PeerMemberList {
            sender: MessageField::some(self.sender()),
            members: group.members.iter().map(to_wire).collect(),
            ..Default::default()
        };
        let hello: PeerHello = self
            .client
            .signed(
                to,
                Rpc::PutGroup,
                group,
                &self.own_mac,
                peer_mac,
                &list,
                deadline,
            )
            .await?;
        self.heard(
            peer_mac,
            hello.sender.as_option().unwrap_or(&PeerSender::default()),
            None,
        );
        Ok(())
    }

    /// Tell the members with these MACs of `group`'s list, all at once, at
    /// the addresses the search heard. Best effort: a member that misses it
    /// takes it at its next exchange.
    async fn tell(&self, group: &Joined, macs: &[String]) {
        let seen = self.seen();
        join_all(macs.iter().map(|mac| async {
            let name = name_of(group, mac);
            let Some(found) = seen.by_mac(mac) else {
                tracing::info!("satellite group: the search doesn't list {name}, so it hears of the change later");
                return;
            };
            if let Err(e) = self
                .put_group(&To::heard(found), group, mac, self.limits.exchange)
                .await
            {
                self.failed(mac, &name, "telling", &e);
            }
        }))
        .await;
    }

    /// Add the satellite with `mac`, which the search found, to the group,
    /// or start a group of the two. The other satellite takes the group
    /// first; this one saves it only then, and the other members hear of it
    /// after.
    ///
    /// It runs to its end even when the dashboard's request goes away: once
    /// the other satellite took the group, this one must save it or take it
    /// back.
    pub async fn add(self: &Arc<Self>, mac: &str) -> Result<(), Refusal> {
        let group = self.clone();
        let mac = mac.to_owned();
        match tokio::spawn(async move { group.adding(&mac).await }).await {
            Ok(added) => added,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => Err(Refusal::Precondition(STOPPING.into())),
        }
    }

    async fn adding(&self, mac: &str) -> Result<(), Refusal> {
        let adding = self
            .begin(Change::Adding)
            .map_err(|_| Refusal::Precondition(ADDING_HERE.into()))?;
        let saved = self.service.group();
        if !saved.on {
            return Err(Refusal::Precondition(OFF_HERE.into()));
        }
        let seen = self.seen();
        let found = seen
            .by_mac(mac)
            .cloned()
            .ok_or_else(|| Refusal::Precondition(NOT_FOUND.into()))?;
        let view = self
            .service
            .group_view(&self.own_mac, &seen, |mac| self.answered_outside(mac));
        let candidate = view
            .candidates
            .iter()
            .find(|c| same_mac(&c.found.mac, mac))
            .ok_or_else(|| Refusal::Precondition(MEMBER_ALREADY.into()))?;
        if !candidate.addable {
            return Err(Refusal::Precondition(
                match candidate.state {
                    CandidateState::InAnotherGroup => IN_A_GROUP_THERE,
                    _ => OFF_THERE,
                }
                .into(),
            ));
        }
        if let Some(why) = &candidate.name_refusal {
            return Err(Refusal::Precondition(why.clone()));
        }

        let own_name = self.own_name();
        let own = Member {
            mac: self.own_mac.clone(),
            name: own_name.clone(),
            version: 1,
            removed: false,
        };
        let mut added = Member {
            mac: mac_form(&found.mac),
            name: found.friendly_name.trim().to_owned(),
            version: 1,
            removed: false,
        };
        let (mut group, started) = match saved.joined {
            None => (
                Joined::start(own, added.clone()).map_err(|e| {
                    tracing::warn!("satellite group: {e:#}");
                    Refusal::Precondition(NO_RANDOM.into())
                })?,
                true,
            ),
            Some(mut joined) => {
                // A member added back takes the next version of its entry,
                // so it wins over its removal everywhere.
                if let Some(known) = joined.members.iter().find(|m| same_mac(&m.mac, mac)) {
                    added.version = next_version(known.version)?;
                }
                model::heard_name(&mut joined.members, &self.own_mac, &own_name);
                joined.members = model::merge(&joined.members, &[added.clone()]).members;
                (joined, false)
            }
        };

        let request = PeerJoinRequest {
            group_id: group.id.clone(),
            key: group.key.bytes().to_vec(),
            members: group.members.iter().map(to_wire).collect(),
            sender_mac: self.own_mac.clone(),
            web_port: self.web_port.into(),
            ..Default::default()
        };
        let answer = self
            .client
            .join(&To::heard(&found), request, self.limits.call)
            .await
            .map_err(|failed| join_refused(&added.name, failed))?;
        // The satellite's own name for itself, which the search may not
        // have heard yet.
        if let Some(entry) = answer.member.as_option() {
            model::heard_name(&mut group.members, mac, &entry.name);
            model::heard_name(std::slice::from_mut(&mut added), mac, &entry.name);
        }

        let saved = if started {
            self.service.start_group(group.clone(), &self.own_mac).await
        } else {
            self.service
                .merge_members(&group.id, &[added.clone()], &self.own_mac)
                .await
                .map(|_| ())
        };
        if let Err(refusal) = saved {
            self.take_back(&group, &To::heard(&found), &added).await;
            return Err(refusal);
        }
        // Whatever it refused before, it is in the group now.
        if started {
            self.reset_outside();
        } else {
            self.outside(mac, false);
        }
        tracing::info!("added {} to the satellite group", added.name);
        drop(adding);

        // The other members hear of it now; the added one has the list.
        if let Some(now) = self.service.group().joined {
            let others = others(&now, &self.own_mac)
                .filter(|m| !same_mac(m, mac))
                .collect::<Vec<_>>();
            self.tell(&now, &others).await;
        }
        Ok(())
    }

    /// `added` took `group`, but this satellite couldn't save it: it left
    /// the group meanwhile, its switch went off, or the write failed. Tell
    /// `added`, signed with the key, that its entry is removed, so it
    /// forgets the group by the rule every member follows. Otherwise it would
    /// stay in a group whose other members don't list it, and whose answers
    /// to its `GetGroup` keep it there. Best effort.
    async fn take_back(&self, group: &Joined, to: &To, added: &Member) {
        let mut back = group.clone();
        let Some(entry) = back
            .members
            .iter_mut()
            .find(|m| same_mac(&m.mac, &added.mac))
        else {
            return;
        };
        let Ok(version) = next_version(entry.version) else {
            return;
        };
        entry.version = version;
        entry.removed = true;
        match self
            .put_group(to, &back, &added.mac, self.limits.exchange)
            .await
        {
            Ok(()) => tracing::info!(
                "satellite group: this satellite couldn't save the group {} took, so {} was \
                 told to forget it",
                added.name,
                added.name
            ),
            Err(e) => self.failed(&added.mac, &added.name, "taking the group back from", &e),
        }
    }

    /// Remove the member with `mac`, and tell every member, the removed one
    /// too, so it forgets the group. A group left with this satellite alone
    /// ends.
    pub async fn remove(&self, mac: &str) -> Result<(), Refusal> {
        if same_mac(mac, &self.own_mac) {
            return Err(Refusal::Invalid(REMOVE_ITSELF.into()));
        }
        let changed = self.service.remove_member(mac, &self.own_mac).await?;
        let to: Vec<String> = changed
            .members
            .iter()
            .filter(|m| !same_mac(&m.mac, &self.own_mac))
            .filter(|m| !m.removed || same_mac(&m.mac, mac))
            .map(|m| m.mac.clone())
            .collect();
        self.tell(&changed, &to).await;
        Ok(())
    }

    /// Turn *Satellite group* on or off. Off leaves the group: the members
    /// are told first, with this satellite's own entry removed, then it
    /// forgets the group.
    pub async fn set_switch(&self, on: bool) -> Result<(), Refusal> {
        if !on && let Some(mut leaving) = self.service.group().joined {
            let own = leaving
                .members
                .iter_mut()
                .find(|m| same_mac(&m.mac, &self.own_mac));
            if let Some(own) = own
                && let Ok(version) = next_version(own.version)
            {
                own.version = version;
                own.removed = true;
                let to: Vec<String> = others(&leaving, &self.own_mac).collect();
                self.tell(&leaving, &to).await;
            }
        }
        self.service.set_group_switch(on).await?;
        Ok(())
    }
}

/// The MACs of `group`'s members that aren't removed, this satellite aside.
pub fn others<'a>(group: &'a Joined, own_mac: &'a str) -> impl Iterator<Item = String> + 'a {
    group
        .members
        .iter()
        .filter(move |m| !m.removed && !same_mac(&m.mac, own_mac))
        .map(|m| m.mac.clone())
}

/// Whether `mac` is a member of `group` now: listed and not removed.
pub fn is_member(group: &Joined, mac: &str) -> bool {
    group
        .members
        .iter()
        .any(|m| !m.removed && same_mac(&m.mac, mac))
}

/// The name `group` lists for `mac`, for a log line.
fn name_of(group: &Joined, mac: &str) -> String {
    group
        .members
        .iter()
        .find(|m| same_mac(&m.mac, mac))
        .map_or_else(|| mac.to_owned(), |m| m.name.clone())
}

/// Why the satellite being added didn't take the group, for the dashboard.
fn join_refused(name: &str, failed: Failed) -> Refusal {
    match failed {
        Failed::Refused(e) if e.code == connectrpc::ErrorCode::Unavailable => {
            Refusal::Precondition(BUSY_THERE.into())
        }
        // Its refusals are worded for the dashboard that adds it.
        Failed::Refused(e) => Refusal::Precondition(
            e.message
                .unwrap_or_else(|| format!("{name} refused to join the group. Try again.")),
        ),
        Failed::Unreachable(e) => {
            tracing::info!("satellite group: adding {name}: {e}");
            Refusal::Precondition(UNREACHABLE.into())
        }
        Failed::BadAnswer => Refusal::Precondition(UNREACHABLE.into()),
    }
}

/// A member list entry as it travels.
pub fn to_wire(m: &Member) -> PeerMember {
    PeerMember {
        mac: m.mac.clone(),
        name: m.name.clone(),
        version: m.version,
        removed: m.removed,
        ..Default::default()
    }
}

/// A member list entry as it arrived. It goes through `model::merge` before
/// anything reads it.
pub fn from_wire(m: &PeerMember) -> Member {
    Member {
        mac: m.mac.clone(),
        name: m.name.clone(),
        version: m.version,
        removed: m.removed,
    }
}

/// The satellites `seen` lists by MAC, for the members of `group`.
pub fn listed<'a>(seen: &'a Seen, group: &'a Joined, own_mac: &'a str) -> Vec<(String, &'a Found)> {
    others(group, own_mac)
        .filter_map(|mac| seen.by_mac(&mac).map(|found| (mac, found)))
        .collect()
}
