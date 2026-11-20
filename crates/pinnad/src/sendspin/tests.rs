//! Sessions over a real loopback WebSocket (ADR 0027): a server built with snow, as Music
//! Assistant's is, against the service, with the speaker's file backend for the media lane.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::{SinkExt, StreamExt};
use pinna_audio::{Backend, EndReason, SpeakerConfig};
use pinna_sendspin::noise::PROTOCOL;
use pinna_sendspin::psk::SENTINEL_PSK;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::clock::{FilterClock, monotonic_us};
use super::*;

/// The server's clock minus the player's.
const OFFSET: i64 = 7_000_000;
/// A group id: 43 base64url characters.
const GROUP: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA0";
const WAIT: Duration = Duration::from_secs(5);

/// The speaker, with what the service did to it on record.
struct Recorder {
    speaker: Speaker,
    gains: Arc<Mutex<Vec<f32>>>,
    tracks: Arc<Mutex<Vec<Track>>>,
    pushed: Arc<AtomicUsize>,
    states: Arc<Mutex<Vec<watch::Receiver<TimedState>>>>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        let mut cfg = SpeakerConfig::new(Backend::WavFile("unused".into()));
        cfg.idle_timeout = Some(Duration::from_secs(5));
        cfg.initial_volume = 100;
        Arc::new(Self {
            speaker: Speaker::new(cfg),
            gains: Arc::default(),
            tracks: Arc::default(),
            pushed: Arc::default(),
            states: Arc::default(),
        })
    }

    fn gains(&self) -> Vec<f32> {
        self.gains.lock().unwrap().clone()
    }

    /// The state of the latest timed source the service began.
    fn state(&self) -> watch::Receiver<TimedState> {
        self.states.lock().unwrap().last().expect("a sink").clone()
    }
}

struct RecordingSink {
    inner: TimedSink,
    gains: Arc<Mutex<Vec<f32>>>,
    pushed: Arc<AtomicUsize>,
}

impl Output for Recorder {
    fn begin(&self, clock: Arc<dyn ServerClock>) -> Box<dyn Sink> {
        let inner = self.speaker.media().begin_timed(SOURCE, clock);
        self.states.lock().unwrap().push(inner.state());
        Box::new(RecordingSink {
            inner,
            gains: self.gains.clone(),
            pushed: self.pushed.clone(),
        })
    }

    fn set_track(&self, track: Track) {
        self.tracks.lock().unwrap().push(track.clone());
        self.speaker.set_track(track);
    }
}

impl Sink for RecordingSink {
    fn push(&self, server_ts_us: i64, rate: u32, pcm: Vec<i16>) -> Result<(), Refused> {
        self.pushed.fetch_add(1, Ordering::SeqCst);
        self.inner.push(server_ts_us, rate, pcm)
    }

    fn clear(&self, from_us: Option<i64>) {
        self.inner.clear(from_us);
    }

    fn end(&self) {
        self.inner.end();
    }

    fn set_gain(&self, gain: f32) {
        self.gains.lock().unwrap().push(gain);
        self.inner.set_gain(gain);
    }

    fn state(&self) -> watch::Receiver<TimedState> {
        self.inner.state()
    }

    fn stats(&self) -> TimedStats {
        self.inner.stats()
    }
}

fn setup(dir: &Path, rec: Option<Arc<Recorder>>) -> Setup {
    Setup {
        output: rec.map(|r| r as Arc<dyn Output>),
        exact_backend: false,
        state_file: dir.join("sendspin.toml"),
        device_info: device_info("echo-dot-2", Some([2, 0, 0, 0xa, 0xb, 0xc])),
        listen: vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        mdns: Mdns {
            file: dir.join("dnssd/pinna-sendspin.dnssd"),
            resolved: false,
        },
    }
}

fn dnssd(dir: &Path) -> PathBuf {
    dir.join("dnssd/pinna-sendspin.dnssd")
}

async fn wait(report: &mut watch::Receiver<Report>, what: impl Fn(&Report) -> bool) -> Report {
    let found = tokio::time::timeout(WAIT, report.wait_for(|r| what(r)))
        .await
        .map(|r| r.map(|r| r.clone()));
    match found {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => panic!("the service stopped"),
        Err(_) => panic!("timed out; the report is {:?}", *report.borrow()),
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Switches the player on as "Kitchen" and waits until it listens.
async fn on(handle: &Handle, report: &mut watch::Receiver<Report>) -> SocketAddr {
    handle.enable(Some(Params {
        name: "Kitchen".into(),
    }));
    wait(report, |r| r.state == State::Ready)
        .await
        .listening
        .expect("listening")
}

fn url(addr: SocketAddr) -> String {
    format!("ws://{addr}{PATH}")
}

/// A Sendspin server, as the test drives it.
struct Server {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    keys: snow::Keypair,
    transport: Option<snow::TransportState>,
    /// `client/init`, exactly as it came.
    client_init: String,
    /// `client/time` requests answered.
    times: usize,
}

impl Server {
    /// Connects and reads `client/init`.
    async fn connect(addr: SocketAddr) -> Self {
        let (mut ws, _) = tokio_tungstenite::connect_async(url(addr)).await.unwrap();
        let client_init = match tokio::time::timeout(WAIT, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => text.as_str().to_owned(),
            other => panic!("expected client/init, got {other:?}"),
        };
        Self {
            ws,
            keys: snow::Builder::new(PROTOCOL.parse().unwrap())
                .generate_keypair()
                .unwrap(),
            transport: None,
            client_init,
            times: 0,
        }
    }

    fn client_id(&self) -> String {
        let init: Value = serde_json::from_str(&self.client_init).unwrap();
        init["payload"]["client_id"].as_str().unwrap().to_owned()
    }

    /// The Noise handshake with the Sentinel key, as a server without a pairing runs it.
    async fn handshake(&mut self) {
        let client_key = URL_SAFE_NO_PAD.decode(self.client_id()).unwrap();
        let server_init = json!({"type": "server/init",
            "payload": {"server_id": URL_SAFE_NO_PAD.encode(&self.keys.public), "version": 1}})
        .to_string();
        let mut prologue = self.client_init.clone().into_bytes();
        prologue.extend_from_slice(server_init.as_bytes());
        let mut hs = snow::Builder::new(PROTOCOL.parse().unwrap())
            .local_private_key(&self.keys.private)
            .unwrap()
            .remote_public_key(&client_key)
            .unwrap()
            .prologue(&prologue)
            .unwrap()
            .psk(2, SENTINEL_PSK.bytes())
            .unwrap()
            .build_initiator()
            .unwrap();
        let payload = json!({"psk_id": SENTINEL_PSK.id(), "psk_category": "sn"}).to_string();
        let mut msg1 = vec![0u8; 1024];
        let n = hs.write_message(payload.as_bytes(), &mut msg1).unwrap();
        msg1.truncate(n);
        self.ws.send(Message::text(server_init)).await.unwrap();
        self.ws
            .send(Message::text(
                json!({"type": "noise/handshake", "payload": {"data": URL_SAFE_NO_PAD.encode(&msg1)}})
                    .to_string(),
            ))
            .await
            .unwrap();
        let reply = match tokio::time::timeout(WAIT, self.ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => text,
            other => panic!("expected noise/handshake, got {other:?}"),
        };
        let reply: Value = serde_json::from_str(reply.as_str()).unwrap();
        let msg2 = URL_SAFE_NO_PAD
            .decode(reply["payload"]["data"].as_str().unwrap())
            .unwrap();
        let mut plain = vec![0u8; 256];
        let n = hs.read_message(&msg2, &mut plain).unwrap();
        assert_eq!(&plain[..n], b"{}");
        self.transport = Some(hs.into_transport_mode().unwrap());
    }

    async fn send_plain(&mut self, plain: &[u8]) {
        let mut message = vec![0u8; plain.len() + 16];
        let n = self
            .transport
            .as_mut()
            .unwrap()
            .write_message(plain, &mut message)
            .unwrap();
        message.truncate(n);
        self.ws.send(Message::binary(message)).await.unwrap();
    }

    async fn send(&mut self, message: Value) {
        let mut plain = vec![0u8];
        plain.extend_from_slice(message.to_string().as_bytes());
        self.send_plain(&plain).await;
    }

    async fn send_audio(&mut self, timestamp: i64, send_ahead: u32, data: &[u8]) {
        let mut plain = vec![4u8];
        plain.extend_from_slice(&timestamp.to_be_bytes());
        plain.extend_from_slice(&send_ahead.to_be_bytes());
        plain.extend_from_slice(data);
        self.send_plain(&plain).await;
    }

    /// The next message, decrypted. `None` once the player closed the connection.
    async fn next(&mut self) -> Option<Value> {
        loop {
            let message = tokio::time::timeout(WAIT, self.ws.next())
                .await
                .expect("a message from the player");
            match message {
                Some(Ok(Message::Binary(bytes))) => {
                    let mut plain = vec![0u8; bytes.len()];
                    let n = self
                        .transport
                        .as_mut()
                        .unwrap()
                        .read_message(&bytes, &mut plain)
                        .unwrap();
                    assert_eq!(plain[0], 0, "a JSON message");
                    return Some(serde_json::from_slice(&plain[1..n]).unwrap());
                }
                Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
            }
        }
    }

    /// The payload of the next message of type `kind`, answering `client/time` meanwhile and
    /// failing on anything else.
    async fn expect(&mut self, kind: &str) -> Value {
        loop {
            let Some(message) = self.next().await else {
                panic!("the player closed the connection; expected {kind}");
            };
            let got = message["type"].as_str().unwrap().to_owned();
            if got == kind {
                return message["payload"].clone();
            }
            assert_eq!(got, "client/time", "expected {kind}: {message}");
            self.answer_time(&message).await;
        }
    }

    async fn answer_time(&mut self, request: &Value) {
        let t1 = request["payload"]["client_transmitted"].as_i64().unwrap();
        let now = monotonic_us() + OFFSET;
        self.times += 1;
        self.send(json!({"type": "server/time", "payload": {
            "client_transmitted": t1, "server_received": now, "server_transmitted": now}}))
            .await;
    }

    /// Reads until the player closes the connection, answering `client/time`.
    async fn closed(&mut self) -> Vec<Value> {
        let mut seen = Vec::new();
        while let Some(message) = self.next().await {
            if message["type"] == "client/time" {
                self.answer_time(&message).await;
            } else {
                seen.push(message);
            }
        }
        seen
    }

    /// `server/hello`, then `server/activate` with playback and every role.
    async fn hello_and_activate(&mut self) -> Value {
        self.send(json!({"type": "server/hello", "payload": {"name": "Music Assistant"}}))
            .await;
        let hello = self.expect("client/hello").await;
        self.send(
            json!({"type": "server/activate", "payload": {"activities": ["playback"],
            "active_roles": ["player@v1", "controller@v1", "metadata@v1"]}}),
        )
        .await;
        hello
    }

    async fn group(&mut self, state: &str) {
        self.send(
            json!({"type": "group/update", "payload": {"playback_state": state,
            "group_id": GROUP, "group_name": "Downstairs"}}),
        )
        .await;
    }

    async fn stream_start(&mut self) {
        self.send(json!({"type": "stream/start", "payload": {"server_transmitted": monotonic_us() + OFFSET,
            "player": {"codec": "pcm", "sample_rate": 48000, "channels": 2, "bit_depth": 16}}}))
            .await;
    }
}

/// A player switched on, and a server admitted with every role, its clock measured: returns
/// after the first `client/state`.
struct Session {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    handle: Handle,
    report: watch::Receiver<Report>,
    rec: Arc<Recorder>,
    server: Server,
    hello: Value,
    state: Value,
}

async fn session() -> Session {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_owned();
    let rec = Recorder::new();
    let (handle, mut report) = worker(setup(&dir, Some(rec.clone())));
    let addr = on(&handle, &mut report).await;
    let mut server = Server::connect(addr).await;
    server.handshake().await;
    let hello = server.hello_and_activate().await;
    let state = server.expect("client/state").await;
    Session {
        _dir: tmp,
        dir,
        handle,
        report,
        rec,
        server,
        hello,
        state,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_from_the_handshake_to_audio_in_the_lane() {
    let mut s = session().await;
    // The keys were kept before the first server learned them.
    let saved = saved::load(&s.dir.join("sendspin.toml")).expect("kept");
    assert_eq!(saved.keys.identity.client_id(), s.server.client_id());
    let init: Value = serde_json::from_str(&s.server.client_init).unwrap();
    assert_eq!(init["payload"]["suite"], "25519_ChaChaPoly_SHA256");

    assert_eq!(
        s.hello,
        json!({
            "name": "Kitchen",
            "device_info": {"product_name": "Echo Dot (2nd gen)", "manufacturer": "Pinna",
                "software_version": pinna_version::VERSION, "mac_address": "02:00:00:0a:0b:0c"},
            "supported_roles": ["player@v1", "controller@v1", "metadata@v1"],
            "player@v1_support": {"supported_formats": [
                {"codec": "flac", "channels": 2, "sample_rate": 48000, "bit_depth": 16},
                {"codec": "pcm", "channels": 2, "sample_rate": 48000, "bit_depth": 16},
                {"codec": "flac", "channels": 2, "sample_rate": 44100, "bit_depth": 16},
                {"codec": "pcm", "channels": 2, "sample_rate": 44100, "bit_depth": 16}],
                "buffer_capacity": 1048576},
            "supported_pair_methods": {"pairing_psk": {}},
            "unpaired_access": {"enabled": true}
        })
    );
    // client/state waited for a whole burst of the clock.
    assert!(s.server.times >= 8, "{} time requests", s.server.times);
    assert_eq!(
        s.state,
        json!({"available": true, "player": {"volume": 100, "muted": false,
            "output_delay_ms": 0, "required_lead_time_ms": 300, "min_buffer_ms": 0,
            "supported_commands": ["volume", "mute"],
            "format": {"codec": "flac", "channels": 2, "sample_rate": 48000, "bit_depth": 16}}})
    );
    let r = wait(&mut s.report, |r| {
        r.state == State::Connected("Music Assistant".into())
    })
    .await;
    assert_eq!(
        (r.name.as_str(), r.server.as_str()),
        ("Kitchen", "Music Assistant")
    );
    assert!(!r.exact_timing, "the file backend has no hardware pointer");
    assert!(r.stats.filter_error_us.is_some());

    s.server
        .send(
            json!({"type": "server/state", "payload": {"metadata": {"timestamp": 0,
            "title": "Song\n", "artist": "Band", "album": "Record"}}}),
        )
        .await;
    s.server.group("playing").await;
    s.server.stream_start().await;
    wait(&mut s.report, |r| r.state == State::Playing).await;
    // 1.5 s of PCM, 20 ms a chunk, sent in real time and due 400 ms ahead. Each was sent
    // 100 ms before it arrived, by its send_ahead.
    let chunk = vec![0u8; 960 * 4];
    let first = monotonic_us() + OFFSET + 400_000;
    for k in 0..75 {
        let ts = first + k * 20_000;
        let sent = monotonic_us() + OFFSET - 100_000;
        s.server
            .send_audio(ts, u32::try_from(ts - sent).unwrap(), &chunk)
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut state = s.rec.state();
    tokio::time::timeout(WAIT, state.wait_for(|s| *s == TimedState::Playing))
        .await
        .expect("the timed source plays")
        .unwrap();
    assert!(s.rec.pushed.load(Ordering::SeqCst) > 50);
    let media = s.rec.speaker.media().state();
    until("the media state", || media.borrow().source == SOURCE).await;
    assert_eq!(
        s.rec
            .tracks
            .lock()
            .unwrap()
            .last()
            .map(|t| t.title.as_str()),
        Some("Song")
    );
    // How late the chunks came is reported once a second of them has.
    let state = s.server.expect("client/state").await;
    let late = state["player"]["min_buffer_ms"].as_u64().unwrap();
    assert!((100..=140).contains(&late), "{state}");

    // Another source takes the lane: the satellite leaves its group, which plays on.
    let clock = Arc::new(FilterClock::new());
    let mut filter = pinna_sendspin::TimeFilter::new();
    let now = monotonic_us();
    filter.update(pinna_sendspin::clock::Sample::from_timestamps(now, now, now, now + 2).unwrap());
    clock.update(&filter);
    let other = s.rec.speaker.media().begin_timed("other", clock);
    other
        .push(monotonic_us() + 50_000, 48_000, vec![0; 2 * 4_800])
        .unwrap();
    assert_eq!(s.server.expect("client/leave").await, json!({}));
    tokio::time::timeout(
        WAIT,
        state_of(&s.rec).wait_for(|s| *s == TimedState::Ended(EndReason::Replaced)),
    )
    .await
    .expect("replaced")
    .unwrap();
    let r = wait(&mut s.report, |r| matches!(r.state, State::Connected(_))).await;
    assert!(!r.group_paused);
}

fn state_of(rec: &Recorder) -> watch::Receiver<TimedState> {
    rec.states.lock().unwrap()[0].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn volume_and_mute_both_ways() {
    let mut s = session().await;
    s.server.stream_start().await;
    until("a sink", || !s.rec.gains().is_empty()).await;
    assert_eq!(s.rec.gains(), [1.0]);

    // From the server: the sink's level follows the specification's curve, the server hears
    // it back, and the file keeps it.
    s.server
        .send(json!({"type": "server/command", "payload": {"player": {"command": "volume", "volume": 30}}}))
        .await;
    let state = s.server.expect("client/state").await;
    assert_eq!(state["player"]["volume"], 30);
    let g = 0.3f32.powf(1.5);
    until("the gain", || s.rec.gains().last() == Some(&g)).await;
    let r = wait(&mut s.report, |r| r.volume == 30).await;
    assert!(!r.muted);
    let file = s.dir.join("sendspin.toml");
    until("the file", || {
        saved::load(&file).is_some_and(|f| f.volume == 30)
    })
    .await;

    s.server
        .send(json!({"type": "server/command", "payload": {"player": {"command": "mute", "mute": true}}}))
        .await;
    assert_eq!(
        s.server.expect("client/state").await["player"]["muted"],
        true
    );
    until("silence", || s.rec.gains().last() == Some(&0.0)).await;
    wait(&mut s.report, |r| r.muted).await;

    // From Home Assistant or the dashboard: the same level, reported to the server.
    s.handle.set_muted(false);
    assert_eq!(
        s.server.expect("client/state").await["player"]["muted"],
        false
    );
    s.handle.set_volume(55);
    let state = s.server.expect("client/state").await;
    assert_eq!(
        (&state["player"]["volume"], &state["player"]["muted"]),
        (&json!(55), &json!(false))
    );
    let g = 0.55f32.powf(1.5);
    until("the gain", || s.rec.gains().last() == Some(&g)).await;
    until("the file", || {
        saved::load(&file).is_some_and(|f| f.volume == 55 && !f.muted)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_group_and_its_controls() {
    let mut s = session().await;
    // Without the controller's state, no command can go.
    assert!(!s.handle.control(Control::Pause).await);
    s.server
        .send(json!({"type": "server/state", "payload": {"controller": {
            "supported_commands": ["play", "pause", "stop"], "volume": 50, "muted": false,
            "repeat": "off", "shuffle": false}}}))
        .await;
    s.server.group("playing").await;
    s.server.stream_start().await;
    wait(&mut s.report, |r| r.state == State::Playing).await;
    // Paused through the group: the server ends the stream.
    assert!(s.handle.control(Control::Pause).await);
    assert_eq!(
        s.server.expect("client/command").await,
        json!({"controller": {"command": "pause"}})
    );
    s.server
        .send(json!({"type": "stream/end", "payload": {}}))
        .await;
    s.server.group("stopped").await;
    let r = wait(&mut s.report, |r| r.group_paused).await;
    assert_eq!(r.state, State::Connected("Music Assistant".into()));
    assert!(s.handle.control(Control::Play).await);
    assert_eq!(
        s.server.expect("client/command").await,
        json!({"controller": {"command": "play"}})
    );
    // Another source plays meanwhile: the satellite leaves the group.
    s.handle.another_source();
    assert_eq!(s.server.expect("client/leave").await, json!({}));
    wait(&mut s.report, |r| !r.group_paused).await;
    // The Brain hears of the source more than once before the report catches up (buffering,
    // then playing): the group is left once. The volume goes through the same queue, so a
    // second leave would come before its client/state.
    s.handle.another_source();
    s.handle.set_volume(42);
    assert_eq!(
        s.server.expect("client/state").await["player"]["volume"],
        42
    );
}

/// The report once the service has read everything the server sent so far: a volume
/// command goes last, and its `client/state` comes back.
async fn settled(s: &mut Session, volume: u8) -> Report {
    s.server
        .send(json!({"type": "server/command", "payload": {"player": {"command": "volume", "volume": volume}}}))
        .await;
    s.server.expect("client/state").await;
    wait(&mut s.report, |r| r.volume == volume).await
}

/// A group reports `stopped` whether it paused, stopped or ran out of queue: only a pause
/// sent from here shows it paused. A queue that ends on its own, and Stop from here, leave
/// the satellite Connected and the group not paused (ADR 0027 §14).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_pause_from_here_leaves_the_group_paused() {
    let mut s = session().await;
    s.server
        .send(json!({"type": "server/state", "payload": {"controller": {
            "supported_commands": ["play", "pause", "stop"], "volume": 50, "muted": false,
            "repeat": "off", "shuffle": false}}}))
        .await;
    let end = json!({"type": "stream/end", "payload": {}});

    // The queue runs out.
    s.server.group("playing").await;
    s.server.stream_start().await;
    wait(&mut s.report, |r| r.state == State::Playing).await;
    s.server.send(end.clone()).await;
    s.server.group("stopped").await;
    let r = settled(&mut s, 31).await;
    assert_eq!(
        (r.state, r.group_paused),
        (State::Connected("Music Assistant".into()), false)
    );

    // Paused from here, then stopped from here.
    s.server.group("playing").await;
    s.server.stream_start().await;
    wait(&mut s.report, |r| r.state == State::Playing).await;
    assert!(s.handle.control(Control::Pause).await);
    s.server.expect("client/command").await;
    s.server.send(end.clone()).await;
    s.server.group("stopped").await;
    wait(&mut s.report, |r| r.group_paused).await;
    assert!(s.handle.control(Control::Stop).await);
    assert_eq!(
        s.server.expect("client/command").await,
        json!({"controller": {"command": "stop"}})
    );
    assert!(!settled(&mut s, 32).await.group_paused);

    // Stopped from here while it plays.
    s.server.group("playing").await;
    s.server.stream_start().await;
    wait(&mut s.report, |r| r.state == State::Playing).await;
    assert!(s.handle.control(Control::Stop).await);
    s.server.expect("client/command").await;
    s.server.send(end.clone()).await;
    s.server.group("stopped").await;
    assert!(!settled(&mut s, 33).await.group_paused);

    // Paused from here, resumed elsewhere, then the queue runs out.
    s.server.group("playing").await;
    s.server.stream_start().await;
    wait(&mut s.report, |r| r.state == State::Playing).await;
    assert!(s.handle.control(Control::Pause).await);
    s.server.expect("client/command").await;
    s.server.send(end.clone()).await;
    s.server.group("stopped").await;
    wait(&mut s.report, |r| r.group_paused).await;
    s.server.group("playing").await;
    s.server.stream_start().await;
    wait(&mut s.report, |r| r.state == State::Playing).await;
    s.server.send(end).await;
    s.server.group("stopped").await;
    assert!(!settled(&mut s, 34).await.group_paused);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_off_withdraws_the_record_first() {
    let mut s = session().await;
    let record = std::fs::read_to_string(dnssd(&s.dir)).unwrap();
    let port = s.report.borrow().listening.unwrap().port();
    assert!(
        record.contains(&format!(
            "\nName=Kitchen\nType=_sendspin._tcp\nPort={port}\nTxtText=path=/sendspin name=Kitchen\n"
        )),
        "{record}"
    );
    s.handle.enable(None);
    let goodbye = s.server.expect("client/goodbye").await;
    // The record went before the goodbye.
    assert!(!dnssd(&s.dir).exists());
    assert_eq!(goodbye, json!({"reason": "shutdown"}));
    assert!(s.server.closed().await.is_empty());
    let r = wait(&mut s.report, |r| r.state == State::Off).await;
    assert_eq!((r.name.as_str(), r.listening), ("", None));
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    until("the listener to close", || {
        std::net::TcpStream::connect(addr).is_err()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rename_rewrites_the_record_and_restarts_the_connection() {
    let mut s = session().await;
    s.handle.rename("Hall 100%");
    assert_eq!(
        s.server.expect("client/goodbye").await,
        json!({"reason": "restart"})
    );
    let record = std::fs::read_to_string(dnssd(&s.dir)).unwrap();
    assert!(record.contains("\nName=Hall 100\n"), "{record}");
    assert!(record.contains("\"name=Hall 100\""), "{record}");
    let addr = wait(&mut s.report, |r| {
        r.state == State::Ready && r.name == "Hall 100"
    })
    .await
    .listening
    .unwrap();
    let mut server = Server::connect(addr).await;
    server.handshake().await;
    assert_eq!(server.hello_and_activate().await["name"], "Hall 100");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_and_a_third_server_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (handle, mut report) = worker(setup(tmp.path(), Some(Recorder::new())));
    let addr = on(&handle, &mut report).await;

    let mut request = url(addr).into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "http://example.com".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 403),
        other => panic!("expected 403, got {other:?}"),
    }
    match tokio_tungstenite::connect_async(format!("ws://{addr}/other")).await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 404),
        other => panic!("expected 404, got {other:?}"),
    }

    let first = Server::connect(addr).await;
    let _second = Server::connect(addr).await;
    match tokio_tungstenite::connect_async(url(addr)).await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 503),
        other => panic!("expected 503, got {other:?}"),
    }
    // One goes: there is room again.
    drop(first);
    let deadline = Instant::now() + WAIT;
    loop {
        match tokio_tungstenite::connect_async(url(addr)).await {
            Ok(_) => break,
            Err(tungstenite::Error::Http(response)) if response.status() == 503 => {
                assert!(Instant::now() < deadline, "still refused");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_older_than_the_protocol_is_named() {
    let tmp = tempfile::tempdir().unwrap();
    let (handle, mut report) = worker(setup(tmp.path(), Some(Recorder::new())));
    let addr = on(&handle, &mut report).await;
    // The older wire format expects client/hello first, and closes on client/init.
    let mut old = Server::connect(addr).await;
    old.ws.close(None).await.unwrap();
    let r = wait(&mut report, |r| !r.reason.is_empty()).await;
    assert_eq!(r.state, State::Ready);
    assert_eq!(r.reason, "Music Assistant must be 2.10.7 or later");
    // A server that connects properly clears it.
    let mut server = Server::connect(addr).await;
    server.handshake().await;
    server.hello_and_activate().await;
    let r = wait(&mut report, |r| matches!(r.state, State::Connected(_))).await;
    assert_eq!(r.reason, "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_and_volume_are_kept_across_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("sendspin.toml");
    let first_id = {
        let (handle, mut report) = worker(setup(tmp.path(), Some(Recorder::new())));
        // Nothing is written until the player is first switched on.
        handle.set_volume(42);
        wait(&mut report, |r| r.volume == 42).await;
        assert!(!file.exists());
        let addr = on(&handle, &mut report).await;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        until("the volume in the file", || {
            saved::load(&file).is_some_and(|f| f.volume == 42)
        })
        .await;
        handle.set_muted(true);
        until("mute in the file", || {
            saved::load(&file).is_some_and(|f| f.muted)
        })
        .await;
        Server::connect(addr).await.client_id()
    };
    let (handle, mut report) = worker(setup(tmp.path(), Some(Recorder::new())));
    let r = wait(&mut report, |r| r.volume == 42).await;
    assert!(r.muted);
    let addr = on(&handle, &mut report).await;
    assert_eq!(Server::connect(addr).await.client_id(), first_id);

    // A damaged file is set aside, and the player starts afresh with a new identity.
    handle.enable(None);
    wait(&mut report, |r| r.state == State::Off).await;
    std::fs::write(&file, "volume = \"loud\"\n").unwrap();
    let (handle, mut report) = worker(setup(tmp.path(), Some(Recorder::new())));
    let addr = on(&handle, &mut report).await;
    assert_ne!(Server::connect(addr).await.client_id(), first_id);
    assert!(tmp.path().join("sendspin.toml.bad").exists());
    assert_eq!(report.borrow().volume, 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_port_in_use_fails_and_is_tried_again() {
    let tmp = tempfile::tempdir().unwrap();
    let taken = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = taken.local_addr().unwrap();
    let mut s = setup(tmp.path(), Some(Recorder::new()));
    s.listen = vec![addr];
    let (handle, mut report) = worker(s);
    handle.enable(Some(Params {
        name: "Kitchen".into(),
    }));
    let r = wait(&mut report, |r| matches!(r.state, State::Failed(_))).await;
    assert_eq!(r.reason, format!("port {} is in use", addr.port()));
    assert!(!dnssd(tmp.path()).exists());
    drop(taken);
    let r = tokio::time::timeout(
        Duration::from_secs(8),
        report.wait_for(|r| r.state == State::Ready),
    )
    .await
    .expect("listening after a retry")
    .unwrap()
    .clone();
    assert_eq!(r.listening, Some(addr));
    assert!(dnssd(tmp.path()).exists());
}

#[tokio::test]
async fn without_a_speaker_the_switch_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("dnssd")).unwrap();
    // A record left over from before a restart.
    std::fs::write(dnssd(tmp.path()), "old").unwrap();
    let (handle, mut report) = worker(setup(tmp.path(), None));
    assert_eq!(report.borrow().state, State::Unavailable);
    handle.enable(Some(Params {
        name: "Kitchen".into(),
    }));
    handle.enable(None);
    until("the old record to go", || !dnssd(tmp.path()).exists()).await;
    let r = report.borrow_and_update().clone();
    assert_eq!((r.state, r.listening), (State::Unavailable, None));
    assert!(!tmp.path().join("sendspin.toml").exists());
}
