//! TCP server and the single actor that owns all protocol state.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use prost::Message;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::{Instant, MissedTickBehavior},
};
use tracing::{debug, info, warn};

use crate::{
    frame::{Frame, FrameCodec, HandshakeStep, NoiseHandshake, Plaintext},
    proto::{ApiMessage, proto},
    types::{
        DeviceConfig, DeviceEvent, ESPHOME_VERSION, EntityCategory, HaCommand, LightState,
        MediaPlayerCommand, MediaPlayerStateKind, MediaPurpose, NumberMode, TimerEventType,
        UpdateCommand, UpdateState, VOICE_FEATURE_FLAGS, VoiceEventType,
    },
};

/// Entity key of the microphone mute switch.
pub const MUTE_KEY: u32 = 2;
/// Entity key of the light.
pub const LIGHT_KEY: u32 = 3;

/// Deterministic key (FNV-1a 32-bit) of an entity from its platform and object id.
fn entity_key(platform: &str, object_id: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for b in platform.bytes().chain(*b"/").chain(object_id.bytes()) {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h
}

/// Keys of all configured entities; fails on a collision (also with the fixed keys and 0).
fn check_keys(cfg: &DeviceConfig) -> anyhow::Result<()> {
    let mut seen: HashMap<u32, String> = HashMap::new();
    let mut add = |key: u32, what: String| -> anyhow::Result<()> {
        if let Some(other) = seen.insert(key, what.clone()) {
            anyhow::bail!("entity key {key} collides: {other} and {what}");
        }
        Ok(())
    };
    add(0, "reserved".into())?;
    add(MUTE_KEY, "mute switch".into())?;
    add(LIGHT_KEY, "light".into())?;
    for (platform, ids) in [
        (
            "number",
            cfg.numbers.iter().map(|e| &e.object_id).collect::<Vec<_>>(),
        ),
        ("button", cfg.buttons.iter().map(|e| &e.object_id).collect()),
        ("event", cfg.events.iter().map(|e| &e.object_id).collect()),
        (
            "text_sensor",
            cfg.text_sensors.iter().map(|e| &e.object_id).collect(),
        ),
        (
            "media_player",
            cfg.media_player.iter().map(|e| &e.object_id).collect(),
        ),
        (
            "switch",
            cfg.switches.iter().map(|e| &e.object_id).collect(),
        ),
        ("select", cfg.selects.iter().map(|e| &e.object_id).collect()),
        ("update", cfg.updates.iter().map(|e| &e.object_id).collect()),
    ] {
        for id in ids {
            add(entity_key(platform, id), format!("{platform} {id}"))?;
        }
    }
    Ok(())
}

const MAX_CLIENTS: usize = 2;

/// The key in a `NoiseEncryptionSetKeyRequest`: base64 text, as Home
/// Assistant and ESPHome send it, or 32 raw bytes. Never all zeros: that is
/// the key for unprovisioned devices.
fn set_key_value(key: &[u8]) -> Option<[u8; 32]> {
    let key = std::str::from_utf8(key)
        .ok()
        .and_then(|s| crate::parse_encryption_key(s).ok())
        .or_else(|| <[u8; 32]>::try_from(key).ok())?;
    (key != [0; 32]).then_some(key)
}
const API_VERSION: (u32, u32) = (1, 14);
/// Close a connection that stayed silent this long (after hello).
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Probe a connection that stayed silent this long.
const PING_AFTER: Duration = Duration::from_secs(60);
/// Minimum spacing between our own pings.
const PING_EVERY: Duration = Duration::from_secs(30);
/// Time a new connection has to send its hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Time a client has to acknowledge our disconnect request.
const DISCONNECT_GRACE: Duration = Duration::from_secs(5);
const OUT_QUEUE: usize = 512;

/// Returned when the server task has stopped.
#[derive(Debug, thiserror::Error)]
#[error("esphome server stopped")]
pub struct Stopped;

enum Input {
    Device(DeviceEvent),
    Connected {
        id: u64,
        peer: String,
        encrypted: bool,
        out: mpsc::Sender<(u32, Bytes)>,
    },
    Frame {
        id: u64,
        frame: Frame,
    },
    Gone {
        id: u64,
    },
}

/// Handle used by the device side to push [`DeviceEvent`]s. Cloneable.
#[derive(Clone)]
pub struct EsphomeHandle {
    tx: EventSender,
    addr: SocketAddr,
}

#[derive(Clone)]
enum EventSender {
    Server(mpsc::Sender<Input>),
    #[cfg(feature = "test-util")]
    Detached(mpsc::Sender<DeviceEvent>),
}

impl EsphomeHandle {
    #[cfg(feature = "test-util")]
    pub fn detached(cap: usize) -> (Self, mpsc::Receiver<DeviceEvent>) {
        let (tx, rx) = mpsc::channel(cap);
        (
            Self {
                tx: EventSender::Detached(tx),
                addr: ([127, 0, 0, 1], 0).into(),
            },
            rx,
        )
    }

    /// Send an event; waits if the server is momentarily backed up.
    pub async fn send(&self, ev: DeviceEvent) -> Result<(), Stopped> {
        match &self.tx {
            EventSender::Server(tx) => tx.send(Input::Device(ev)).await.map_err(|_| Stopped),
            #[cfg(feature = "test-util")]
            EventSender::Detached(tx) => tx.send(ev).await.map_err(|_| Stopped),
        }
    }

    /// Non-blocking variant; fails if the queue is full or the server stopped.
    pub fn try_send(&self, ev: DeviceEvent) -> Result<(), Stopped> {
        match &self.tx {
            EventSender::Server(tx) => tx.try_send(Input::Device(ev)).map_err(|_| Stopped),
            #[cfg(feature = "test-util")]
            EventSender::Detached(tx) => tx.try_send(ev).map_err(|_| Stopped),
        }
    }

    /// Address the server is listening on (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Start the server. Returns the event handle and the stream of commands from Home Assistant.
pub async fn serve(
    cfg: DeviceConfig,
) -> anyhow::Result<(EsphomeHandle, mpsc::Receiver<HaCommand>)> {
    check_keys(&cfg)?;
    let listener = TcpListener::bind((cfg.bind, cfg.port)).await?;
    let addr = listener.local_addr()?;
    info!(%addr, name = %cfg.name, "esphome api listening");
    let (in_tx, in_rx) = mpsc::channel(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let key = Arc::new(Mutex::new(cfg.encryption_key));
    let ident = (Arc::new(Mutex::new(cfg.name.clone())), cfg.mac);
    let actor = Actor::new(cfg, cmd_tx, key.clone(), ident.0.clone());
    tokio::spawn(actor.run(in_rx));
    tokio::spawn(accept_loop(listener, in_tx.clone(), key, ident));
    Ok((
        EsphomeHandle {
            tx: EventSender::Server(in_tx),
            addr,
        },
        cmd_rx,
    ))
}

#[cfg(all(test, feature = "test-util"))]
#[tokio::test]
async fn detached_events_arrive_in_order() {
    let (handle, mut events) = EsphomeHandle::detached(2);
    handle.send(DeviceEvent::VoiceStop).await.unwrap();
    handle.try_send(DeviceEvent::RequestTime).unwrap();
    assert!(handle.try_send(DeviceEvent::RequestTime).is_err());
    assert_eq!(events.recv().await, Some(DeviceEvent::VoiceStop));
    assert_eq!(events.recv().await, Some(DeviceEvent::RequestTime));
}

/// Encryption key shared between the actor (which updates it) and the accept loop.
type SharedKey = Arc<Mutex<Option<[u8; 32]>>>;

/// Node name shared between the actor (which updates it) and the accept loop.
type SharedName = Arc<Mutex<String>>;

async fn accept_loop(
    listener: TcpListener,
    inputs: mpsc::Sender<Input>,
    key: SharedKey,
    ident: (SharedName, [u8; 6]),
) {
    let live = Arc::new(AtomicUsize::new(0));
    let mut next_id = 0u64;
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        if inputs.is_closed() {
            return;
        }
        if live.load(Ordering::SeqCst) >= MAX_CLIENTS {
            warn!(%peer, "rejecting connection: client limit reached");
            continue;
        }
        live.fetch_add(1, Ordering::SeqCst);
        next_id += 1;
        let id = next_id;
        let inputs = inputs.clone();
        let live = live.clone();
        let m = ident.1;
        let mac = format!(
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        );
        let name = ident.0.lock().map(|n| n.clone()).unwrap_or_default();
        // With a key, only the key. Without one, plaintext or the all-zero
        // key, so Home Assistant can send one without it travelling in
        // plaintext (ADR 0018).
        let mode = match key.lock().ok().and_then(|k| *k) {
            Some(psk) => Mode::Noise(NoiseHandshake::new(psk, &name, &mac)),
            None => Mode::Provisionable(NoiseHandshake::new([0; 32], &name, &mac)),
        };
        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);
            run_client(id, stream, peer, mode, inputs).await;
            live.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// How a connection is to be spoken.
enum Mode {
    /// Noise with the device's key.
    Noise(NoiseHandshake),
    /// No key yet: plaintext, or Noise with the all-zero key; the client's
    /// first byte says which.
    Provisionable(NoiseHandshake),
}

/// Per-connection I/O: framing, idle detection and keep-alive pings.
async fn run_client(
    id: u64,
    mut stream: TcpStream,
    peer: SocketAddr,
    mode: Mode,
    inputs: mpsc::Sender<Input>,
) {
    let (mut noise, mut zero_key) = match mode {
        Mode::Noise(hs) => (Some(hs), None),
        Mode::Provisionable(hs) => (None, Some(hs)),
    };
    let (out_tx, mut out_rx) = mpsc::channel::<(u32, Bytes)>(OUT_QUEUE);
    if inputs
        .send(Input::Connected {
            id,
            peer: peer.to_string(),
            encrypted: noise.is_some(),
            out: out_tx.clone(),
        })
        .await
        .is_err()
    {
        return;
    }
    let mut codec: Box<dyn FrameCodec> = Box::new(Plaintext);
    let mut rbuf = BytesMut::with_capacity(4096);
    let mut wbuf = BytesMut::new();
    let started = Instant::now();
    let mut last_seen = started;
    let mut last_ping = started;
    let mut hello = false;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Only the actor holds a sender once we drop ours, so closing it ends the connection.
    drop(out_tx);
    'conn: loop {
        tokio::select! {
            r = stream.read_buf(&mut rbuf) => {
                match r {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => { debug!(id, "read error: {e}"); break; }
                }
                last_seen = Instant::now();
                if !rbuf.is_empty()
                    && let Some(hs) = zero_key.take()
                    && rbuf[0] == 0x01
                {
                    noise = Some(hs);
                }
                if let Some(hs) = noise.as_mut() {
                    wbuf.clear();
                    let step = hs.feed(&mut rbuf, &mut wbuf);
                    if !wbuf.is_empty() && stream.write_all(&wbuf).await.is_err() {
                        break;
                    }
                    match step {
                        Ok(HandshakeStep::Pending) => continue,
                        Ok(HandshakeStep::Done(c)) => {
                            codec = Box::new(c);
                            noise = None;
                        }
                        Ok(HandshakeStep::Rejected) => break,
                        Err(e) => { warn!(id, "noise handshake error: {e}"); break; }
                    }
                }
                loop {
                    match codec.decode(&mut rbuf) {
                        Ok(Some(frame)) => {
                            hello |= frame.id == 1;
                            if inputs.send(Input::Frame { id, frame }).await.is_err() {
                                break 'conn;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => { warn!(id, "framing error: {e}"); break 'conn; }
                    }
                }
            }
            m = out_rx.recv() => {
                let Some((mid, payload)) = m else { break };
                wbuf.clear();
                if let Err(e) = codec.encode(mid, &payload, &mut wbuf) {
                    warn!(id, "encode error: {e}");
                    break;
                }
                if let Err(e) = stream.write_all(&wbuf).await { debug!(id, "write error: {e}"); break; }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let idle = now - last_seen;
                if (hello && idle >= IDLE_TIMEOUT) || (!hello && now - started >= HELLO_TIMEOUT) {
                    debug!(id, "idle timeout");
                    break;
                }
                if hello && idle >= PING_AFTER && now - last_ping >= PING_EVERY {
                    last_ping = now;
                    wbuf.clear();
                    if codec.encode(proto::PingRequest::ID, &[], &mut wbuf).is_err()
                        || stream.write_all(&wbuf).await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    }
    let _ = stream.shutdown().await;
    let _ = inputs.send(Input::Gone { id }).await;
}

struct Client {
    out: mpsc::Sender<(u32, Bytes)>,
    peer: String,
    encrypted: bool,
    hello: bool,
    states: bool,
    /// We asked it to disconnect, at this time; closed on its acknowledgement
    /// or after [`DISCONNECT_GRACE`].
    closing: Option<Instant>,
}

struct Actor {
    cfg: DeviceConfig,
    cmds: mpsc::Sender<HaCommand>,
    key: SharedKey,
    name: SharedName,
    clients: HashMap<u64, Client>,
    voice_owner: Option<u64>,
    run_active: bool,
    light: LightState,
    muted: bool,
    numbers: HashMap<String, f32>,
    texts: HashMap<String, String>,
    switches: HashMap<String, bool>,
    selects: HashMap<String, String>,
    updates: HashMap<String, UpdateState>,
    media: (MediaPlayerStateKind, f32, bool),
    active_wake: Vec<String>,
}

impl Actor {
    fn new(
        cfg: DeviceConfig,
        cmds: mpsc::Sender<HaCommand>,
        key: SharedKey,
        name: SharedName,
    ) -> Self {
        let active_wake = cfg.active_wake_words.clone();
        Self {
            cfg,
            cmds,
            key,
            name,
            clients: HashMap::new(),
            voice_owner: None,
            run_active: false,
            light: LightState::default(),
            muted: false,
            numbers: HashMap::new(),
            texts: HashMap::new(),
            switches: HashMap::new(),
            selects: HashMap::new(),
            updates: HashMap::new(),
            media: (MediaPlayerStateKind::Idle, 0.0, false),
            active_wake,
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Input>) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let input = tokio::select! {
                input = rx.recv() => match input {
                    Some(input) => input,
                    None => return,
                },
                _ = tick.tick() => {
                    self.close_unacknowledged();
                    continue;
                }
            };
            match input {
                Input::Device(ev) => self.device_event(ev),
                Input::Connected {
                    id,
                    peer,
                    encrypted,
                    out,
                } => {
                    self.clients.insert(
                        id,
                        Client {
                            out,
                            peer,
                            encrypted,
                            hello: false,
                            states: false,
                            closing: None,
                        },
                    );
                }
                Input::Frame { id, frame } => self.frame(id, frame),
                Input::Gone { id } => self.drop_client(id),
            }
        }
    }

    fn command(&self, cmd: HaCommand) {
        if let Err(e) = self.cmds.try_send(cmd) {
            warn!("dropping command for the device: {e}");
        }
    }

    /// Queue a message for one client; a client that cannot keep up is disconnected.
    fn send<M: ApiMessage>(&mut self, id: u64, msg: &M) {
        let Some(c) = self.clients.get(&id) else {
            return;
        };
        if c.out
            .try_send((M::ID, Bytes::from(msg.encode_to_vec())))
            .is_err()
        {
            warn!(id, "client queue full or closed, dropping client");
            self.drop_client(id);
        }
    }

    fn broadcast<M: ApiMessage>(&mut self, msg: &M, only_states: bool) {
        let ids: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, c)| c.hello && (!only_states || c.states))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.send(id, msg);
        }
    }

    /// Close the connections that didn't acknowledge our disconnect request in time.
    fn close_unacknowledged(&mut self) {
        let late: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, c)| c.closing.is_some_and(|t| t.elapsed() >= DISCONNECT_GRACE))
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            warn!(id, "client did not acknowledge the disconnect request");
            self.drop_client(id);
        }
    }

    /// Forget a client. Dropping its queue sender makes the connection task finish.
    fn drop_client(&mut self, id: u64) {
        let Some(c) = self.clients.remove(&id) else {
            return;
        };
        if self.voice_owner == Some(id) {
            self.voice_owner = None;
            self.run_active = false;
            self.command(HaCommand::VoiceSubscribed(false));
        }
        if c.hello {
            info!(peer = %c.peer, "client disconnected");
            self.command(HaCommand::ClientDisconnected);
        }
    }

    fn light_state_msg(&self) -> proto::LightStateResponse {
        let s = &self.light;
        proto::LightStateResponse {
            key: LIGHT_KEY,
            state: s.on,
            brightness: s.brightness,
            color_mode: proto::ColorMode::Rgb as i32,
            color_brightness: 1.0,
            red: s.rgb.0,
            green: s.rgb.1,
            blue: s.rgb.2,
            effect: s.effect.clone().unwrap_or_else(|| "None".into()),
            ..Default::default()
        }
    }

    fn mute_state_msg(&self) -> proto::SwitchStateResponse {
        proto::SwitchStateResponse {
            key: MUTE_KEY,
            state: self.muted,
            ..Default::default()
        }
    }

    fn switch_state_msg(&self, object_id: &str) -> proto::SwitchStateResponse {
        proto::SwitchStateResponse {
            key: entity_key("switch", object_id),
            state: self.switches.get(object_id).copied().unwrap_or(false),
            ..Default::default()
        }
    }

    fn select_state_msg(&self, object_id: &str) -> proto::SelectStateResponse {
        let state = self.selects.get(object_id);
        proto::SelectStateResponse {
            key: entity_key("select", object_id),
            state: state.cloned().unwrap_or_default(),
            missing_state: state.is_none(),
            ..Default::default()
        }
    }

    fn update_state_msg(&self, object_id: &str) -> proto::UpdateStateResponse {
        let state = self.updates.get(object_id).cloned().unwrap_or_default();
        proto::UpdateStateResponse {
            key: entity_key("update", object_id),
            missing_state: state.missing,
            in_progress: state.in_progress,
            has_progress: state.progress.is_some(),
            progress: state.progress.unwrap_or_default(),
            current_version: state.current_version,
            latest_version: state.latest_version,
            title: state.title,
            release_summary: state.release_summary,
            release_url: state.release_url,
            ..Default::default()
        }
    }

    fn number_state_msg(&self, object_id: &str) -> proto::NumberStateResponse {
        let value = self.numbers.get(object_id).copied();
        proto::NumberStateResponse {
            key: entity_key("number", object_id),
            state: value.unwrap_or_default(),
            missing_state: value.is_none(),
            ..Default::default()
        }
    }

    fn text_state_msg(&self, object_id: &str) -> proto::TextSensorStateResponse {
        let value = self.texts.get(object_id);
        proto::TextSensorStateResponse {
            key: entity_key("text_sensor", object_id),
            state: value.cloned().unwrap_or_default(),
            missing_state: value.is_none(),
            ..Default::default()
        }
    }

    fn media_state_msg(&self) -> Option<proto::MediaPlayerStateResponse> {
        let mp = self.cfg.media_player.as_ref()?;
        let (state, volume, muted) = self.media;
        let state = match state {
            MediaPlayerStateKind::Idle => proto::MediaPlayerState::Idle,
            MediaPlayerStateKind::Playing => proto::MediaPlayerState::Playing,
            MediaPlayerStateKind::Paused => proto::MediaPlayerState::Paused,
            MediaPlayerStateKind::Announcing => proto::MediaPlayerState::Announcing,
        };
        Some(proto::MediaPlayerStateResponse {
            key: entity_key("media_player", &mp.object_id),
            state: state as i32,
            volume,
            muted,
            ..Default::default()
        })
    }

    fn device_event(&mut self, ev: DeviceEvent) {
        match ev {
            DeviceEvent::NumberState { object_id, value } => {
                if !self.cfg.numbers.iter().any(|n| n.object_id == object_id) {
                    warn!(%object_id, "state for unknown number dropped");
                    return;
                }
                self.numbers.insert(object_id.clone(), value);
                let m = self.number_state_msg(&object_id);
                self.broadcast(&m, true);
            }
            DeviceEvent::Event {
                object_id,
                event_type,
            } => {
                let Some(e) = self.cfg.events.iter().find(|e| e.object_id == object_id) else {
                    warn!(%object_id, "unknown event entity dropped");
                    return;
                };
                if !e.event_types.contains(&event_type) {
                    warn!(%object_id, %event_type, "unknown event type dropped");
                    return;
                }
                let m = proto::EventResponse {
                    key: entity_key("event", &object_id),
                    event_type,
                    ..Default::default()
                };
                self.broadcast(&m, true);
            }
            DeviceEvent::SwitchState { object_id, on } => {
                if !self.cfg.switches.iter().any(|s| s.object_id == object_id) {
                    warn!(%object_id, "state for unknown switch dropped");
                    return;
                }
                self.switches.insert(object_id.clone(), on);
                let m = self.switch_state_msg(&object_id);
                self.broadcast(&m, true);
            }
            DeviceEvent::SelectState { object_id, state } => {
                let Some(s) = self.cfg.selects.iter().find(|s| s.object_id == object_id) else {
                    warn!(%object_id, "state for unknown select dropped");
                    return;
                };
                if !s.options.contains(&state) {
                    warn!(%object_id, %state, "unknown select option dropped");
                    return;
                }
                self.selects.insert(object_id.clone(), state);
                self.broadcast(&self.select_state_msg(&object_id), true);
            }
            DeviceEvent::UpdateState { object_id, state } => {
                if !self.cfg.updates.iter().any(|u| u.object_id == object_id) {
                    warn!(%object_id, "state for unknown update entity dropped");
                    return;
                }
                self.updates.insert(object_id.clone(), state);
                self.broadcast(&self.update_state_msg(&object_id), true);
            }
            DeviceEvent::TextSensorState { object_id, value } => {
                if !self
                    .cfg
                    .text_sensors
                    .iter()
                    .any(|t| t.object_id == object_id)
                {
                    warn!(%object_id, "state for unknown text sensor dropped");
                    return;
                }
                self.texts.insert(object_id.clone(), value);
                let m = self.text_state_msg(&object_id);
                self.broadcast(&m, true);
            }
            DeviceEvent::ActiveWakeWords(ids) => {
                self.active_wake = self.known_wake_words(ids);
            }
            DeviceEvent::FriendlyName(name) => self.cfg.friendly_name = name,
            DeviceEvent::NodeName(name) => {
                if let Ok(mut shared) = self.name.lock() {
                    shared.clone_from(&name);
                }
                self.cfg.name = name;
            }
            DeviceEvent::Disconnect => {
                let ids: Vec<u64> = self
                    .clients
                    .iter()
                    .filter(|(_, c)| c.hello && c.closing.is_none())
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    // The client acknowledges, then both sides close.
                    self.send(id, &proto::DisconnectRequest::default());
                    if let Some(c) = self.clients.get_mut(&id) {
                        c.closing = Some(Instant::now());
                    }
                }
            }
            DeviceEvent::MediaPlayerState {
                state,
                volume,
                muted,
            } => {
                self.media = (state, volume, muted);
                if let Some(m) = self.media_state_msg() {
                    self.broadcast(&m, true);
                }
            }
            DeviceEvent::LightState {
                on,
                brightness,
                rgb,
                effect,
            } => {
                self.light = LightState {
                    on,
                    brightness,
                    rgb,
                    effect,
                };
                if self.cfg.light.is_some() {
                    let m = self.light_state_msg();
                    self.broadcast(&m, true);
                }
            }
            DeviceEvent::MuteState(m) => {
                self.muted = m;
                if self.cfg.mute_switch {
                    let m = self.mute_state_msg();
                    self.broadcast(&m, true);
                }
            }
            DeviceEvent::VoiceStart {
                conversation_id,
                wake_word_phrase,
                use_vad,
            } => {
                let Some(owner) = self.voice_owner else {
                    warn!("voice start dropped: no client subscribed to the voice assistant");
                    return;
                };
                self.run_active = true;
                self.send(
                    owner,
                    &proto::VoiceAssistantRequest {
                        start: true,
                        conversation_id,
                        flags: u32::from(use_vad),
                        wake_word_phrase: wake_word_phrase.unwrap_or_default(),
                        ..Default::default()
                    },
                );
            }
            DeviceEvent::VoiceAudio(data) => {
                if let (Some(owner), true) = (self.voice_owner, self.run_active) {
                    self.send(
                        owner,
                        &proto::VoiceAssistantAudio {
                            data,
                            end: false,
                            ..Default::default()
                        },
                    );
                }
            }
            DeviceEvent::VoiceAudioEnd => {
                if let (Some(owner), true) = (self.voice_owner, self.run_active) {
                    self.run_active = false;
                    self.send(
                        owner,
                        &proto::VoiceAssistantAudio {
                            end: true,
                            ..Default::default()
                        },
                    );
                }
            }
            DeviceEvent::VoiceStop => {
                self.run_active = false;
                if let Some(owner) = self.voice_owner {
                    self.send(
                        owner,
                        &proto::VoiceAssistantRequest {
                            start: false,
                            ..Default::default()
                        },
                    );
                }
            }
            DeviceEvent::AnnounceFinished { success } => {
                if let Some(owner) = self.voice_owner {
                    self.send(owner, &proto::VoiceAssistantAnnounceFinished { success });
                }
            }
            DeviceEvent::RequestTime => self.broadcast(&proto::GetTimeRequest {}, false),
        }
    }

    fn frame(&mut self, id: u64, frame: Frame) {
        if let Err(reason) = self.handle(id, frame) {
            warn!(id, "dropping client: {reason}");
            self.drop_client(id);
        }
    }

    fn handle(&mut self, id: u64, frame: Frame) -> Result<(), String> {
        let Some(hello) = self.clients.get(&id).map(|c| c.hello) else {
            return Ok(());
        };
        let p = &frame.payload[..];
        macro_rules! parse {
            ($t:ty) => {
                <$t>::decode(p).map_err(|e| format!("bad {}: {e}", stringify!($t)))?
            };
        }
        if frame.id == proto::HelloRequest::ID {
            if hello {
                return Err("duplicate hello".into());
            }
            let req = parse!(proto::HelloRequest);
            let Some(c) = self.clients.get_mut(&id) else {
                return Ok(());
            };
            c.hello = true;
            let peer = c.peer.clone();
            info!(%peer, client = %req.client_info, "client connected");
            self.send(
                id,
                &proto::HelloResponse {
                    api_version_major: API_VERSION.0,
                    api_version_minor: API_VERSION.1,
                    server_info: format!("Pinna {}", env!("CARGO_PKG_VERSION")),
                    name: self.cfg.name.clone(),
                },
            );
            self.command(HaCommand::ClientConnected { peer });
            return Ok(());
        }
        // Legacy auth is obsolete; the pinned client never sends it.
        if frame.id == proto::AuthenticationRequest::ID {
            return Ok(());
        }
        if !hello {
            return Err(format!("message {} before hello", frame.id));
        }
        match frame.id {
            proto::PingRequest::ID => self.send(id, &proto::PingResponse {}),
            proto::PingResponse::ID => {}
            proto::DisconnectResponse::ID => self.drop_client(id),
            proto::DisconnectRequest::ID => {
                self.send(id, &proto::DisconnectResponse {});
                // Queue is drained before the connection task sees the close.
                self.drop_client(id);
            }
            proto::NoiseEncryptionSetKeyRequest::ID => {
                let r = parse!(proto::NoiseEncryptionSetKeyRequest);
                let encrypted = self.clients.get(&id).is_some_and(|c| c.encrypted);
                // The first key from anyone (ADR 0018); a new one only over a
                // connection encrypted with the current key.
                let provisioning = self.cfg.encryption_key.is_none();
                let key = set_key_value(&r.key).filter(|_| encrypted || provisioning);
                if let Some(key) = key {
                    if let Ok(mut k) = self.key.lock() {
                        *k = Some(key);
                    }
                    self.cfg.encryption_key = Some(key);
                    self.command(HaCommand::EncryptionKeySet(key));
                    info!(id, provisioning, "encryption key set");
                } else {
                    warn!(id, "rejecting encryption key");
                }
                self.send(
                    id,
                    &proto::NoiseEncryptionSetKeyResponse {
                        success: key.is_some(),
                    },
                );
                if key.is_some() && provisioning {
                    // Plaintext and all-zero connections end; Home Assistant
                    // reconnects with the key. Queues drain before they close.
                    let open: Vec<u64> = self
                        .clients
                        .iter()
                        .filter(|(_, c)| !c.encrypted)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in open {
                        self.drop_client(id);
                    }
                }
            }
            proto::DeviceInfoRequest::ID => self.device_info(id),
            proto::ListEntitiesRequest::ID => self.list_entities(id),
            proto::SubscribeStatesRequest::ID => {
                if let Some(c) = self.clients.get_mut(&id) {
                    c.states = true;
                }
                if self.cfg.light.is_some() {
                    let m = self.light_state_msg();
                    self.send(id, &m);
                }
                if self.cfg.mute_switch {
                    let m = self.mute_state_msg();
                    self.send(id, &m);
                }
                let ids: Vec<String> = self
                    .cfg
                    .switches
                    .iter()
                    .map(|s| s.object_id.clone())
                    .collect();
                for o in ids {
                    let m = self.switch_state_msg(&o);
                    self.send(id, &m);
                }
                for i in 0..self.cfg.selects.len() {
                    let m = self.select_state_msg(&self.cfg.selects[i].object_id);
                    self.send(id, &m);
                }
                for i in 0..self.cfg.updates.len() {
                    let m = self.update_state_msg(&self.cfg.updates[i].object_id);
                    self.send(id, &m);
                }
                let ids: Vec<String> = self
                    .cfg
                    .numbers
                    .iter()
                    .map(|n| n.object_id.clone())
                    .collect();
                for o in ids {
                    let m = self.number_state_msg(&o);
                    self.send(id, &m);
                }
                let ids: Vec<String> = self
                    .cfg
                    .text_sensors
                    .iter()
                    .map(|t| t.object_id.clone())
                    .collect();
                for o in ids {
                    let m = self.text_state_msg(&o);
                    self.send(id, &m);
                }
                if let Some(m) = self.media_state_msg() {
                    self.send(id, &m);
                }
            }
            proto::NumberCommandRequest::ID => {
                let r = parse!(proto::NumberCommandRequest);
                let Some(n) = self
                    .cfg
                    .numbers
                    .iter()
                    .find(|n| entity_key("number", &n.object_id) == r.key)
                else {
                    return Err(format!("number command for unknown key {}", r.key));
                };
                let object_id = n.object_id.clone();
                self.command(HaCommand::Number {
                    object_id,
                    value: r.state,
                });
            }
            proto::ButtonCommandRequest::ID => {
                let r = parse!(proto::ButtonCommandRequest);
                let Some(b) = self
                    .cfg
                    .buttons
                    .iter()
                    .find(|b| entity_key("button", &b.object_id) == r.key)
                else {
                    return Err(format!("button command for unknown key {}", r.key));
                };
                let object_id = b.object_id.clone();
                self.command(HaCommand::Button { object_id });
            }
            proto::MediaPlayerCommandRequest::ID => {
                let r = parse!(proto::MediaPlayerCommandRequest);
                let known = self
                    .cfg
                    .media_player
                    .as_ref()
                    .is_some_and(|m| entity_key("media_player", &m.object_id) == r.key);
                if !known {
                    return Err(format!("media player command for unknown key {}", r.key));
                }
                self.command(HaCommand::MediaPlayer {
                    command: r
                        .has_command
                        .then(|| MediaPlayerCommand::from_i32(r.command)),
                    volume: r.has_volume.then_some(r.volume),
                    media_url: r.has_media_url.then_some(r.media_url),
                    announcement: r.has_announcement && r.announcement,
                });
            }
            proto::LightCommandRequest::ID => {
                let r = parse!(proto::LightCommandRequest);
                if r.key != LIGHT_KEY || self.cfg.light.is_none() {
                    return Err(format!("light command for unknown key {}", r.key));
                }
                self.command(HaCommand::Light {
                    on: r.has_state.then_some(r.state),
                    brightness: r.has_brightness.then_some(r.brightness),
                    rgb: r.has_rgb.then_some((r.red, r.green, r.blue)),
                    effect: r.has_effect.then_some(r.effect),
                });
            }
            proto::SwitchCommandRequest::ID => {
                let r = parse!(proto::SwitchCommandRequest);
                if r.key == MUTE_KEY && self.cfg.mute_switch {
                    self.command(HaCommand::MuteSwitch(r.state));
                } else if let Some(s) = self
                    .cfg
                    .switches
                    .iter()
                    .find(|s| entity_key("switch", &s.object_id) == r.key)
                {
                    let object_id = s.object_id.clone();
                    self.command(HaCommand::Switch {
                        object_id,
                        on: r.state,
                    });
                } else {
                    return Err(format!("switch command for unknown key {}", r.key));
                }
            }
            proto::SelectCommandRequest::ID => {
                let r = parse!(proto::SelectCommandRequest);
                let Some(s) = self
                    .cfg
                    .selects
                    .iter()
                    .find(|s| entity_key("select", &s.object_id) == r.key)
                else {
                    return Err(format!("select command for unknown key {}", r.key));
                };
                if !s.options.contains(&r.state) {
                    return Err(format!(
                        "unknown option {:?} for select {}",
                        r.state, s.object_id
                    ));
                }
                self.command(HaCommand::Select {
                    object_id: s.object_id.clone(),
                    state: r.state,
                });
            }
            proto::UpdateCommandRequest::ID => {
                let r = parse!(proto::UpdateCommandRequest);
                let command = match proto::UpdateCommand::try_from(r.command) {
                    Ok(proto::UpdateCommand::None) => return Ok(()),
                    Ok(proto::UpdateCommand::Update) => UpdateCommand::Install,
                    Ok(proto::UpdateCommand::Check) => UpdateCommand::Check,
                    Err(_) => return Err(format!("unknown update command {}", r.command)),
                };
                let Some(u) = self
                    .cfg
                    .updates
                    .iter()
                    .find(|u| entity_key("update", &u.object_id) == r.key)
                else {
                    return Err(format!("update command for unknown key {}", r.key));
                };
                self.command(HaCommand::Update {
                    object_id: u.object_id.clone(),
                    command,
                });
            }
            proto::GetTimeResponse::ID => {
                let r = parse!(proto::GetTimeResponse);
                self.command(HaCommand::Time {
                    epoch_seconds: r.epoch_seconds,
                });
            }
            proto::SubscribeVoiceAssistantRequest::ID => {
                let r = parse!(proto::SubscribeVoiceAssistantRequest);
                if !r.subscribe {
                    if self.voice_owner == Some(id) {
                        self.voice_owner = None;
                        self.run_active = false;
                        self.command(HaCommand::VoiceSubscribed(false));
                    }
                    return Ok(());
                }
                // aioesphomeapi sets 1<<2 for API audio (the proto enum says 1); take either.
                if r.flags & (1 | 4) == 0 {
                    return Err("voice subscription without API audio".into());
                }
                if self.voice_owner.is_some_and(|o| o != id) {
                    return Err("voice assistant already has a subscriber".into());
                }
                self.voice_owner = Some(id);
                self.command(HaCommand::VoiceSubscribed(true));
            }
            proto::VoiceAssistantResponse::ID => {
                let r = parse!(proto::VoiceAssistantResponse);
                if self.voice_owner == Some(id) {
                    if r.error {
                        self.run_active = false;
                    }
                    self.command(HaCommand::VoiceStarted {
                        port: r.port,
                        error: r.error,
                    });
                }
            }
            proto::VoiceAssistantEventResponse::ID => {
                let r = parse!(proto::VoiceAssistantEventResponse);
                if self.voice_owner == Some(id) {
                    self.command(HaCommand::VoiceEvent {
                        event: VoiceEventType::from_i32(r.event_type),
                        data: r.data.into_iter().map(|d| (d.name, d.value)).collect(),
                    });
                }
            }
            proto::VoiceAssistantTimerEventResponse::ID => {
                let r = parse!(proto::VoiceAssistantTimerEventResponse);
                if self.voice_owner == Some(id) {
                    self.command(HaCommand::Timer {
                        event: TimerEventType::from_i32(r.event_type),
                        id: r.timer_id,
                        name: r.name,
                        total_seconds: r.total_seconds,
                        seconds_left: r.seconds_left,
                        is_active: r.is_active,
                    });
                }
            }
            proto::VoiceAssistantAnnounceRequest::ID => {
                let r = parse!(proto::VoiceAssistantAnnounceRequest);
                if self.voice_owner == Some(id) {
                    self.command(HaCommand::Announce {
                        media_id: r.media_id,
                        text: r.text,
                        preannounce_media_id: r.preannounce_media_id,
                        start_conversation: r.start_conversation,
                    });
                }
            }
            proto::VoiceAssistantConfigurationRequest::ID => {
                let _ = parse!(proto::VoiceAssistantConfigurationRequest);
                self.send_wake_config(id);
            }
            proto::VoiceAssistantSetConfiguration::ID => {
                let r = parse!(proto::VoiceAssistantSetConfiguration);
                let active = self.known_wake_words(r.active_wake_words);
                self.active_wake = active.clone();
                self.send_wake_config(id);
                self.command(HaCommand::SetActiveWakeWords(active));
            }
            other => {
                debug!(id, msg = other, name = ?crate::proto::message_name(other), "ignoring message")
            }
        }
        Ok(())
    }

    /// Known wake word ids only, without repeats, at most `max_active_wake_words`.
    fn known_wake_words(&self, ids: Vec<String>) -> Vec<String> {
        let mut active: Vec<String> = Vec::new();
        for w in ids {
            if self.cfg.wake_words.iter().any(|k| k.id == w) && !active.contains(&w) {
                active.push(w);
            } else {
                warn!(wake_word = %w, "ignoring unknown or duplicate wake word");
            }
        }
        active.truncate(self.cfg.max_active_wake_words as usize);
        active
    }

    fn send_wake_config(&mut self, id: u64) {
        let m = proto::VoiceAssistantConfigurationResponse {
            available_wake_words: self
                .cfg
                .wake_words
                .iter()
                .map(|w| proto::VoiceAssistantWakeWord {
                    id: w.id.clone(),
                    wake_word: w.wake_word.clone(),
                    trained_languages: w.trained_languages.clone(),
                })
                .collect(),
            active_wake_words: self.active_wake.clone(),
            max_active_wake_words: self.cfg.max_active_wake_words,
        };
        self.send(id, &m);
    }

    fn device_info(&mut self, id: u64) {
        let c = &self.cfg;
        let m = proto::DeviceInfoResponse {
            name: c.name.clone(),
            mac_address: c.mac_string(),
            esphome_version: ESPHOME_VERSION.into(),
            model: c.model.clone(),
            project_name: c.project_name.clone(),
            project_version: c.project_version.clone(),
            manufacturer: c.manufacturer.clone(),
            friendly_name: c.friendly_name.clone(),
            suggested_area: c.suggested_area.clone().unwrap_or_default(),
            voice_assistant_feature_flags: VOICE_FEATURE_FLAGS,
            api_encryption_supported: true,
            api_encryption_provisionable: c.encryption_key.is_none(),
            webserver_port: c.webserver_port.into(),
            ..Default::default()
        };
        self.send(id, &m);
    }

    fn list_entities(&mut self, id: u64) {
        if let Some(l) = self.cfg.light.clone() {
            self.send(
                id,
                &proto::ListEntitiesLightResponse {
                    object_id: l.object_id,
                    key: LIGHT_KEY,
                    name: l.name,
                    supported_color_modes: vec![proto::ColorMode::Rgb as i32],
                    effects: l.effects,
                    icon: "mdi:led-ring".into(),
                    ..Default::default()
                },
            );
        }
        if self.cfg.mute_switch {
            self.send(
                id,
                &proto::ListEntitiesSwitchResponse {
                    object_id: "microphone_mute".into(),
                    key: MUTE_KEY,
                    name: "Microphone mute".into(),
                    icon: "mdi:microphone-off".into(),
                    ..Default::default()
                },
            );
        }
        let cat = |c: EntityCategory| match c {
            EntityCategory::None => proto::EntityCategory::None,
            EntityCategory::Config => proto::EntityCategory::Config,
            EntityCategory::Diagnostic => proto::EntityCategory::Diagnostic,
        } as i32;
        for s in self.cfg.switches.clone() {
            self.send(
                id,
                &proto::ListEntitiesSwitchResponse {
                    key: entity_key("switch", &s.object_id),
                    object_id: s.object_id,
                    name: s.name,
                    icon: s.icon,
                    entity_category: cat(s.entity_category),
                    ..Default::default()
                },
            );
        }
        for s in self.cfg.selects.clone() {
            self.send(
                id,
                &proto::ListEntitiesSelectResponse {
                    key: entity_key("select", &s.object_id),
                    object_id: s.object_id,
                    name: s.name,
                    icon: s.icon,
                    options: s.options,
                    entity_category: cat(s.entity_category),
                    ..Default::default()
                },
            );
        }
        for u in self.cfg.updates.clone() {
            self.send(
                id,
                &proto::ListEntitiesUpdateResponse {
                    key: entity_key("update", &u.object_id),
                    object_id: u.object_id,
                    name: u.name,
                    icon: u.icon,
                    device_class: u.device_class.unwrap_or_default(),
                    entity_category: cat(u.entity_category),
                    ..Default::default()
                },
            );
        }
        for n in self.cfg.numbers.clone() {
            self.send(
                id,
                &proto::ListEntitiesNumberResponse {
                    key: entity_key("number", &n.object_id),
                    object_id: n.object_id,
                    name: n.name,
                    icon: n.icon,
                    min_value: n.min,
                    max_value: n.max,
                    step: n.step,
                    entity_category: cat(n.entity_category),
                    unit_of_measurement: n.unit.unwrap_or_default(),
                    mode: match n.mode {
                        NumberMode::Auto => proto::NumberMode::Auto,
                        NumberMode::Box => proto::NumberMode::Box,
                        NumberMode::Slider => proto::NumberMode::Slider,
                    } as i32,
                    ..Default::default()
                },
            );
        }
        for b in self.cfg.buttons.clone() {
            self.send(
                id,
                &proto::ListEntitiesButtonResponse {
                    key: entity_key("button", &b.object_id),
                    object_id: b.object_id,
                    name: b.name,
                    icon: b.icon,
                    ..Default::default()
                },
            );
        }
        for e in self.cfg.events.clone() {
            self.send(
                id,
                &proto::ListEntitiesEventResponse {
                    key: entity_key("event", &e.object_id),
                    object_id: e.object_id,
                    name: e.name,
                    icon: e.icon,
                    device_class: e.device_class.unwrap_or_default(),
                    event_types: e.event_types,
                    ..Default::default()
                },
            );
        }
        for t in self.cfg.text_sensors.clone() {
            self.send(
                id,
                &proto::ListEntitiesTextSensorResponse {
                    key: entity_key("text_sensor", &t.object_id),
                    object_id: t.object_id,
                    name: t.name,
                    icon: t.icon,
                    ..Default::default()
                },
            );
        }
        if let Some(m) = self.cfg.media_player.clone() {
            #[allow(deprecated)]
            let msg = proto::ListEntitiesMediaPlayerResponse {
                key: entity_key("media_player", &m.object_id),
                object_id: m.object_id,
                name: m.name,
                icon: m.icon,
                supports_pause: m.supports_pause,
                feature_flags: m.feature_flags,
                supported_formats: m
                    .supported_formats
                    .into_iter()
                    .map(|f| proto::MediaPlayerSupportedFormat {
                        format: f.format,
                        sample_rate: f.sample_rate,
                        num_channels: f.num_channels,
                        purpose: match f.purpose {
                            MediaPurpose::Default => proto::MediaPlayerFormatPurpose::Default,
                            MediaPurpose::Announcement => {
                                proto::MediaPlayerFormatPurpose::Announcement
                            }
                        } as i32,
                        sample_bytes: f.sample_bytes,
                    })
                    .collect(),
                ..Default::default()
            };
            self.send(id, &msg);
        }
        self.send(id, &proto::ListEntitiesDoneResponse {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{echo_dot_entities, types::ButtonConfig};

    #[test]
    fn set_key_takes_base64_or_raw_but_never_zeros() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let key: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
        assert_eq!(set_key_value(STANDARD.encode(key).as_bytes()), Some(key));
        assert_eq!(set_key_value(&key), Some(key));
        assert_eq!(set_key_value(STANDARD.encode([0u8; 32]).as_bytes()), None);
        assert_eq!(set_key_value(&[0u8; 32]), None);
        assert_eq!(set_key_value(b""), None);
        assert_eq!(set_key_value(b"short"), None);
    }

    #[test]
    fn echo_profile_keys_are_unique() {
        let mut cfg = DeviceConfig::new("t", [0; 6]);
        echo_dot_entities(&mut cfg);
        check_keys(&cfg).unwrap();
    }

    #[test]
    fn duplicate_object_id_is_a_collision() {
        let mut cfg = DeviceConfig::new("t", [0; 6]);
        let b = ButtonConfig {
            object_id: "x".into(),
            name: "X".into(),
            icon: String::new(),
        };
        cfg.buttons = vec![b.clone(), b];
        assert!(check_keys(&cfg).is_err());
    }

    /// mDNS announces the satellite's IPv6 addresses too, and Home Assistant
    /// stores whichever one it picks as the host; an IPv4-only listener
    /// refuses it.
    #[tokio::test]
    async fn default_bind_accepts_ipv4_and_ipv6() {
        let mut cfg = DeviceConfig::new("t", [0; 6]);
        cfg.port = 0;
        let (handle, _cmds) = serve(cfg).await.unwrap();
        let port = handle.local_addr().port();
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tokio::net::TcpStream::connect(("::1", port)).await.unwrap();
    }

    #[test]
    fn keys_are_per_platform() {
        assert_ne!(
            entity_key("event", "microphone_mute"),
            entity_key("switch", "microphone_mute")
        );
        assert_eq!(
            entity_key("number", "volume"),
            entity_key("number", "volume")
        );
    }
}
