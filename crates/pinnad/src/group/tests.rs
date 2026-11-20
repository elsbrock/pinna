//! Satellites in a group, in one process: each one's `PeerService` and
//! `AssistantService` on its own port on 127.0.0.1, and a search whose
//! results the test hands it. No test reaches a real satellite on the LAN.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use buffa::{Message as _, MessageField};
use connectrpc::client::{ClientConfig, HttpClient};
use connectrpc::{ConnectError, ErrorCode, RequestContext, Server, ServiceRequest, ServiceResult};
use futures::future::BoxFuture;
use pinna_api::peer_delivery::Outcome;
use pinna_api::{
    AddGroupMemberRequest, AssistantServiceClient, GetSatelliteGroupRequest, PeerDelivery,
    PeerEnvelope, PeerHello, PeerJoinRequest, PeerJoinResponse, PeerMemberList, PeerMessage,
    PeerSender, PeerService, PeerServiceClient, PeerToolCall, PeerToolResult,
    RemoveGroupMemberRequest, SatelliteGroupView, SetSatelliteGroupRequest,
};
use tokio::sync::{mpsc, oneshot, watch};

use super::envelope::{self, Rpc};
use super::model::{Member, same_mac};
use super::peer::{NOT_IN_THIS_GROUP, PeerApi};
use super::runtime::{Change, to_wire};
use super::seal::Key;
use super::{Group, Joined, Limits, exchange};
use crate::assistant_api::AssistantApi;
use crate::auth::Auth;
use crate::mcp::McpServer;
use crate::metrics::Counters;
use crate::satellites::{Found, Seen};
use crate::settings::Mode;
use crate::standalone::AssistLive;
use crate::standalone::service::{Paths, Service};
use crate::tools::message::NO_ONE;
use crate::tools::{Caller, DeviceTools, Surface, ToolKit};
use crate::voice::{ApiCommand, AssistStatus, Input, RunId, Snapshot, Taken};
use pinna_mcp::{ToolOutput, ToolSet};
use serde_json::{Value, json};

fn limits() -> Limits {
    Limits {
        connect: Duration::from_millis(300),
        call: Duration::from_secs(3),
        exchange: Duration::from_secs(2),
        deliver: Duration::from_millis(300),
        run: Duration::from_millis(300),
    }
}

/// The Brain a satellite hands its messages and device commands to: it
/// records each one and answers messages as told, or never.
struct FakeBrain {
    /// The messages it took, as `(from, text)`.
    took: Mutex<Vec<(String, String)>>,
    /// The device commands it got, by name.
    commands: Mutex<Vec<String>>,
    /// Its answer; `None` keeps every message without answering.
    answer: Mutex<Option<Taken>>,
    /// While false, it reads nothing from its queue.
    reading: watch::Sender<bool>,
    /// Its queue.
    inputs: mpsc::Sender<Input>,
}

impl FakeBrain {
    fn start() -> Arc<Self> {
        let (inputs, mut rx) = mpsc::channel(64);
        let (reading, mut reads) = watch::channel(true);
        let brain = Arc::new(Self {
            took: Mutex::default(),
            commands: Mutex::default(),
            answer: Mutex::new(Some(Taken::Played)),
            reading,
            inputs,
        });
        let me = Arc::downgrade(&brain);
        tokio::spawn(async move {
            let mut kept: Vec<oneshot::Sender<Taken>> = Vec::new();
            loop {
                if reads.wait_for(|r| *r).await.is_err() {
                    return;
                }
                let Some(input) = rx.recv().await else { return };
                let Some(me) = me.upgrade() else { return };
                let delivery = match input {
                    Input::Message(delivery) => delivery,
                    Input::Api(cmd) => {
                        me.command(cmd);
                        continue;
                    }
                    _ => continue,
                };
                me.took
                    .lock()
                    .unwrap()
                    .push((delivery.from.clone(), delivery.text.clone()));
                match me.answer.lock().unwrap().clone() {
                    Some(taken) => {
                        let _ = delivery.reply.send(taken);
                    }
                    None => kept.push(delivery.reply),
                }
            }
        });
        brain
    }

    fn answers(&self, taken: Option<Taken>) {
        *self.answer.lock().unwrap() = taken;
    }

    /// Record a device command and answer it as a Brain would, for the
    /// commands the tests use; any other is dropped unanswered.
    fn command(&self, cmd: ApiCommand) {
        let (name, reply, snapshot) = match cmd {
            ApiCommand::SetVolume(volume, reply) => (
                format!("SetVolume({volume})"),
                reply,
                Snapshot {
                    volume,
                    ..Snapshot::default()
                },
            ),
            ApiCommand::CancelForGroup(reply) => {
                ("CancelForGroup".into(), reply, Snapshot::default())
            }
            ApiCommand::StopConversation(reply) => {
                ("StopConversation".into(), reply, Snapshot::default())
            }
            ApiCommand::SetTimer { seconds, reply, .. } => {
                self.commands
                    .lock()
                    .unwrap()
                    .push(format!("SetTimer({seconds})"));
                let _ = reply.send(Ok("local-1".into()));
                return;
            }
            other => {
                self.commands.lock().unwrap().push(format!("{other:?}"));
                return;
            }
        };
        self.commands.lock().unwrap().push(name);
        let _ = reply.send(Ok(snapshot));
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    /// Stop reading the queue: what comes next waits there.
    async fn pause(&self) {
        self.reading.send_replace(false);
        // It may wait for an input already: hand it one to pass over.
        self.inputs.try_send(Input::Privacy(false)).unwrap();
        while self.inputs.capacity() < self.inputs.max_capacity() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    fn took(&self) -> Vec<(String, String)> {
        self.took.lock().unwrap().clone()
    }
}

/// One satellite, served on its own port.
struct Sat {
    name: String,
    mac: String,
    port: u16,
    service: Arc<Service>,
    group: Arc<Group>,
    assistant: AssistantServiceClient<HttpClient>,
    peer: PeerServiceClient<HttpClient>,
    seen: watch::Sender<Seen>,
    status: watch::Sender<Snapshot>,
    brain: Arc<FakeBrain>,
    /// Its `/mcp`, which runs the other members' calls.
    mcp: Arc<McpServer>,
    /// The owner's say on a Join, and its windows (ADR 0029).
    auth: Arc<Auth>,
    counters: Counters,
    _inputs: mpsc::Receiver<Input>,
    _dir: tempfile::TempDir,
}

impl Sat {
    async fn start(n: u8, name: &str) -> Self {
        Self::start_with(n, name, limits()).await
    }

    async fn start_with(n: u8, name: &str, limits: Limits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Standalone, with a checked voice: it takes messages.
        let (status, status_rx) = watch::channel(Snapshot {
            mode: Mode::Standalone,
            friendly_name: name.into(),
            assist: AssistStatus {
                slots: pinna_assist::presets::Slots::all(true),
                ..AssistStatus::default()
            },
            ..Snapshot::default()
        });
        let service = Service::load(
            Paths::beside(&dir.path().join("settings.toml")),
            status_rx.clone(),
        );
        let (seen, seen_rx) = watch::channel(Seen::default());
        let server = Server::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        let mac = format!("02:00:00:00:00:{n:02x}");
        let group = Arc::new(Group::new(
            service.clone(),
            &mac,
            port,
            status_rx.clone(),
            seen_rx.clone(),
            limits,
        ));
        let (inputs, inputs_rx) = mpsc::channel(8);
        let assistant = AssistantApi::new(service.clone(), inputs)
            .with_satellites(seen_rx, mac.clone())
            .with_group(group.clone());
        let brain = FakeBrain::start();
        let counters = Counters::default();
        let mcp = Arc::new(McpServer::new(
            service.clone(),
            brain.inputs.clone(),
            status_rx,
            ToolKit::new(counters.clone()).with_group(group.clone()),
            "0.4.0",
        ));
        let auth = Auth::new(service.clone(), &mac, brain.inputs.clone());
        let router = crate::assistant_api::router(Arc::new(assistant)).add_service(Arc::new(
            PeerApi::new(group.clone())
                .with_auth(auth.clone())
                .with_brain(brain.inputs.clone())
                .with_tools(mcp.clone()),
        ));
        tokio::spawn(async move { server.serve(router).await });
        let url = format!("http://127.0.0.1:{port}");
        let config = || ClientConfig::new(url.parse().unwrap());
        Self {
            name: name.into(),
            mac,
            port,
            service,
            group,
            assistant: AssistantServiceClient::new(HttpClient::plaintext(), config()),
            peer: PeerServiceClient::new(HttpClient::plaintext(), config()),
            seen,
            status,
            brain,
            mcp,
            auth,
            counters,
            _inputs: inputs_rx,
            _dir: dir,
        }
    }

    /// Satellites `1..=n`, each with Satellite group on.
    async fn switched_on(names: &[&str]) -> Vec<Self> {
        let mut sats = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let sat = Self::start(i as u8 + 1, name).await;
            sat.service.set_group_switch(true).await.unwrap();
            sats.push(sat);
        }
        sats
    }

    /// What another satellite's search lists for this one now. Its MAC in
    /// upper case, as some records have it.
    fn found(&self) -> Found {
        Found {
            name: self.name.to_lowercase(),
            host: self.name.to_lowercase(),
            friendly_name: self.name.clone(),
            board: "Echo Dot (2nd Gen)".into(),
            mac: self.mac.to_uppercase(),
            version: "0.4.0".into(),
            web_port: self.port,
            url: String::new(),
            ipv4: vec![Ipv4Addr::LOCALHOST],
            ipv6: Vec::new(),
            group: self.service.group().record(),
            missed: 0,
        }
    }

    /// The next search finds these satellites, as they are.
    fn finds(&self, others: &[&Sat]) {
        self.finds_as(others.iter().map(|o| o.found()).collect());
    }

    /// The next search finds exactly this.
    fn finds_as(&self, found: Vec<Found>) {
        self.seen.send_modify(|seen| {
            seen.searched = true;
            seen.on_network = true;
            seen.search += 1;
            seen.satellites = found;
        });
    }

    fn joined(&self) -> Option<Joined> {
        self.service.group().joined
    }

    /// The group's id, and its members as `(name, removed)` by MAC.
    fn members(&self) -> Vec<(String, bool)> {
        self.joined()
            .map(|j| {
                j.members
                    .iter()
                    .map(|m| (m.mac.clone(), m.removed))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The members not removed, by MAC.
    fn current(&self) -> Vec<String> {
        self.members()
            .into_iter()
            .filter(|(_, removed)| !removed)
            .map(|(mac, _)| mac)
            .collect()
    }

    async fn add(&self, other: &Sat) -> Result<SatelliteGroupView, ConnectError> {
        self.assistant
            .add_group_member(AddGroupMemberRequest {
                mac: other.found().mac,
                ..Default::default()
            })
            .await
            .map(|r| {
                r.into_view()
                    .to_owned_message()
                    .group
                    .into_option()
                    .unwrap()
            })
    }

    async fn remove(&self, other: &Sat) -> Result<SatelliteGroupView, ConnectError> {
        self.assistant
            .remove_group_member(RemoveGroupMemberRequest {
                mac: other.mac.clone(),
                ..Default::default()
            })
            .await
            .map(|r| {
                r.into_view()
                    .to_owned_message()
                    .group
                    .into_option()
                    .unwrap()
            })
    }

    async fn view(&self) -> SatelliteGroupView {
        self.assistant
            .get_satellite_group(GetSatelliteGroupRequest::default())
            .await
            .unwrap()
            .into_view()
            .to_owned_message()
            .group
            .into_option()
            .unwrap()
    }

    /// The minute's exchange after the search the satellite last saw.
    async fn exchange(&self, last: Option<u64>) {
        exchange::once(&self.group, &self.group.seen(), last).await;
    }

    fn search(&self) -> u64 {
        self.seen.borrow().search
    }

    /// A signed call to this satellite, as `from` would make it with
    /// `group`'s key.
    fn sealed(group: &Joined, from: &Sat, rpc: Rpc, body: &impl buffa::Message) -> PeerEnvelope {
        envelope::seal(group, &from.mac, rpc.call(), body)
    }

    /// `from`'s message to this satellite, signed with `group`'s key, and
    /// this satellite's signed answer, opened.
    async fn deliver(&self, group: &Joined, from: &Sat, text: &str) -> PeerDelivery {
        let message = PeerMessage {
            sender: hello(from).sender,
            text: text.into(),
            ..Default::default()
        };
        let answer = self
            .peer
            .deliver(Self::sealed(group, from, Rpc::Deliver, &message))
            .await
            .unwrap()
            .into_view()
            .to_owned_message();
        assert!(same_mac(&answer.sender_mac, &self.mac));
        envelope::open(group, Rpc::Deliver.answer(), (&answer).into()).unwrap()
    }

    /// `from`'s call of `tool` to this satellite, signed with `group`'s
    /// key, and this satellite's signed result, opened.
    async fn called(
        &self,
        group: &Joined,
        from: &Sat,
        tool: &str,
        arguments: &str,
    ) -> PeerToolResult {
        let call = PeerToolCall {
            sender: hello(from).sender,
            tool: tool.into(),
            arguments_json: arguments.into(),
            ..Default::default()
        };
        let answer = self
            .peer
            .call(Self::sealed(group, from, Rpc::Call, &call))
            .await
            .unwrap()
            .into_view()
            .to_owned_message();
        assert!(same_mac(&answer.sender_mac, &self.mac));
        envelope::open(group, Rpc::Call.answer(), (&answer).into()).unwrap()
    }

    /// Switch this satellite's own tool `name` on or off.
    async fn switch_tool(&self, name: &str, on: bool) {
        self.service
            .update(crate::standalone::service::AssistantPatch {
                tools: Some([(name.to_owned(), on)].into()),
                ..Default::default()
            })
            .await
            .unwrap();
    }
}

/// The device tools `sat`'s brain gets, on `surface`.
fn tools_on(sat: &Sat, surface: Surface) -> DeviceTools {
    let caller = match surface {
        Surface::Run => Caller::Run(RunId(1)),
        Surface::Mcp => Caller::Mcp,
    };
    DeviceTools::new(
        &ToolKit::new(crate::metrics::Counters::default()).with_group(sat.group.clone()),
        mpsc::channel(64).0,
        sat.status.subscribe(),
        Arc::new(AssistLive::empty()),
        surface,
        caller,
    )
}

/// Whether `sat`'s brain is offered `send_message`.
async fn offered(sat: &Sat) -> bool {
    tools_on(sat, Surface::Run)
        .specs()
        .await
        .iter()
        .any(|s| s.name == "send_message")
}

/// `sat`'s brain calls `send_message`.
async fn send(sat: &Sat, args: Value) -> ToolOutput {
    tools_on(sat, Surface::Run).call("send_message", args).await
}

/// A delivery's outcome and reason.
fn outcome(delivery: &PeerDelivery) -> (Outcome, &str) {
    (
        delivery.outcome.as_known().unwrap(),
        delivery.reason.as_str(),
    )
}

/// Every satellite's search finds every other one, as they are now.
fn all_find(sats: &[&Sat]) {
    for sat in sats {
        let others: Vec<&Sat> = sats.iter().copied().filter(|o| o.mac != sat.mac).collect();
        sat.finds(&others);
    }
}

fn macs(sats: &[&Sat]) -> Vec<String> {
    let mut macs: Vec<String> = sats.iter().map(|s| s.mac.clone()).collect();
    macs.sort();
    macs
}

fn hello(sat: &Sat) -> PeerHello {
    PeerHello {
        sender: MessageField::some(PeerSender {
            web_port: sat.port.into(),
            mode: pinna_api::Mode::MODE_STANDALONE.into(),
            has_password: sat.group.sender().has_password,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `sat`'s next search finds `found`, and its exchange follows the search
/// `last` saw.
async fn search(sat: &Sat, found: Vec<Found>, last: &mut Option<u64>) {
    sat.finds_as(found);
    sat.exchange(*last).await;
    *last = Some(sat.search());
}

/// The first satellite adds every other one, each one's search finding all.
async fn grouped(sats: &[&Sat]) {
    all_find(sats);
    for other in &sats[1..] {
        sats[0].add(other).await.unwrap();
        all_find(sats);
    }
    for sat in sats {
        assert_eq!(sat.current(), macs(sats), "{}", sat.name);
    }
}

#[tokio::test]
async fn add_starts_a_group_of_the_two_with_one_key() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    all_find(&[a, b]);
    let view = a.add(b).await.unwrap();
    assert!(view.in_group);
    // Each search still lists the other as in no group, from before the
    // Add: neither is offered as a satellite that left.
    assert_eq!(view.candidates, []);
    assert_eq!(b.view().await.candidates, []);
    let e = a.add(b).await.unwrap_err();
    assert_eq!(
        e.message.as_deref(),
        Some("That satellite is in this group already.")
    );
    let names: Vec<(&str, bool)> = view
        .members
        .iter()
        .map(|m| (m.name.as_str(), m.this_satellite))
        .collect();
    assert_eq!(names, [("Hall", true), ("Kitchen", false)]);
    // The satellite added answered Join; its mode isn't known before its
    // first signed answer.
    let (ja, jb) = (a.joined().unwrap(), b.joined().unwrap());
    assert_eq!(ja.id, jb.id);
    assert_eq!(ja.key, jb.key);
    assert_eq!(ja.members, jb.members);
    assert_eq!(
        ja.members,
        [
            Member {
                mac: a.mac.clone(),
                name: "Hall".into(),
                version: 1,
                removed: false
            },
            Member {
                mac: b.mac.clone(),
                name: "Kitchen".into(),
                version: 1,
                removed: false
            },
        ]
    );
    // Both records name the group now.
    assert_eq!(a.found().group, Some(ja.id.clone()));
    assert_eq!(b.found().group, Some(ja.id));
}

#[tokio::test]
async fn a_third_satellite_joins_and_every_member_hears_of_it() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    all_find(&[a, b, c]);
    a.add(b).await.unwrap();
    all_find(&[a, b, c]);
    // B adds C: A hears of it from B, at once.
    b.add(c).await.unwrap();
    for sat in [a, b, c] {
        assert_eq!(sat.current(), macs(&[a, b, c]), "{}", sat.name);
    }
    // A's view has B's mode from B's call, and when it called. C hasn't
    // called A or answered it yet.
    let view = a.view().await;
    let kitchen = view.members.iter().find(|m| m.name == "Kitchen").unwrap();
    assert_eq!(
        kitchen.mode.map(|m| m.as_known()),
        Some(Some(pinna_api::Mode::MODE_STANDALONE))
    );
    assert!(kitchen.last_seen_unix_ms > 0);
    let office = view.members.iter().find(|m| m.name == "Office").unwrap();
    assert_eq!((office.mode, office.last_seen_unix_ms), (None, 0));
    let hall = view.members.iter().find(|m| m.this_satellite).unwrap();
    assert_eq!((hall.mode, hall.last_seen_unix_ms), (None, 0));
    // A's exchange asks C too, and C's signed answer brings its mode.
    a.exchange(None).await;
    let view = a.view().await;
    let office = view.members.iter().find(|m| m.name == "Office").unwrap();
    assert_eq!(
        office.mode.map(|m| m.as_known()),
        Some(Some(pinna_api::Mode::MODE_STANDALONE))
    );
    assert!(office.last_seen_unix_ms > 0);
}

#[tokio::test]
async fn a_member_is_offered_as_left_only_once_it_refused_and_can_be_added_back() {
    use pinna_api::satellite_group_candidate::State;
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    let id = a.joined().unwrap().id;
    // C was reset: in no group, with its switch on.
    c.service.forget_group(&id).await.unwrap();
    let candidates = |view: &SatelliteGroupView| -> Vec<(String, Option<State>, bool)> {
        view.candidates
            .iter()
            .map(|c| {
                (
                    c.satellite.friendly_name.clone(),
                    c.state.as_known(),
                    c.addable,
                )
            })
            .collect()
    };
    // Its record alone doesn't make it one that left.
    a.finds(&[b, c]);
    assert_eq!(candidates(&a.view().await), []);
    // Its refusal after a search that listed it outside the group does.
    let mut last = Some(a.search());
    search(a, vec![b.found(), c.found()], &mut last).await;
    assert_eq!(
        candidates(&a.view().await),
        [("Office".into(), Some(State::STATE_LEFT_THE_GROUP), true)]
    );
    // B's search and exchange count it outside too.
    let mut last_b = Some(b.search());
    search(b, vec![a.found(), c.found()], &mut last_b).await;
    assert_eq!(
        candidates(&b.view().await),
        [("Office".into(), Some(State::STATE_LEFT_THE_GROUP), true)]
    );
    // Added back on A, while both searches still list it as in no group.
    let view = a.add(c).await.unwrap();
    assert_eq!(candidates(&view), []);
    for sat in [a, b, c] {
        assert_eq!(sat.current(), macs(&[a, b, c]), "{}", sat.name);
    }
    // B heard of it through A's list: Office is a member there too, not a
    // satellite to add.
    assert_eq!(candidates(&b.view().await), []);
    let refusal = b.add(c).await.unwrap_err();
    assert_eq!(refusal.code, ErrorCode::FailedPrecondition);
    assert_eq!(
        refusal.message.as_deref(),
        Some("That satellite is in this group already.")
    );
}

#[tokio::test]
async fn add_is_refused_before_the_call_when_the_search_says_no() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office", "Porch"]).await;
    let (a, b, c, d) = (&s[0], &s[1], &s[2], &s[3]);
    let refusal = |e: ConnectError| (e.code, e.message.unwrap_or_default());
    // Not found by the search.
    a.finds(&[]);
    let (code, why) = refusal(a.add(b).await.unwrap_err());
    assert_eq!(code, ErrorCode::FailedPrecondition);
    assert!(why.starts_with("The search doesn't list"), "{why}");

    // Off there: no group key in its record.
    let mut off = b.found();
    off.group = None;
    // In another group.
    let mut other = c.found();
    other.group = Some("ffeeddccbbaa99887766554433221100".into());
    // Its friendly name is this satellite's.
    let mut same_name = d.found();
    same_name.friendly_name = "hall".into();
    a.finds_as(vec![off, other, same_name]);
    let (_, why) = refusal(a.add(b).await.unwrap_err());
    assert!(
        why.starts_with("Satellite group is off on the satellite being added"),
        "{why}"
    );
    let (_, why) = refusal(a.add(c).await.unwrap_err());
    assert!(
        why.starts_with("The satellite being added is in a group already"),
        "{why}"
    );
    let (_, why) = refusal(a.add(d).await.unwrap_err());
    assert_eq!(
        why,
        "It has the same name as this satellite, Hall. Rename it first on its dashboard."
    );

    // Off here.
    a.service.set_group_switch(false).await.unwrap();
    a.finds(&[b]);
    let (_, why) = refusal(a.add(b).await.unwrap_err());
    assert!(
        why.starts_with("Satellite group is off on this satellite"),
        "{why}"
    );
    for sat in [a, b, c, d] {
        assert_eq!(sat.joined(), None);
    }
}

/// A's Add of B, while A's search still lists B as addable. It is refused
/// with a sentence for the dashboard, and changes nothing.
async fn add_refused(a: &Sat, b: &Sat) -> String {
    let mut found = b.found();
    found.group = Some(String::new());
    a.finds_as(vec![found]);
    let e = a.add(b).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::FailedPrecondition, "{e}");
    assert_eq!(a.joined(), None);
    e.message.unwrap()
}

#[tokio::test]
async fn every_join_refusal_reaches_the_dashboard_and_changes_nothing() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);

    // Its switch went off since the search.
    b.service.set_group_switch(false).await.unwrap();
    assert_eq!(
        add_refused(a, b).await,
        "Satellite group is off on the satellite being added. Switch it on there first."
    );
    b.service.set_group_switch(true).await.unwrap();

    // It is adding a satellite itself.
    let adding = b.group.begin(Change::Adding).unwrap();
    assert_eq!(
        add_refused(a, b).await,
        "The satellite being added is adding or joining another one. Try again in a moment."
    );
    drop(adding);

    // It joined another group since the search.
    all_find(&[b, c]);
    b.add(c).await.unwrap();
    assert!(
        add_refused(a, b)
            .await
            .starts_with("The satellite being added is in a group already.")
    );

    // It doesn't answer.
    let gone = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut found = b.found();
    found.web_port = gone.local_addr().unwrap().port();
    found.group = Some(String::new());
    drop(gone);
    a.finds_as(vec![found]);
    let e = a.add(b).await.unwrap_err();
    assert!(e.message.unwrap().contains("didn't answer"));
    assert_eq!(a.joined(), None);

    // The search heard no address for it: it didn't answer either, rather
    // than refuse.
    let mut found = b.found();
    found.ipv4.clear();
    found.group = Some(String::new());
    a.finds_as(vec![found]);
    let e = a.add(b).await.unwrap_err();
    assert!(e.message.unwrap().contains("didn't answer"));
    assert_eq!(a.joined(), None);
}

/// A satellite's `PeerService` that runs `then` once the satellite took a
/// group, before its answer to `Join` leaves.
struct AfterJoin {
    inner: PeerApi,
    then: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
}

#[allow(refining_impl_trait)]
impl PeerService for AfterJoin {
    async fn join(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerJoinRequest>,
    ) -> ServiceResult<PeerJoinResponse> {
        let answer = self.inner.join(ctx, request).await?;
        (self.then)().await;
        Ok(answer)
    }

    async fn get_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        self.inner.get_group(ctx, request).await
    }

    async fn put_group(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        self.inner.put_group(ctx, request).await
    }

    async fn deliver(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        self.inner.deliver(ctx, request).await
    }

    async fn call(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, PeerEnvelope>,
    ) -> ServiceResult<PeerEnvelope> {
        self.inner.call(ctx, request).await
    }
}

/// What a search lists for `sat` when `then` runs once it took a group, before
/// its answer to `Join` leaves.
async fn found_after_join(
    sat: &Sat,
    then: impl Fn() -> BoxFuture<'static, ()> + Send + Sync + 'static,
) -> Found {
    let server = Server::bind("127.0.0.1:0").await.unwrap();
    let mut found = sat.found();
    found.web_port = server.local_addr().unwrap().port();
    let api = AfterJoin {
        inner: PeerApi::new(sat.group.clone()),
        then: Box::new(then),
    };
    let router = connectrpc::Router::new().add_service(Arc::new(api));
    tokio::spawn(async move { server.serve(router).await });
    found
}

#[tokio::test]
async fn a_satellite_added_to_a_group_this_one_couldnt_save_forgets_it() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);

    // A starts a group with C, and its switch goes off while C joins.
    let service = a.service.clone();
    let found = found_after_join(c, move || {
        let service = service.clone();
        Box::pin(async move {
            service.set_group_switch(false).await.unwrap();
        })
    })
    .await;
    a.finds_as(vec![found]);
    let e = a.add(c).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::FailedPrecondition, "{e}");
    // C was told, signed with the key, that its entry is removed.
    assert_eq!((a.joined(), c.joined()), (None, None));
    assert_eq!(c.found().group, Some(String::new()));

    // A adds C to its group with B, and leaves that group while C joins.
    a.service.set_group_switch(true).await.unwrap();
    grouped(&[a, b]).await;
    let id = a.joined().unwrap().id;
    let service = a.service.clone();
    let found = found_after_join(c, move || {
        let (service, id) = (service.clone(), id.clone());
        Box::pin(async move {
            service.forget_group(&id).await.unwrap();
        })
    })
    .await;
    a.finds_as(vec![b.found(), found]);
    let e = a.add(c).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Aborted, "{e}");
    assert_eq!((a.joined(), c.joined()), (None, None));
    assert_eq!(b.current(), macs(&[a, b]));
}

#[tokio::test]
async fn a_signed_call_records_where_it_came_from() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    // Join is unsigned: B knows nothing of A yet.
    assert_eq!(b.group.peer(&a.mac), None);
    let group = a.joined().unwrap();
    b.peer
        .get_group(Sat::sealed(&group, a, Rpc::GetGroup, &hello(a)))
        .await
        .unwrap();
    let peer = b.group.peer(&a.mac).unwrap();
    assert_eq!(peer.address, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert_eq!(peer.web_port, Some(a.port));
    assert_eq!(peer.mode, Some(Mode::Standalone));
    // An IPv4 address that arrived in IPv6 form is kept as IPv4.
    let sender = hello(a).sender.into_option().unwrap();
    b.group.heard(
        &a.mac,
        &sender,
        Some("::ffff:192.168.1.20".parse().unwrap()),
    );
    assert_eq!(
        b.group.peer(&a.mac).unwrap().address,
        Some("192.168.1.20".parse().unwrap())
    );
}

#[tokio::test]
async fn join_takes_only_a_whole_group() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    let both = Joined::start(
        Member {
            mac: a.mac.clone(),
            name: "Hall".into(),
            version: 1,
            removed: false,
        },
        Member {
            mac: b.mac.clone(),
            name: "Kitchen".into(),
            version: 1,
            removed: false,
        },
    )
    .unwrap();
    let request = |group: &Joined, key: Vec<u8>| PeerJoinRequest {
        group_id: group.id.clone(),
        key,
        members: group.members.iter().map(to_wire).collect(),
        sender_mac: a.mac.clone(),
        web_port: a.port.into(),
        ..Default::default()
    };
    let code = |r: Result<_, ConnectError>| r.map(|_| ()).unwrap_err().code;

    let short = request(&both, vec![1; 31]);
    assert_eq!(code(b.peer.join(short).await), ErrorCode::InvalidArgument);
    let without_b = Joined {
        members: both.members[..1].to_vec(),
        ..both.clone()
    };
    let bad = request(&without_b, both.key.bytes().to_vec());
    assert_eq!(code(b.peer.join(bad).await), ErrorCode::InvalidArgument);
    assert_eq!(b.joined(), None);

    let good = request(&both, both.key.bytes().to_vec());
    let own = b.peer.join(good.clone()).await.unwrap();
    let own = own
        .into_view()
        .to_owned_message()
        .member
        .into_option()
        .unwrap();
    assert_eq!(
        (own.mac.as_str(), own.name.as_str()),
        (b.mac.as_str(), "Kitchen")
    );
    assert_eq!(b.joined().unwrap().key, both.key);
    // In a group now.
    assert_eq!(code(b.peer.join(good).await), ErrorCode::FailedPrecondition);
}

#[tokio::test]
async fn two_satellites_that_add_each_other_at_once_both_fail() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    all_find(&[a, b]);
    // Each takes its own Add's turn before either Join arrives.
    let (ab, ba) = tokio::join!(a.group.add(&b.mac), b.group.add(&a.mac));
    for refusal in [ab.unwrap_err(), ba.unwrap_err()] {
        assert!(
            refusal.sentence().ends_with("Try again in a moment."),
            "{refusal}"
        );
    }
    assert_eq!((a.joined(), b.joined()), (None, None));
    // Once one tries again, it works.
    a.add(b).await.unwrap();
    assert_eq!(a.joined().unwrap().id, b.joined().unwrap().id);
}

#[tokio::test]
async fn two_adds_on_two_members_at_once_both_stay() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office", "Porch"]).await;
    let (a, b, c, d) = (&s[0], &s[1], &s[2], &s[3]);
    grouped(&[a, b]).await;
    all_find(&[a, b, c, d]);
    let (ac, bd) = tokio::join!(a.add(c), b.add(d));
    ac.unwrap();
    bd.unwrap();
    // C and D may not know of each other yet; one exchange settles it.
    all_find(&[a, b, c, d]);
    for sat in [a, b, c, d] {
        sat.exchange(None).await;
    }
    for sat in [a, b, c, d] {
        assert_eq!(sat.current(), macs(&[a, b, c, d]), "{}", sat.name);
        assert_eq!(sat.joined().unwrap().members, a.joined().unwrap().members);
    }
}

#[tokio::test]
async fn a_removed_member_hears_it_and_forgets_the_group() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    let view = a.remove(c).await.unwrap();
    assert_eq!(view.members.len(), 2);
    assert_eq!(c.joined(), None);
    // Its switch stays on: it can be added again.
    assert_eq!(c.found().group, Some(String::new()));
    for sat in [a, b] {
        assert_eq!(sat.current(), macs(&[a, b]));
        // The removal stays in the list, at the next version.
        let entry = sat
            .joined()
            .unwrap()
            .members
            .into_iter()
            .find(|m| m.mac == c.mac)
            .unwrap();
        assert!(entry.removed && entry.version == 2, "{entry:?}");
    }

    // Added back, at the version after its removal.
    all_find(&[a, b, c]);
    b.add(c).await.unwrap();
    for sat in [a, b, c] {
        assert_eq!(sat.current(), macs(&[a, b, c]), "{}", sat.name);
        let entry = sat
            .joined()
            .unwrap()
            .members
            .into_iter()
            .find(|m| m.mac == c.mac)
            .unwrap();
        assert_eq!((entry.version, entry.removed), (3, false));
    }

    // This satellite leaves with the switch, not by removing itself.
    let e = a.remove(a).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn a_group_left_with_one_member_ends() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let view = a.remove(b).await.unwrap();
    assert!(!view.in_group);
    assert_eq!((a.joined(), b.joined()), (None, None));
    assert_eq!(a.found().group, Some(String::new()));
}

#[tokio::test]
async fn the_exchange_takes_the_names_the_search_heard_without_a_new_version() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let before = a.joined().unwrap().members;
    let mut renamed = b.found();
    renamed.friendly_name = "Cuisine".into();
    a.status.send_modify(|s| s.friendly_name = "Hallway".into());
    let mut last = Some(a.search());
    search(a, vec![renamed], &mut last).await;
    let after = a.joined().unwrap().members;
    let names: Vec<(&str, u64)> = after.iter().map(|m| (m.name.as_str(), m.version)).collect();
    assert_eq!(
        names,
        [
            ("Hallway", before[0].version),
            ("Cuisine", before[1].version)
        ]
    );
    // B keeps the names its own search heard.
    assert_eq!(b.joined().unwrap().members, before);
}

#[tokio::test]
async fn a_removed_member_that_was_off_learns_it_at_its_next_exchange() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    // C is off while A removes it: A's search doesn't list it.
    a.finds(&[b]);
    a.remove(c).await.unwrap();
    assert!(c.joined().is_some());
    // Back on, it asks the others for their lists, signed with the key.
    all_find(&[a, b, c]);
    c.exchange(None).await;
    assert_eq!(c.joined(), None);
}

#[tokio::test]
async fn switching_off_tells_the_others_first() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    let view = a
        .assistant
        .set_satellite_group(SetSatelliteGroupRequest {
            on: Some(false),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_view()
        .to_owned_message()
        .group
        .into_option()
        .unwrap();
    assert!(!view.on && !view.in_group);
    assert_eq!(a.joined(), None);
    assert_eq!(a.found().group, None);
    for sat in [b, c] {
        assert_eq!(sat.current(), macs(&[b, c]), "{}", sat.name);
    }

    // The last other member left alone ends the group too.
    b.group.set_switch(false).await.unwrap();
    assert_eq!((b.joined(), c.joined()), (None, None));
}

#[tokio::test]
async fn a_member_that_was_reset_never_makes_the_others_forget() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    let id = a.joined().unwrap().id;
    // B was reset: in no group, refusing every call.
    b.service.forget_group(&id).await.unwrap();
    let mut last = Some(a.search());
    for _ in 0..4 {
        search(a, vec![b.found(), c.found()], &mut last).await;
    }
    assert_eq!(a.joined().unwrap().id, id);
    assert_eq!(a.current(), macs(&[a, b, c]));

    // Its refusals of a PutGroup don't either.
    a.remove(c).await.unwrap();
    assert!(a.joined().is_some());
}

#[tokio::test]
async fn a_member_leaves_once_every_other_member_answers_outside_the_group_twice() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let id = a.joined().unwrap().id;
    b.service.forget_group(&id).await.unwrap();

    let mut last = Some(a.search());
    // Once: not yet.
    search(a, vec![b.found()], &mut last).await;
    assert!(a.joined().is_some());
    // Searches that passed unseen start the count again.
    last = last.map(|l| l - 1);
    search(a, vec![b.found()], &mut last).await;
    assert!(a.joined().is_some());
    // Twice in a row: it leaves, and keeps its switch on.
    search(a, vec![b.found()], &mut last).await;
    assert_eq!(a.joined(), None);
    assert_eq!(a.found().group, Some(String::new()));
}

#[tokio::test]
async fn neither_the_record_nor_the_refusal_alone_ends_a_group() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let id = a.joined().unwrap().id;
    let mut last = Some(a.search());

    // A stale record that says B is in no group, while B answers in it.
    let mut stale = b.found();
    stale.group = Some(String::new());
    for _ in 0..3 {
        search(a, vec![stale.clone()], &mut last).await;
    }
    assert!(a.joined().is_some());

    // B refuses, but its record still names the group (resolved shows an
    // old one), or the search didn't hear it this time.
    b.service.forget_group(&id).await.unwrap();
    let mut old = b.found();
    old.group = Some(id.clone());
    for _ in 0..3 {
        search(a, vec![old.clone()], &mut last).await;
    }
    let mut missed = b.found();
    missed.missed = 1;
    for _ in 0..3 {
        search(a, vec![missed.clone()], &mut last).await;
    }
    // Not listed at all: unreachable, not gone.
    for _ in 0..3 {
        search(a, Vec::new(), &mut last).await;
    }
    assert_eq!(a.joined().unwrap().id, id);
}

#[tokio::test]
async fn two_newcomers_put_their_lists_to_each_other_without_looping() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office", "Porch"]).await;
    let (a, b, c, d) = (&s[0], &s[1], &s[2], &s[3]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    // C and D each joined from a member that hadn't heard of the other.
    let with = |sat: &Sat| {
        let mut members = group.members.clone();
        members.push(Member {
            mac: sat.mac.clone(),
            name: sat.name.clone(),
            version: 1,
            removed: false,
        });
        Joined {
            members,
            ..group.clone()
        }
    };
    c.service
        .join_group(with(c), &c.mac, &c.name)
        .await
        .unwrap();
    d.service
        .join_group(with(d), &d.mac, &d.name)
        .await
        .unwrap();
    // Neither search lists the other yet: D calls C back at the address the
    // call came from, on the port it named.
    c.finds(&[]);
    d.finds(&[]);
    let put = |from: &Sat| {
        let list = PeerMemberList {
            sender: hello(from).sender,
            members: from.joined().unwrap().members.iter().map(to_wire).collect(),
            ..Default::default()
        };
        Sat::sealed(&from.joined().unwrap(), from, Rpc::PutGroup, &list)
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        d.peer.put_group(put(c)).await.unwrap();
        c.peer.put_group(put(d)).await.unwrap();
    })
    .await
    .expect("the calls ended");
    assert_eq!(c.current(), macs(&[a, b, c, d]));
    assert_eq!(d.current(), macs(&[a, b, c, d]));
}

#[tokio::test]
async fn a_caller_that_is_still_no_member_after_the_call_back_is_refused() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    // C holds the key, but its own list says it was removed.
    let mut members = group.members.clone();
    members.push(Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 1,
        removed: false,
    });
    c.service
        .join_group(
            Joined {
                members,
                ..group.clone()
            },
            &c.mac,
            "Office",
        )
        .await
        .unwrap();
    // A removed C before C's Add reached it.
    let removed = Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 2,
        removed: true,
    };
    a.service
        .merge_members(&group.id, &[removed], &a.mac)
        .await
        .unwrap();
    let list = PeerMemberList {
        sender: hello(c).sender,
        ..Default::default()
    };
    let call = Sat::sealed(&group, c, Rpc::PutGroup, &list);
    let e = a.peer.put_group(call).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "{e}");
    assert_eq!(a.current(), macs(&[a, b]));
}

#[tokio::test]
async fn a_call_must_be_signed_with_the_key_for_this_group() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    let get = async |call: PeerEnvelope| b.peer.get_group(call).await.map(|_| ()).unwrap_err();

    // In no group.
    let stranger = Joined::start(
        Member {
            mac: a.mac.clone(),
            name: "Hall".into(),
            version: 1,
            removed: false,
        },
        Member {
            mac: b.mac.clone(),
            name: "Kitchen".into(),
            version: 1,
            removed: false,
        },
    )
    .unwrap();
    let e = get(Sat::sealed(&stranger, a, Rpc::GetGroup, &hello(a))).await;
    assert_eq!(e.code, ErrorCode::FailedPrecondition);
    assert_eq!(e.message.as_deref(), Some(NOT_IN_THIS_GROUP));

    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    // Another group's id.
    let e = get(Sat::sealed(&stranger, a, Rpc::GetGroup, &hello(a))).await;
    assert_eq!(e.message.as_deref(), Some(NOT_IN_THIS_GROUP));
    // The right id, the wrong key.
    let forged = Joined {
        key: Key::new([7; 32]),
        ..group.clone()
    };
    let e = get(Sat::sealed(&forged, a, Rpc::GetGroup, &hello(a))).await;
    assert_eq!(e.code, ErrorCode::Unauthenticated, "{e}");
    // A tag made for another sender.
    let mut claimed = Sat::sealed(&group, a, Rpc::GetGroup, &hello(a));
    claimed.sender_mac = "02:00:00:00:00:99".into();
    assert_eq!(get(claimed).await.code, ErrorCode::Unauthenticated);

    // Signed right, from a MAC B doesn't know: a GetGroup needs only the
    // tag.
    let unknown = Sat::start(9, "Porch").await;
    let answer = b
        .peer
        .get_group(Sat::sealed(
            &group,
            &unknown,
            Rpc::GetGroup,
            &hello(&unknown),
        ))
        .await
        .unwrap()
        .into_view()
        .to_owned_message();
    // The answer is signed as B, for this answer only.
    let list: PeerMemberList =
        envelope::open(&group, Rpc::GetGroup.answer(), (&answer).into()).unwrap();
    assert!(same_mac(&answer.sender_mac, &b.mac));
    assert_eq!(list.members.len(), 2);
    assert!(
        envelope::open::<PeerMemberList>(&group, Rpc::PutGroup.call(), (&answer).into()).is_err()
    );
    // Replayed as a call, B's own signed answer is refused: its tag is for
    // an answer.
    let e = a.peer.put_group(answer).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unauthenticated, "{e}");
    assert_eq!(a.current(), macs(&[a, b]));
}

/// A tool's result as `(text, is_error)`.
fn result(result: &PeerToolResult) -> (&str, bool) {
    (result.text.as_str(), result.is_error)
}

#[tokio::test]
async fn a_member_runs_a_tool_from_the_group_as_its_mcp_would() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();

    let volume = b
        .called(&group, a, "set_volume", r#"{"percent": 40}"#)
        .await;
    assert_eq!(result(&volume), ("The volume is 40%.", false));
    // The answer carries Kitchen's facts.
    assert_eq!(
        volume.sender.as_option().unwrap().web_port,
        u32::from(b.port)
    );
    let timer = b
        .called(&group, a, "set_timer", r#"{"seconds": 300, "name": "Tea"}"#)
        .await;
    assert_eq!(
        result(&timer),
        ("Timer \"Tea\" set for 5 minutes (id local-1).", false)
    );
    // No arguments are none.
    let stopped = b.called(&group, a, "cancel_request", "").await;
    assert_eq!(result(&stopped), ("Stopped.", false));
    // A tool that needs no Brain runs too.
    let stations = b.called(&group, a, "list_stations", "{}").await;
    assert!(!stations.is_error, "{}", stations.text);
    assert_eq!(
        b.brain.commands(),
        ["SetVolume(40)", "SetTimer(300)", "CancelForGroup"]
    );
    // Arguments are checked as /mcp checks them.
    let wrong = b
        .called(&group, a, "set_volume", r#"{"percent": "loud"}"#)
        .await;
    assert_eq!(result(&wrong), ("percent must be a whole number.", true));

    // Kitchen's own switch for a tool applies.
    b.switch_tool("set_volume", false).await;
    let off = b
        .called(&group, a, "set_volume", r#"{"percent": 10}"#)
        .await;
    assert_eq!(
        result(&off),
        ("The owner switched set_volume off under Assistant.", true)
    );
    assert_eq!(b.brain.commands().len(), 3);

    // Each call counts as one from the group.
    let text = b.counters.tool_calls_text();
    for line in [
        "pinna_tool_calls_total{tool=\"set_volume\",outcome=\"ok\",via=\"peer\"} 1",
        "pinna_tool_calls_total{tool=\"set_volume\",outcome=\"error\",via=\"peer\"} 2",
        "pinna_tool_calls_total{tool=\"cancel_request\",outcome=\"ok\",via=\"peer\"} 1",
    ] {
        assert!(text.lines().any(|l| l == line), "no {line:?} in\n{text}");
    }
}

#[tokio::test]
async fn a_member_refuses_a_call_with_a_signed_reason_in_the_owners_order() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    let refused = async |tool: &str, arguments: &str| {
        let answer = b.called(&group, a, tool, arguments).await;
        assert!(answer.is_error, "{tool}: {}", answer.text);
        answer.text
    };
    let volume = r#"{"percent": 40}"#;

    // Everything wrong at once: the mode comes first.
    b.status.send_modify(|s| s.mode = Mode::HomeAssistant);
    b.service.set_group_control(false).await.unwrap();
    assert_eq!(
        refused("speak", r#"{"satellite": "Office"}"#).await,
        "Kitchen is in Home Assistant mode, out of reach."
    );
    b.status.send_modify(|s| s.mode = Mode::Standalone);
    assert_eq!(
        refused("set_volume", volume).await,
        "Control from the group is off on Kitchen."
    );
    b.service.set_group_control(true).await.unwrap();
    // Only the tools that act on the satellite.
    for tool in [
        "speak",
        "mute_microphone",
        "clear_notification",
        "switch_assistant",
        "send_message",
        "status",
        "time",
        "weather",
        "search_radio",
    ] {
        assert_eq!(
            refused(tool, "{}").await,
            format!("Kitchen doesn't run {tool} for another satellite.")
        );
    }
    assert_eq!(
        refused("made_up", "{}").await,
        "Kitchen doesn't run that tool for another satellite."
    );
    // A call is never passed on.
    for satellite in [r#""Office""#, r#""Kitchen""#, r#""""#, "null"] {
        assert_eq!(
            refused(
                "set_volume",
                &format!(r#"{{"percent": 40, "satellite": {satellite}}}"#)
            )
            .await,
            "Kitchen doesn't pass calls on to another satellite."
        );
    }
    assert_eq!(
        refused("set_volume", "{percent: 40").await,
        "The call's arguments aren't JSON."
    );
    assert!(b.brain.commands().is_empty(), "the Brain never saw them");
}

#[tokio::test]
async fn a_call_from_the_group_ends_after_the_members_limit_and_may_still_take_effect() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    b.brain.pause().await;

    let slow = b
        .called(&group, a, "set_volume", r#"{"percent": 40}"#)
        .await;

    assert_eq!(
        result(&slow),
        (
            "set_volume didn't finish within 0.3 seconds. It may still take effect.",
            true
        )
    );
    b.brain.reading.send_replace(true);
    for _ in 0..100 {
        if !b.brain.commands().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(b.brain.commands(), ["SetVolume(40)"]);
    let text = b.counters.tool_calls_text();
    let line = "pinna_tool_calls_total{tool=\"set_volume\",outcome=\"error\",via=\"peer\"} 1";
    assert!(text.lines().any(|l| l == line), "no {line:?} in\n{text}");
}

#[tokio::test]
async fn calls_from_the_group_share_the_busy_rule_of_mcp() {
    use tower::ServiceExt;

    let s = Sat::switched_on(&["Hall"]).await;
    let a = &s[0];
    let b = Sat::start_with(
        2,
        "Kitchen",
        Limits {
            run: Duration::from_secs(5),
            ..limits()
        },
    )
    .await;
    b.service.set_group_switch(true).await.unwrap();
    grouped(&[a, &b]).await;
    let group = a.joined().unwrap();
    let volume = r#"{"percent": 40}"#;

    // Two calls to /mcp itself wait for the Brain: a call from the group is
    // a third one.
    let app = axum::Router::new().route(
        crate::mcp::ROUTE,
        crate::mcp::route(b.mcp.clone(), crate::web::Hosts::new(Some("pinna"), &[])),
    );
    let mcp_call = |id: u64| {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": "set_volume", "arguments": {"percent": 40}},
        });
        let request = axum::http::Request::post(crate::mcp::ROUTE)
            .header(axum::http::header::HOST, "pinna.local")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let app = app.clone();
        tokio::spawn(async move { app.oneshot(request).await.unwrap().status() })
    };
    b.brain.pause().await;
    let free = b.brain.inputs.capacity();
    let waiting = [mcp_call(1), mcp_call(2)];
    while b.brain.inputs.capacity() > free - 2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let busy = b.called(&group, a, "set_volume", volume).await;

    assert_eq!(result(&busy), (crate::tools::BUSY, true));
    b.brain.reading.send_replace(true);
    for call in waiting {
        assert_eq!(call.await.unwrap(), axum::http::StatusCode::OK);
    }
    assert_eq!(b.brain.commands(), ["SetVolume(40)", "SetVolume(40)"]);

    // And a call that would leave the Brain's queue too full.
    b.brain.pause().await;
    while b.brain.inputs.capacity() >= crate::tools::BRAIN_RESERVE {
        b.brain.inputs.try_send(Input::Privacy(false)).unwrap();
    }
    let full = b.called(&group, a, "set_volume", volume).await;
    assert_eq!(result(&full), (crate::tools::BUSY, true));
}

#[tokio::test]
async fn a_call_from_a_newcomer_runs_once_its_list_is_merged() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    // B added C, but A hasn't heard of it yet.
    let mut members = group.members.clone();
    members.push(Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 1,
        removed: false,
    });
    c.service
        .join_group(
            Joined {
                members,
                ..group.clone()
            },
            &c.mac,
            "Office",
        )
        .await
        .unwrap();

    let volume = a
        .called(&group, c, "set_volume", r#"{"percent": 40}"#)
        .await;

    assert_eq!(result(&volume), ("The volume is 40%.", false));
    assert_eq!(a.current(), macs(&[a, b, c]));
}

/// A door to `port` on 127.0.0.1 that lets each connection through after
/// `delay`, as a satellite slow to answer.
async fn slow_door(port: u16, delay: Duration) -> u16 {
    let door = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = door.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = door.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let mut outbound = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });
    at
}

#[tokio::test]
async fn a_call_from_a_newcomer_whose_list_is_slow_ends_inside_the_callers_limit() {
    // Hall runs a tool with 1.5 s; the caller waits 2 s.
    let caller = Duration::from_secs(2);
    let a = Sat::start_with(
        1,
        "Hall",
        Limits {
            run: Duration::from_millis(1500),
            ..limits()
        },
    )
    .await;
    let b = Sat::start(2, "Kitchen").await;
    let c = Sat::start(3, "Office").await;
    let d = Sat::start(4, "Porch").await;
    for sat in [&a, &b, &c, &d] {
        sat.service.set_group_switch(true).await.unwrap();
    }
    grouped(&[&a, &b]).await;
    let group = a.joined().unwrap();
    // Kitchen added Office and Porch, but Hall hasn't heard of them yet.
    for sat in [&c, &d] {
        let mut members = group.members.clone();
        members.push(Member {
            mac: sat.mac.clone(),
            name: sat.name.clone(),
            version: 1,
            removed: false,
        });
        sat.service
            .join_group(
                Joined {
                    members,
                    ..group.clone()
                },
                &sat.mac,
                &sat.name,
            )
            .await
            .unwrap();
    }
    // Hall's call-back for the caller's list goes through a slow door.
    let ask = async |from: &Sat, delay: Duration| {
        let call = PeerToolCall {
            sender: MessageField::some(PeerSender {
                web_port: slow_door(from.port, delay).await.into(),
                mode: pinna_api::Mode::MODE_STANDALONE.into(),
                ..Default::default()
            }),
            tool: "set_volume".into(),
            arguments_json: r#"{"percent": 40}"#.into(),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let answer = a
            .peer
            .call(Sat::sealed(&group, from, Rpc::Call, &call))
            .await
            .unwrap()
            .into_view()
            .to_owned_message();
        let result: PeerToolResult =
            envelope::open(&group, Rpc::Call.answer(), (&answer).into()).unwrap();
        (result, started.elapsed())
    };

    // The list takes 0.8 s: the tool gets the rest of Hall's limit.
    a.brain.pause().await;
    let (slow, took) = ask(&c, Duration::from_millis(800)).await;
    assert!(took < caller, "{took:?}");
    assert!(slow.is_error, "{}", slow.text);
    assert!(
        slow.text.starts_with("set_volume didn't finish within 0.")
            && slow.text.ends_with(" seconds. It may still take effect."),
        "{}",
        slow.text
    );
    assert!(a.current().contains(&c.mac));
    a.brain.reading.send_replace(true);
    for _ in 0..100 {
        if !a.brain.commands().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(a.brain.commands(), ["SetVolume(40)"]);

    // The list takes longer than Hall's limit: the tool isn't run.
    let (late, took) = ask(&d, Duration::from_millis(1600)).await;
    assert!(took < caller, "{took:?}");
    assert_eq!(
        result(&late),
        (
            "Hall didn't run set_volume: checking that Porch is in the group took too long. \
             Try again.",
            true
        )
    );
    assert!(a.current().contains(&d.mac));
    assert_eq!(a.brain.commands(), ["SetVolume(40)"]);
}

#[tokio::test]
async fn a_call_from_no_member_or_not_signed_for_a_call_is_refused() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    let call = |from: &Sat| PeerToolCall {
        sender: hello(from).sender,
        tool: "set_volume".into(),
        arguments_json: r#"{"percent": 40}"#.into(),
        ..Default::default()
    };
    // C holds the key, but A's list says it was removed.
    let mut members = group.members.clone();
    members.push(Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 1,
        removed: false,
    });
    c.service
        .join_group(
            Joined {
                members,
                ..group.clone()
            },
            &c.mac,
            "Office",
        )
        .await
        .unwrap();
    let removed = Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 2,
        removed: true,
    };
    a.service
        .merge_members(&group.id, &[removed], &a.mac)
        .await
        .unwrap();
    let e = a
        .peer
        .call(Sat::sealed(&group, c, Rpc::Call, &call(c)))
        .await
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "{e}");

    let forged = Joined {
        key: Key::new([7; 32]),
        ..group.clone()
    };
    for sealed in [
        Sat::sealed(&forged, b, Rpc::Call, &call(b)),
        // Signed right, but for another call.
        Sat::sealed(&group, b, Rpc::Deliver, &call(b)),
    ] {
        let e = a.peer.call(sealed).await.map(|_| ()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unauthenticated, "{e}");
    }
    assert!(a.brain.commands().is_empty());
}

#[tokio::test]
async fn deliver_answers_signed_with_what_the_brain_did() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();

    let played = b.deliver(&group, a, "  dinner is ready.  ").await;
    assert_eq!(outcome(&played), (Outcome::OUTCOME_PLAYED, ""));
    // The answer carries B's facts; the Brain got the text trimmed, from
    // the sender as B's list names it.
    assert_eq!(
        played.sender.as_option().unwrap().web_port,
        u32::from(b.port)
    );
    assert_eq!(b.brain.took(), [("Hall".into(), "dinner is ready.".into())]);

    b.brain.answers(Some(Taken::Held));
    let held = b.deliver(&group, a, "call me.").await;
    assert_eq!(outcome(&held), (Outcome::OUTCOME_HELD, ""));
    // A signed call is heard like any other.
    assert_eq!(b.group.peer(&a.mac).unwrap().mode, Some(Mode::Standalone));
}

#[tokio::test]
async fn deliver_refuses_with_a_signed_reason_in_the_owners_order() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    let refused = async |text: &str| {
        let delivery = b.deliver(&group, a, text).await;
        let (outcome, reason) = outcome(&delivery);
        assert_eq!(outcome, Outcome::OUTCOME_REFUSED, "{reason}");
        reason.to_owned()
    };

    // Everything wrong at once: the mode comes first.
    b.status.send_modify(|s| {
        s.mode = Mode::HomeAssistant;
        s.settings.group_messages = Some(false);
        s.assist.slots.tts = false;
    });
    assert_eq!(
        refused("").await,
        "Kitchen is in Home Assistant mode and takes no messages."
    );
    b.status.send_modify(|s| s.mode = Mode::Standalone);
    assert_eq!(
        refused("").await,
        "Kitchen has Group messages switched off."
    );
    b.status
        .send_modify(|s| s.settings.group_messages = Some(true));
    assert_eq!(
        refused("").await,
        "Kitchen can't say messages: its text-to-speech hasn't passed its check."
    );
    b.status.send_modify(|s| s.assist.slots.tts = true);
    assert_eq!(refused(" \n ").await, "The message is empty.");
    assert_eq!(
        refused(&"ä".repeat(501)).await,
        "A message is at most 500 characters."
    );
    assert!(b.brain.took().is_empty(), "the Brain never saw them");
    // 500 are fine.
    let fine = b.deliver(&group, a, &"ä".repeat(500)).await;
    assert_eq!(outcome(&fine).0, Outcome::OUTCOME_PLAYED);

    // A Brain that doesn't answer in time.
    b.brain.answers(None);
    assert_eq!(
        refused("call me.").await,
        "Kitchen didn't take the message in time. Try again in a moment."
    );
    // A Brain whose queue is nearly full: the message never waits for room.
    b.brain.answers(Some(Taken::Played));
    b.brain.reading.send_replace(false);
    while b.brain.inputs.capacity() >= crate::tools::BRAIN_RESERVE {
        b.brain.inputs.try_send(Input::Privacy(false)).unwrap();
        tokio::task::yield_now().await;
    }
    let took = b.brain.took().len();
    assert_eq!(
        refused("call me.").await,
        "Kitchen is busy. Try again in a moment."
    );
    b.brain.reading.send_replace(true);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(b.brain.took().len(), took, "the busy one never queued");
    // And the Brain's own refusal comes back as it is.
    b.brain
        .answers(Some(Taken::Refused("Kitchen is resting.".into())));
    assert_eq!(refused("call me.").await, "Kitchen is resting.");
}

#[tokio::test]
async fn a_message_from_a_newcomer_is_taken_once_its_list_is_merged() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    // B added C, but A hasn't heard of it yet.
    let mut members = group.members.clone();
    members.push(Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 1,
        removed: false,
    });
    c.service
        .join_group(
            Joined {
                members,
                ..group.clone()
            },
            &c.mac,
            "Office",
        )
        .await
        .unwrap();
    let delivery = a.deliver(&group, c, "dinner is ready.").await;
    assert_eq!(outcome(&delivery).0, Outcome::OUTCOME_PLAYED);
    assert_eq!(a.current(), macs(&[a, b, c]));
    assert_eq!(
        a.brain.took(),
        [("Office".into(), "dinner is ready.".into())]
    );
}

#[tokio::test]
async fn a_message_from_a_satellite_still_no_member_is_refused() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    // C holds the key, but its own list says it was removed.
    let mut members = group.members.clone();
    members.push(Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 1,
        removed: false,
    });
    c.service
        .join_group(
            Joined {
                members,
                ..group.clone()
            },
            &c.mac,
            "Office",
        )
        .await
        .unwrap();
    let removed = Member {
        mac: c.mac.clone(),
        name: "Office".into(),
        version: 2,
        removed: true,
    };
    a.service
        .merge_members(&group.id, &[removed], &a.mac)
        .await
        .unwrap();
    let message = PeerMessage {
        sender: hello(c).sender,
        text: "dinner is ready.".into(),
        ..Default::default()
    };
    let e = a
        .peer
        .deliver(Sat::sealed(&group, c, Rpc::Deliver, &message))
        .await
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "{e}");
    assert!(a.brain.took().is_empty());
}

#[tokio::test]
async fn a_message_must_be_signed_with_the_key_for_a_message() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    let message = PeerMessage {
        sender: hello(a).sender,
        text: "dinner is ready.".into(),
        ..Default::default()
    };
    let forged = Joined {
        key: Key::new([7; 32]),
        ..group.clone()
    };
    for call in [
        Sat::sealed(&forged, a, Rpc::Deliver, &message),
        // Signed right, but for another call.
        Sat::sealed(&group, a, Rpc::PutGroup, &message),
    ] {
        let e = b.peer.deliver(call).await.map(|_| ()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unauthenticated, "{e}");
    }
    assert!(b.brain.took().is_empty());
}

#[tokio::test]
async fn an_answer_from_another_satellite_counts_as_no_answer() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    let group = a.joined().unwrap();
    // B is gone, and C took its address: C's signed answer is C's, not B's.
    b.service.forget_group(&group.id).await.unwrap();
    let mut moved = b.found();
    moved.web_port = c.port;
    moved.group = Some(String::new());
    let mut last = Some(a.search());
    for _ in 0..3 {
        search(a, vec![moved.clone(), c.found()], &mut last).await;
    }
    assert_eq!(a.joined().unwrap().id, group.id);
    assert_eq!(a.current(), macs(&[a, b, c]));
    assert_eq!(a.group.peer(&b.mac).unwrap().outside, 0);
}

thread_local! {
    static CAPTURED: std::cell::RefCell<Option<Arc<Mutex<Vec<u8>>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Writes a log line into the buffer of the thread that logs it, if it has
/// one.
struct ThreadBuffer;

impl std::io::Write for ThreadBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CAPTURED.with(|captured| {
            if let Some(lines) = &*captured.borrow() {
                lines.lock().unwrap().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every log line pinnad writes on this thread from now on. The subscriber
/// is the process's global one: tracing caches whether a log line is wanted
/// across threads, so a subscriber for one thread alone misses lines that
/// another test's thread reached first.
pub(crate) fn capture_logs() -> Arc<Mutex<Vec<u8>>> {
    static GLOBAL: std::sync::Once = std::sync::Once::new();
    GLOBAL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("pinnad=trace")
            .with_writer(|| ThreadBuffer)
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    let lines = Arc::new(Mutex::new(Vec::new()));
    CAPTURED.with(|captured| *captured.borrow_mut() = Some(lines.clone()));
    lines
}

/// A message's text that must never show outside the message itself.
const SECRET_TEXT: &str = "the spare key is under the third flowerpot";

#[tokio::test]
async fn neither_the_key_nor_a_message_leaves_in_a_view_an_answer_or_a_log_line() {
    let captured = capture_logs();

    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    all_find(&[a, b, c]);
    let mut texts = vec![format!("{:?}", a.add(b).await.unwrap())];
    all_find(&[a, b, c]);
    texts.push(format!("{:?}", b.add(c).await.unwrap()));
    let group = a.joined().unwrap();
    let forms = [
        group.key.to_base64(),
        group
            .key
            .bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        format!("{:?}", group.key.bytes()),
    ];

    all_find(&[a, b, c]);
    for sat in [a, b, c] {
        sat.exchange(None).await;
        let view = sat.view().await;
        texts.push(format!("{view:?}"));
        texts.push(String::from_utf8_lossy(&view.encode_to_vec()).into_owned());
        texts.push(format!(
            "{:?}",
            sat.service.group_view(&sat.mac, &sat.group.seen(), |mac| {
                sat.group.answered_outside(mac)
            })
        ));
    }
    let answer = b
        .peer
        .get_group(Sat::sealed(&group, a, Rpc::GetGroup, &hello(a)))
        .await
        .unwrap()
        .into_view()
        .to_owned_message();
    texts.push(format!("{answer:?}"));
    texts.push(String::from_utf8_lossy(&answer.encode_to_vec()).into_owned());
    // A message, taken and refused: its sender and length only.
    let played = b.deliver(&group, a, SECRET_TEXT).await;
    b.status
        .send_modify(|s| s.settings.group_messages = Some(false));
    let refused = b.deliver(&group, a, SECRET_TEXT).await;
    for answer in [played, refused] {
        texts.push(format!("{answer:?}"));
        texts.push(String::from_utf8_lossy(&answer.encode_to_vec()).into_owned());
    }
    // And through the brain's tool.
    let sent = send(a, json!({"to": "Kitchen", "text": SECRET_TEXT})).await;
    assert!(sent.text.starts_with("Kitchen: refused"), "{sent:?}");
    texts.push(format!("{sent:?}"));
    a.remove(c).await.unwrap();
    a.group.set_switch(false).await.unwrap();

    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    assert!(
        logs.contains("added Kitchen to the satellite group"),
        "{logs}"
    );
    assert!(
        logs.contains("a message from Hall, 42 characters, refused"),
        "{logs}"
    );
    assert!(
        logs.contains("a message, 42 characters. Kitchen: refused"),
        "{logs}"
    );
    texts.push(logs);
    for text in &texts {
        for form in &forms {
            assert!(!text.contains(form.as_str()), "{form} in {text}");
        }
        assert!(!text.contains("flowerpot"), "the message's text in {text}");
    }
    // The raw bytes neither.
    let answer_bytes = answer.encode_to_vec();
    assert!(
        !answer_bytes
            .windows(32)
            .any(|w| w == group.key.bytes().as_slice())
    );
}

#[tokio::test]
async fn send_message_is_offered_only_with_another_member_in_standalone_mode() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    assert!(!offered(a).await, "in no group");
    grouped(&[a, b]).await;
    // Kitchen's mode isn't known before its first signed answer.
    assert!(!offered(a).await);
    let out = send(a, json!({"to": "Kitchen", "text": "dinner is ready."})).await;
    assert_eq!(out, ToolOutput::error(NO_ONE));

    b.status.send_modify(|s| s.mode = Mode::HomeAssistant);
    a.exchange(None).await;
    assert_eq!(
        a.group.peer(&b.mac).unwrap().mode,
        Some(Mode::HomeAssistant)
    );
    assert!(!offered(a).await);

    b.status.send_modify(|s| s.mode = Mode::Standalone);
    a.exchange(None).await;
    assert!(offered(a).await);
    // Never on /mcp.
    let mcp = tools_on(a, Surface::Mcp);
    assert!(!mcp.specs().await.iter().any(|s| s.name == "send_message"));
    let out = mcp
        .call("send_message", json!({"to": "Kitchen", "text": "hi"}))
        .await;
    assert_eq!(out, ToolOutput::error("Unknown tool send_message."));
    assert!(b.brain.took().is_empty());
}

#[tokio::test]
async fn send_message_to_one_member_says_it_there() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    a.exchange(None).await;

    let out = send(a, json!({"to": " kitchen ", "text": " dinner is ready. "})).await;
    assert_eq!(out, ToolOutput::ok("Kitchen: played now."));
    assert_eq!(b.brain.took(), [("Hall".into(), "dinner is ready.".into())]);
    assert!(c.brain.took().is_empty());

    // Once the search no longer lists it, it isn't called.
    a.finds(&[c]);
    let out = send(a, json!({"to": "Kitchen", "text": "call me."})).await;
    assert_eq!(out, ToolOutput::ok("Kitchen: unreachable."));
    assert_eq!(b.brain.took().len(), 1);
}

#[tokio::test]
async fn send_message_to_all_says_what_each_member_did() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office", "Porch", "Attic", "Cellar"]).await;
    let all: Vec<&Sat> = s.iter().collect();
    let (a, b, c, d, e, f) = (&s[0], &s[1], &s[2], &s[3], &s[4], &s[5]);
    grouped(&all).await;
    c.brain.answers(Some(Taken::Held));
    d.status
        .send_modify(|s| s.settings.group_messages = Some(false));
    e.status.send_modify(|s| s.mode = Mode::HomeAssistant);
    a.exchange(None).await;
    // The search lists Cellar without an address.
    let mut nowhere = f.found();
    nowhere.ipv4.clear();
    a.finds_as(vec![b.found(), c.found(), d.found(), e.found(), nowhere]);

    let out = send(a, json!({"to": "All", "text": "dinner is ready."})).await;
    assert_eq!(
        out,
        ToolOutput::ok(
            "Kitchen: played now.\n\
             Office: held until it is free.\n\
             Porch: refused: Porch has Group messages switched off.\n\
             Attic is in Home Assistant mode, out of reach.\n\
             Cellar: unreachable."
        )
    );
    assert_eq!(b.brain.took().len(), 1);
    assert_eq!(c.brain.took().len(), 1);
    for sat in [d, e, f] {
        assert!(sat.brain.took().is_empty(), "{}", sat.name);
    }
}

#[tokio::test]
async fn send_message_refuses_an_unknown_name_and_a_long_text() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    a.exchange(None).await;
    let out = send(a, json!({"to": "Garage", "text": "dinner is ready."})).await;
    assert_eq!(
        out,
        ToolOutput::error(
            "No satellite of this one's group is named Garage. Its other members are Kitchen, \
             Office."
        )
    );
    let out = send(a, json!({"to": " hall ", "text": "dinner is ready."})).await;
    assert_eq!(
        out,
        ToolOutput::error(
            "Hall is this satellite, and it can't send a message to itself. The other members \
             of its group are Kitchen, Office."
        )
    );
    let out = send(a, json!({"to": "Kitchen", "text": "a".repeat(501)})).await;
    assert_eq!(
        out,
        ToolOutput::error("The text to say is at most 500 characters.")
    );
    let out = send(a, json!({"to": "Kitchen"})).await;
    assert_eq!(out, ToolOutput::error("send_message needs text."));
    let out = send(a, json!({"to": " ", "text": "dinner is ready."})).await;
    assert_eq!(
        out,
        ToolOutput::error("send_message needs to: a satellite's name, or all.")
    );
    assert!(b.brain.took().is_empty() && c.brain.took().is_empty());
}

#[tokio::test]
async fn send_message_names_two_members_that_share_a_name_by_host_name() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    // Office was renamed Kitchen, and the search heard it.
    let mut renamed = c.found();
    renamed.friendly_name = "Kitchen".into();
    a.finds_as(vec![b.found(), renamed]);
    a.exchange(None).await;
    let out = send(a, json!({"to": "Kitchen", "text": "dinner is ready."})).await;
    assert_eq!(
        out,
        ToolOutput::error(
            "2 satellites of this one's group are named Kitchen: kitchen, office. Rename one \
             on its dashboard first."
        )
    );
    assert!(b.brain.took().is_empty() && c.brain.took().is_empty());
}

#[tokio::test]
async fn send_message_names_a_member_that_shares_this_ones_name_by_host_name() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    a.status.send_modify(|s| s.name = "hall".into());
    // Kitchen was renamed Hall, and the search heard it.
    let mut renamed = b.found();
    renamed.friendly_name = "Hall".into();
    a.finds_as(vec![renamed]);
    a.exchange(None).await;
    let out = send(a, json!({"to": "Hall", "text": "dinner is ready."})).await;
    assert_eq!(
        out,
        ToolOutput::error(
            "2 satellites of this one's group are named Hall: hall, kitchen. Rename one on its \
             dashboard first."
        )
    );
    assert!(b.brain.took().is_empty());
}

#[tokio::test]
async fn the_request_context_names_the_other_members_with_their_modes() {
    use pinna_assist::prompt::{GroupMember, MemberMode};

    let s = Sat::switched_on(&["Hall", "Kitchen", "Office", "Porch"]).await;
    let all: Vec<&Sat> = s.iter().collect();
    let (a, c, d) = (&s[0], &s[2], &s[3]);
    assert!(crate::standalone::spawn::group_context(Some(&a.group)).is_empty());
    grouped(&all).await;
    // Kitchen and Office answered the news of Porch's Add; Porch only took
    // its Join, which is unsigned.
    c.status.send_modify(|s| s.mode = Mode::HomeAssistant);
    a.group.heard(
        &c.mac,
        &c.group.sender(),
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    );
    assert_eq!(a.group.peer(&d.mac).and_then(|p| p.mode), None);
    let member = |name: &str, mode| GroupMember {
        name: name.into(),
        mode,
    };
    assert_eq!(
        crate::standalone::spawn::group_context(Some(&a.group)),
        [
            member("Kitchen", Some(MemberMode::Standalone)),
            member("Office", Some(MemberMode::HomeAssistant)),
            member("Porch", None),
        ]
    );
    assert!(crate::standalone::spawn::group_context(None).is_empty());
}

/// `sat`'s brain's tools with its owner's `switches`, over a queue the test
/// reads, and the counters they count in.
fn brain(sat: &Sat, switches: &[(&str, bool)]) -> (DeviceTools, mpsc::Receiver<Input>, Counters) {
    let (inputs, rx) = mpsc::channel(64);
    let counters = Counters::default();
    let mut live = AssistLive::empty();
    live.switches = switches
        .iter()
        .map(|&(name, on)| (name.to_owned(), on))
        .collect();
    let tools = DeviceTools::new(
        &ToolKit::new(counters.clone()).with_group(sat.group.clone()),
        inputs,
        sat.status.subscribe(),
        Arc::new(live),
        Surface::Run,
        Caller::Run(RunId(1)),
    );
    (tools, rx, counters)
}

/// The tools that act on the satellite, which another member runs.
const ON_THE_SATELLITE: [&str; 13] = [
    "set_volume",
    "change_volume",
    "set_reply_volume",
    "ring_light",
    "play_url",
    "pause_media",
    "resume_media",
    "stop_media",
    "play_station",
    "list_stations",
    "set_timer",
    "list_timers",
    "cancel_timer",
];

/// The specs on `tools`, by name.
async fn specs(tools: &DeviceTools) -> std::collections::BTreeMap<String, pinna_mcp::ToolSpec> {
    tools
        .specs()
        .await
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect()
}

#[tokio::test]
async fn the_brain_names_another_member_only_in_a_group_and_mcp_stays_as_it_is() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    let mcp = async || serde_json::to_string(&tools_on(a, Surface::Mcp).specs().await).unwrap();
    let alone = mcp().await;
    let plain = specs(&tools_on(a, Surface::Run)).await;
    let takes_satellite = |specs: &std::collections::BTreeMap<String, pinna_mcp::ToolSpec>| {
        specs
            .values()
            .filter(|s| s.input_schema["properties"].get("satellite").is_some())
            .map(|s| s.name.clone())
            .collect::<Vec<_>>()
    };
    assert!(takes_satellite(&plain).is_empty());
    assert!(!plain.contains_key("cancel_request"));

    grouped(&[a, b, c]).await;
    a.exchange(None).await;

    let in_group = specs(&tools_on(a, Surface::Run)).await;
    let mut expected: Vec<String> = ON_THE_SATELLITE
        .iter()
        .chain(&["cancel_request"])
        .map(|n| (*n).to_owned())
        .collect();
    expected.sort();
    assert_eq!(takes_satellite(&in_group), expected);
    for name in ON_THE_SATELLITE {
        let schema = &in_group[name].input_schema;
        // Optional, and the rest of the tool as it was.
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("satellite"))
        );
        let mut without = schema.clone();
        without["properties"]
            .as_object_mut()
            .unwrap()
            .remove("satellite");
        assert_eq!(without, plain[name].input_schema, "{name}");
        assert_eq!(in_group[name].description, plain[name].description);
    }
    let cancel = &in_group["cancel_request"];
    assert_eq!(cancel.input_schema["required"], json!(["satellite"]));
    assert!(
        cancel.description.contains("another satellite"),
        "{cancel:?}"
    );
    // One tool per action, whatever the number of members: the brain's
    // limit of 64 holds.
    let mut names: Vec<&String> = in_group.keys().collect();
    names.retain(|n| !plain.contains_key(*n));
    assert_eq!(names, ["cancel_request", "send_message"]);

    // /mcp's tools are byte for byte what they were, in or out of a group.
    assert_eq!(mcp().await, alone);
    assert!(!alone.contains(r#""satellite""#), "{alone}");

    // With no other member in standalone mode, the brain is as before.
    for sat in [b, c] {
        sat.status.send_modify(|s| s.mode = Mode::HomeAssistant);
    }
    a.exchange(None).await;
    assert_eq!(specs(&tools_on(a, Surface::Run)).await, plain);
    assert_eq!(mcp().await, alone);
}

#[tokio::test]
async fn a_tool_switched_off_here_is_offered_for_no_satellite() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    a.exchange(None).await;
    let (tools, mut rx, _) = brain(a, &[("set_volume", false), ("cancel_request", false)]);

    let offered = specs(&tools).await;
    assert!(!offered.contains_key("set_volume"));
    assert!(!offered.contains_key("cancel_request"));
    assert!(offered.contains_key("change_volume"));

    for (tool, args) in [
        ("set_volume", json!({"percent": 40, "satellite": "Kitchen"})),
        ("set_volume", json!({"percent": 40})),
        ("cancel_request", json!({"satellite": "Kitchen"})),
    ] {
        assert_eq!(
            tools.call(tool, args).await,
            ToolOutput::error(format!("The owner switched {tool} off under Assistant."))
        );
    }
    assert!(b.brain.commands().is_empty());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn the_brain_runs_a_tool_on_the_member_it_names() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    a.exchange(None).await;
    let (tools, mut rx, counters) = brain(a, &[]);

    let out = tools
        .call(
            "set_volume",
            json!({"percent": 40, "satellite": " kitchen "}),
        )
        .await;
    assert_eq!(out, ToolOutput::ok("Kitchen: The volume is 40%."));
    // A timer set there rings there.
    let out = tools
        .call(
            "set_timer",
            json!({"seconds": 300, "name": "Tea", "satellite": "Kitchen"}),
        )
        .await;
    assert_eq!(
        out,
        ToolOutput::ok("Kitchen: Timer \"Tea\" set for 5 minutes (id local-1).")
    );
    let out = tools
        .call("cancel_request", json!({"satellite": "Kitchen"}))
        .await;
    assert_eq!(out, ToolOutput::ok("Kitchen: Stopped."));
    // The member got the calls without `satellite`, or it would have
    // refused them.
    assert_eq!(
        b.brain.commands(),
        ["SetVolume(40)", "SetTimer(300)", "CancelForGroup"]
    );
    assert!(c.brain.commands().is_empty());
    assert!(rx.try_recv().is_err(), "nothing ran here");
    // The member's refusal and errors come back as errors.
    b.service.set_group_control(false).await.unwrap();
    let out = tools
        .call("stop_media", json!({"satellite": "Kitchen"}))
        .await;
    assert_eq!(
        out,
        ToolOutput::error("Kitchen: Control from the group is off on Kitchen.")
    );
    // Each counts here as the brain's call, and there as the group's.
    let line = "pinna_tool_calls_total{tool=\"set_volume\",outcome=\"ok\",via=\"brain\"} 1";
    let text = counters.tool_calls_text();
    assert!(text.lines().any(|l| l == line), "no {line:?} in\n{text}");
    let line = "pinna_tool_calls_total{tool=\"set_volume\",outcome=\"ok\",via=\"peer\"} 1";
    let text = b.counters.tool_calls_text();
    assert!(text.lines().any(|l| l == line), "no {line:?} in\n{text}");
}

#[tokio::test]
async fn no_satellite_or_this_ones_name_runs_the_tool_here() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    a.exchange(None).await;
    let (tools, mut rx, _) = brain(a, &[]);

    for satellite in [json!(null), json!(""), json!(" hall "), json!("HALL")] {
        let call = tools.call(
            "set_volume",
            json!({"percent": 30, "satellite": satellite.clone()}),
        );
        let answer = async {
            match rx.recv().await.unwrap() {
                Input::RunTool {
                    cmd: ApiCommand::SetVolume(30, reply),
                    ..
                } => reply
                    .send(Ok(Snapshot {
                        volume: 30,
                        ..Snapshot::default()
                    }))
                    .unwrap(),
                other => panic!("unexpected input {other:?}"),
            }
        };
        let (out, ()) = tokio::join!(call, answer);
        assert_eq!(out, ToolOutput::ok("The volume is 30%."), "{satellite}");
    }
    assert!(b.brain.commands().is_empty());

    // cancel_request is for another member only.
    assert_eq!(
        tools.call("cancel_request", json!({})).await,
        ToolOutput::error("cancel_request needs satellite.")
    );
    assert_eq!(
        tools
            .call("cancel_request", json!({"satellite": "Hall"}))
            .await,
        ToolOutput::error(
            "cancel_request acts on another satellite of the group, not this one. Its other \
             members are Kitchen."
        )
    );
    assert!(rx.try_recv().is_err());
    assert!(b.brain.commands().is_empty());
}

#[tokio::test]
async fn a_run_whose_tools_named_members_still_takes_satellite_once_none_is_in_reach() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    a.exchange(None).await;
    let (tools, mut rx, _) = brain(a, &[]);
    let offered = specs(&tools).await;
    assert!(offered["set_volume"].input_schema["properties"]["satellite"].is_object());
    assert!(offered.contains_key("cancel_request"));
    // /mcp's tools never take it.
    assert_eq!(
        tools_on(a, Surface::Mcp)
            .call("set_volume", json!({"percent": 30, "satellite": "Hall"}))
            .await,
        ToolOutput::error("set_volume has no argument satellite.")
    );

    // Kitchen switches to Home Assistant mode while the request runs.
    b.status.send_modify(|s| s.mode = Mode::HomeAssistant);
    a.exchange(None).await;

    for (tool, args) in [
        ("set_volume", json!({"percent": 30, "satellite": "Kitchen"})),
        ("cancel_request", json!({"satellite": "Kitchen"})),
    ] {
        assert_eq!(
            tools.call(tool, args).await,
            ToolOutput::error("Kitchen is in Home Assistant mode, out of reach."),
            "{tool}"
        );
    }
    // No name, or this satellite's, runs here, also once the group is gone.
    let id = a.joined().unwrap().id;
    for gone in [false, true] {
        if gone {
            a.service.forget_group(&id).await.unwrap();
        }
        for satellite in [json!(null), json!(""), json!("Hall")] {
            let call = tools.call(
                "set_volume",
                json!({"percent": 30, "satellite": satellite.clone()}),
            );
            let answer = async {
                match rx.recv().await.unwrap() {
                    Input::RunTool {
                        cmd: ApiCommand::SetVolume(30, reply),
                        ..
                    } => reply
                        .send(Ok(Snapshot {
                            volume: 30,
                            ..Snapshot::default()
                        }))
                        .unwrap(),
                    other => panic!("unexpected input {other:?}"),
                }
            };
            let (out, ()) = tokio::join!(call, answer);
            assert_eq!(out, ToolOutput::ok("The volume is 30%."), "{satellite}");
        }
    }
    for (tool, args) in [
        ("set_volume", json!({"percent": 30, "satellite": "Kitchen"})),
        ("cancel_request", json!({"satellite": "Kitchen"})),
        ("cancel_request", json!({"satellite": "Hall"})),
        ("cancel_request", json!({})),
    ] {
        assert_eq!(
            tools.call(tool, args).await,
            ToolOutput::error("This satellite is in no satellite group now."),
            "{tool}"
        );
    }
    assert!(rx.try_recv().is_err());
    assert!(b.brain.commands().is_empty());
}

#[tokio::test]
async fn the_brain_is_told_why_a_member_isnt_called() {
    let captured = capture_logs();
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office", "Porch"]).await;
    let (a, b, c, d) = (&s[0], &s[1], &s[2], &s[3]);
    grouped(&[a, b, c, d]).await;
    c.status.send_modify(|s| s.mode = Mode::HomeAssistant);
    a.exchange(None).await;
    // The search lists Porch no more.
    a.finds(&[b, c]);
    let (tools, mut rx, _) = brain(a, &[]);
    let refused = async |args: Value| {
        let out = tools.call("set_volume", args).await;
        assert!(out.is_error, "{out:?}");
        out.text
    };

    assert_eq!(
        refused(json!({"percent": 40, "satellite": "Garage"})).await,
        "No satellite of this one's group is named Garage. Its other members are Kitchen, \
         Office, Porch."
    );
    assert_eq!(
        refused(json!({"percent": 40, "satellite": "all"})).await,
        "set_volume acts on one satellite at a time. Call it once for each of Kitchen, \
         Office, Porch."
    );
    assert_eq!(
        refused(json!({"percent": 40, "satellite": 2})).await,
        "satellite must be text."
    );
    assert_eq!(
        refused(json!({"percent": "loud", "satellite": "Kitchen"})).await,
        "percent must be a whole number."
    );
    assert_eq!(
        refused(json!({"percent": 40, "satellite": "Office"})).await,
        "Office is in Home Assistant mode, out of reach."
    );
    assert_eq!(
        refused(json!({"percent": 40, "satellite": "Porch"})).await,
        "Porch is unreachable."
    );
    // The search lists Porch again, with no address.
    let mut porch = d.found();
    porch.ipv4.clear();
    porch.ipv6.clear();
    a.finds_as(vec![b.found(), c.found(), porch]);
    assert_eq!(
        refused(json!({"percent": 40, "satellite": "Porch"})).await,
        "Porch is unreachable."
    );
    // Neither time was Porch called: a failed call logs why.
    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    assert!(!logs.contains("running a tool on Porch"), "{logs}");
    // Kitchen's Satellite group went off without Hall hearing of it.
    b.service.set_group_switch(false).await.unwrap();
    assert_eq!(
        refused(json!({"percent": 40, "satellite": "Kitchen"})).await,
        format!("Kitchen: refused: {NOT_IN_THIS_GROUP}")
    );
    for sat in [b, c, d] {
        assert!(sat.brain.commands().is_empty(), "{} was called", sat.name);
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn two_satellites_that_share_this_ones_name_are_named_by_host_name() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    a.status.send_modify(|s| s.name = "hall".into());
    // Kitchen was renamed Hall, and the search heard it.
    let mut renamed = b.found();
    renamed.friendly_name = "Hall".into();
    a.finds_as(vec![renamed]);
    a.exchange(None).await;
    let (tools, mut rx, _) = brain(a, &[]);

    let out = tools
        .call("set_volume", json!({"percent": 40, "satellite": "Hall"}))
        .await;

    assert_eq!(
        out,
        ToolOutput::error(
            "2 satellites of this one's group are named Hall: hall, kitchen. Rename one on its \
             dashboard first."
        )
    );
    assert!(rx.try_recv().is_err());
    assert!(b.brain.commands().is_empty());
}

#[tokio::test]
async fn a_member_that_doesnt_answer_in_time_may_still_do_it() {
    let a = Sat::start_with(
        1,
        "Hall",
        Limits {
            call: Duration::from_millis(500),
            ..limits()
        },
    )
    .await;
    let b = Sat::start(2, "Kitchen").await;
    for sat in [&a, &b] {
        sat.service.set_group_switch(true).await.unwrap();
    }
    grouped(&[&a, &b]).await;
    a.exchange(None).await;
    // Porch takes the connection and never answers.
    let hung = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = hung.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = hung.accept().await {
            held.push(stream);
        }
    });
    let porch = "02:00:00:00:00:09";
    let id = a.joined().unwrap().id;
    let entry = Member {
        mac: porch.into(),
        name: "Porch".into(),
        version: 1,
        removed: false,
    };
    a.service
        .merge_members(&id, &[entry], &a.mac)
        .await
        .unwrap();
    let found = Found {
        name: "porch".into(),
        host: "porch".into(),
        friendly_name: "Porch".into(),
        mac: porch.into(),
        web_port: port,
        group: Some(id),
        ..b.found()
    };
    a.finds_as(vec![b.found(), found]);
    let (tools, _rx, _) = brain(&a, &[]);

    let started = std::time::Instant::now();
    let out = tools
        .call("set_volume", json!({"percent": 40, "satellite": "Porch"}))
        .await;

    assert_eq!(
        out,
        ToolOutput::error("Porch didn't answer within 0.5 seconds. It may still do it.")
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// The members of `sat`'s view by name, with what it knows of their
/// passwords.
async fn passwords(sat: &Sat) -> Vec<(String, Option<bool>)> {
    sat.view()
        .await
        .members
        .into_iter()
        .map(|m| (m.name, m.has_password))
        .collect()
}

#[tokio::test]
async fn with_a_password_a_join_needs_the_window_the_owner_opens() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    all_find(&[a, b]);
    b.auth.set_password_for_test("correct horse").await;

    // The switch alone is not consent any more, and the refusal reaches the
    // dashboard that adds it as it is worded.
    let e = a.add(b).await.unwrap_err();
    let sentence = "This satellite has a dashboard password. On its dashboard, open Satellite group \
         and choose Allow adding to a group, then add it again.";
    assert_eq!(e.message.as_deref(), Some(sentence));
    assert_eq!(b.joined(), None);
    assert_eq!(a.joined(), None);

    b.auth.allow_group_join().unwrap();
    a.add(b).await.unwrap();
    assert_eq!(b.current(), macs(&[a, b]));
}

#[tokio::test]
async fn without_a_password_the_switch_is_consent_to_a_join() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    all_find(&[a, b]);
    // A password on the satellite that adds changes nothing for the one added.
    a.auth.set_password_for_test("correct horse").await;
    a.add(b).await.unwrap();
    assert_eq!(b.current(), macs(&[a, b]));
}

#[tokio::test]
async fn a_satellite_with_a_password_refuses_messages_and_calls_from_one_without() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    b.auth.set_password_for_test("correct horse").await;
    let sentence = "Kitchen has a dashboard password and this satellite doesn't. \
         Set one on this satellite first.";

    let delivery = b.deliver(&group, a, "dinner is ready.").await;
    assert_eq!(outcome(&delivery), (Outcome::OUTCOME_REFUSED, sentence));
    let result = b
        .called(&group, a, "set_volume", r#"{"percent": 40}"#)
        .await;
    assert!(result.is_error);
    assert_eq!(result.text, sentence);
    assert!(b.brain.took().is_empty() && b.brain.commands().is_empty());

    // Once the caller has one too, both go through.
    a.auth.set_password_for_test("another horse").await;
    let delivery = b.deliver(&group, a, "dinner is ready.").await;
    assert_eq!(outcome(&delivery), (Outcome::OUTCOME_PLAYED, ""));
    let result = b
        .called(&group, a, "set_volume", r#"{"percent": 40}"#)
        .await;
    assert!(!result.is_error, "{}", result.text);
}

#[tokio::test]
async fn a_member_that_sends_no_password_fact_counts_as_without_one() {
    let s = Sat::switched_on(&["Hall", "Kitchen"]).await;
    let (a, b) = (&s[0], &s[1]);
    grouped(&[a, b]).await;
    let group = a.joined().unwrap();
    // A has a password, but an older Pinna doesn't say so.
    a.auth.set_password_for_test("correct horse").await;
    b.auth.set_password_for_test("another horse").await;
    let message = PeerMessage {
        sender: MessageField::some(PeerSender {
            web_port: a.port.into(),
            mode: pinna_api::Mode::MODE_STANDALONE.into(),
            ..Default::default()
        }),
        text: "dinner is ready.".into(),
        ..Default::default()
    };
    let answer = b
        .peer
        .deliver(Sat::sealed(&group, a, Rpc::Deliver, &message))
        .await
        .unwrap()
        .into_view()
        .to_owned_message();
    let delivery: PeerDelivery =
        envelope::open(&group, Rpc::Deliver.answer(), (&answer).into()).unwrap();
    assert_eq!(outcome(&delivery).0, Outcome::OUTCOME_REFUSED);
}

#[tokio::test]
async fn the_member_list_marks_the_members_without_a_password() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b, c]).await;
    // It says nothing of itself.
    assert_eq!(passwords(a).await[0], ("Hall".into(), None));
    b.auth.set_password_for_test("correct horse").await;
    let mut last = None;
    search(a, vec![b.found(), c.found()], &mut last).await;
    assert_eq!(
        passwords(a).await,
        [
            ("Hall".into(), None),
            ("Kitchen".into(), Some(true)),
            ("Office".into(), Some(false))
        ]
    );
}

#[tokio::test]
async fn the_list_of_satellites_marks_the_members_of_this_satellites_group() {
    let s = Sat::switched_on(&["Hall", "Kitchen", "Office"]).await;
    let (a, b, c) = (&s[0], &s[1], &s[2]);
    grouped(&[a, b]).await;
    a.finds(&[b, c]);
    let info = crate::api::Info {
        version: "0.4.0".into(),
        mac: a.mac.clone(),
        time_zones: Arc::new(Vec::new()),
        audio_backend: "none".into(),
        led_backend: "none".into(),
        model: "echo-dot-2".into(),
        virtual_device: false,
        last_boot: crate::boot::LastBoot::cold(),
        key_from_image: false,
    };
    let api = crate::api::DeviceApi::new(info, a.status.subscribe(), a.brain.inputs.clone())
        .with_satellites(a.group.satellites())
        .with_group(a.group.clone());
    let addr = crate::api::start(Arc::new(api), "127.0.0.1:0")
        .await
        .unwrap();
    let client = pinna_api::DeviceServiceClient::new(
        HttpClient::plaintext(),
        ClientConfig::new(format!("http://{addr}").parse().unwrap()),
    );
    let list = client
        .list_satellites(pinna_api::ListSatellitesRequest::default())
        .await
        .unwrap()
        .into_view()
        .to_owned_message();
    let marks: Vec<(&str, bool)> = list
        .satellites
        .iter()
        .map(|sat| (sat.friendly_name.as_str(), sat.group_member))
        .collect();
    // The key confirms a member; a satellite only the network announced is
    // not one, even when it is in no group.
    assert_eq!(marks, [("Kitchen", true), ("Office", false)]);
}
