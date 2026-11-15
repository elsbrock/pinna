//! TCP server and the single actor that owns all protocol state.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
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
    frame::{Frame, FrameCodec, Plaintext},
    proto::{ApiMessage, proto},
    types::{
        DeviceConfig, DeviceEvent, ESPHOME_VERSION, HaCommand, LightState, TimerEventType,
        VOICE_FEATURE_FLAGS, VoiceEventType,
    },
};

/// Entity key of the microphone mute switch.
pub const MUTE_KEY: u32 = 2;
/// Entity key of the light.
pub const LIGHT_KEY: u32 = 3;

const MAX_CLIENTS: usize = 2;
const API_VERSION: (u32, u32) = (1, 14);
/// Close a connection that stayed silent this long (after hello).
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Probe a connection that stayed silent this long.
const PING_AFTER: Duration = Duration::from_secs(60);
/// Minimum spacing between our own pings.
const PING_EVERY: Duration = Duration::from_secs(30);
/// Time a new connection has to send its hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
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
    tx: mpsc::Sender<Input>,
    addr: SocketAddr,
}

impl EsphomeHandle {
    /// Send an event; waits if the server is momentarily backed up.
    pub async fn send(&self, ev: DeviceEvent) -> Result<(), Stopped> {
        self.tx.send(Input::Device(ev)).await.map_err(|_| Stopped)
    }

    /// Non-blocking variant; fails if the queue is full or the server stopped.
    pub fn try_send(&self, ev: DeviceEvent) -> Result<(), Stopped> {
        self.tx.try_send(Input::Device(ev)).map_err(|_| Stopped)
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
    let listener = TcpListener::bind((cfg.bind, cfg.port)).await?;
    let addr = listener.local_addr()?;
    info!(%addr, name = %cfg.name, "esphome api listening");
    let (in_tx, in_rx) = mpsc::channel(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let actor = Actor::new(cfg, cmd_tx);
    tokio::spawn(actor.run(in_rx));
    tokio::spawn(accept_loop(listener, in_tx.clone()));
    Ok((EsphomeHandle { tx: in_tx, addr }, cmd_rx))
}

async fn accept_loop(listener: TcpListener, inputs: mpsc::Sender<Input>) {
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
        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);
            run_client(id, stream, peer, inputs).await;
            live.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Per-connection I/O: framing, idle detection and keep-alive pings.
async fn run_client(id: u64, mut stream: TcpStream, peer: SocketAddr, inputs: mpsc::Sender<Input>) {
    let (out_tx, mut out_rx) = mpsc::channel::<(u32, Bytes)>(OUT_QUEUE);
    if inputs
        .send(Input::Connected {
            id,
            peer: peer.to_string(),
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
    hello: bool,
    states: bool,
}

struct Actor {
    cfg: DeviceConfig,
    cmds: mpsc::Sender<HaCommand>,
    clients: HashMap<u64, Client>,
    voice_owner: Option<u64>,
    run_active: bool,
    light: LightState,
    muted: bool,
    active_wake: Vec<String>,
}

impl Actor {
    fn new(cfg: DeviceConfig, cmds: mpsc::Sender<HaCommand>) -> Self {
        let active_wake = cfg.active_wake_words.clone();
        Self {
            cfg,
            cmds,
            clients: HashMap::new(),
            voice_owner: None,
            run_active: false,
            light: LightState::default(),
            muted: false,
            active_wake,
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Input>) {
        while let Some(input) = rx.recv().await {
            match input {
                Input::Device(ev) => self.device_event(ev),
                Input::Connected { id, peer, out } => {
                    self.clients.insert(
                        id,
                        Client {
                            out,
                            peer,
                            hello: false,
                            states: false,
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

    fn device_event(&mut self, ev: DeviceEvent) {
        match ev {
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
            proto::DisconnectRequest::ID => {
                self.send(id, &proto::DisconnectResponse {});
                // Queue is drained before the connection task sees the close.
                self.drop_client(id);
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
                if r.key != MUTE_KEY || !self.cfg.mute_switch {
                    return Err(format!("switch command for unknown key {}", r.key));
                }
                self.command(HaCommand::MuteSwitch(r.state));
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
                let mut active: Vec<String> = Vec::new();
                for w in r.active_wake_words {
                    if self.cfg.wake_words.iter().any(|k| k.id == w) && !active.contains(&w) {
                        active.push(w);
                    } else {
                        warn!(wake_word = %w, "ignoring unknown or duplicate wake word");
                    }
                }
                active.truncate(self.cfg.max_active_wake_words as usize);
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
        self.send(id, &proto::ListEntitiesDoneResponse {});
    }
}
