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
use pinna_api::{
    AddGroupMemberRequest, AssistantServiceClient, GetSatelliteGroupRequest, PeerEnvelope,
    PeerHello, PeerJoinRequest, PeerJoinResponse, PeerMemberList, PeerSender, PeerService,
    PeerServiceClient, RemoveGroupMemberRequest, SatelliteGroupView, SetSatelliteGroupRequest,
};
use tokio::sync::{mpsc, watch};

use super::envelope::{self, Rpc};
use super::model::{Member, same_mac};
use super::peer::{NOT_IN_THIS_GROUP, PeerApi};
use super::runtime::{Change, to_wire};
use super::seal::Key;
use super::{Group, Joined, Limits, exchange};
use crate::assistant_api::AssistantApi;
use crate::satellites::{Found, Seen};
use crate::settings::Mode;
use crate::standalone::service::{Paths, Service};
use crate::voice::{Input, Snapshot};

fn limits() -> Limits {
    Limits {
        connect: Duration::from_millis(300),
        call: Duration::from_secs(3),
        exchange: Duration::from_secs(2),
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
    _inputs: mpsc::Receiver<Input>,
    _dir: tempfile::TempDir,
}

impl Sat {
    async fn start(n: u8, name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (status, status_rx) = watch::channel(Snapshot {
            mode: Mode::Standalone,
            friendly_name: name.into(),
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
            status_rx,
            seen_rx.clone(),
            limits(),
        ));
        let (inputs, inputs_rx) = mpsc::channel(8);
        let assistant = AssistantApi::new(service.clone(), inputs)
            .with_satellites(seen_rx, mac.clone())
            .with_group(group.clone());
        let router = crate::assistant_api::router(Arc::new(assistant))
            .add_service(Arc::new(PeerApi::new(group.clone())));
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

#[tokio::test]
async fn deliver_and_call_are_not_taken_yet() {
    let s = Sat::switched_on(&["Hall"]).await;
    let e = s[0]
        .peer
        .deliver(PeerEnvelope::default())
        .await
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unimplemented);
    let e = s[0]
        .peer
        .call(PeerEnvelope::default())
        .await
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unimplemented);
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
fn capture_logs() -> Arc<Mutex<Vec<u8>>> {
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

#[tokio::test]
async fn the_key_never_leaves_in_a_view_an_answer_or_a_log_line() {
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
    a.remove(c).await.unwrap();
    a.group.set_switch(false).await.unwrap();

    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    assert!(
        logs.contains("added Kitchen to the satellite group"),
        "{logs}"
    );
    texts.push(logs);
    for text in &texts {
        for form in &forms {
            assert!(!text.contains(form.as_str()), "{form} in {text}");
        }
    }
    // The raw bytes neither.
    let answer_bytes = answer.encode_to_vec();
    assert!(
        !answer_bytes
            .windows(32)
            .any(|w| w == group.key.bytes().as_slice())
    );
}
