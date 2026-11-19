//! One Sendspin connection from the player's side, without I/O (ADR 0027 §2–§6, §9, §14, §15).
//!
//! `pinnad` accepts the WebSocket, makes a [`Connection`] and then only moves bytes and time:
//! it passes every text and binary message in with [`Connection::on_text`] and
//! [`Connection::on_binary`], calls [`Connection::on_tick`] at [`Connection::next_wakeup`],
//! sends what [`Connection::poll_transmit`] returns, in order and at once, and acts on
//! [`Connection::poll_event`]. Times are µs on the monotonic clock, the one the speaker's
//! timestamps use. A message's time is when it arrived, as close to the socket as possible.
//!
//! The connection runs the handshake as the Noise responder, admits or refuses each
//! `server/activate` by the specification's table for the key the session was keyed with,
//! measures the server's clock, reports the player's state when the specification allows, and
//! carries out the Pairing PSK method. It closes, with the goodbye the specification names or
//! silently, on everything the specification calls a failure, and on a server that sends more
//! audio than `buffer_capacity` or takes longer than 30 s for a step of the handshake or the
//! first activation (ADR 0027 §6).
//!
//! A second server may connect while one is admitted (ADR 0027 §3). Its connection stays
//! provisional until its first `server/activate`, which is judged against the admitted
//! connection. Before passing a message to a connection that is not yet activated, `pinnad`
//! calls [`Connection::set_rival`] with the admitted connection's [`Connection::as_rival`] and the
//! last-playback server from its own store: the admitted connection's activities and pairing
//! attempt change without an event (a pairing that times out, the server's `pair/abort`), and
//! the last-playback server changes when the admitted connection plays. Running every
//! connection in one task keeps the two in step. `pinnad` also keeps fewer paired connections
//! open than [`crate::pairing::MAX_PAIRINGS`], so a new pairing never has to evict the record
//! of an open one.

use std::collections::VecDeque;

use serde::Serialize;

use crate::b64;
use crate::budget::Budget;
use crate::clock::{Bursts, TimeFilter};
use crate::frame::{self, AUDIO_HEADER_LEN, JSON_MAX, Reassembler, TYPE_AUDIO, TYPE_JSON};
use crate::identity::Identity;
use crate::messages::{
    self, AudioFormat, BUFFER_MS_MAX, ClientCommand, ClientGoodbye, ClientHello, ClientInit,
    ClientPairAbort, ClientPairFinalize, ClientPairInit, ClientState, ClientTime,
    ControllerCommandKind, ControllerCommandObject, ControllerState, DeviceInfo, Empty,
    GoodbyeReason, GroupUpdate, Metadata, NAME_MAX, NoiseHandshake, PairAbortReason, PairMethods,
    PairingParams, PlayerCommand, PlayerCommandKind, PlayerStateObject, PlayerSupport,
    ServerActivate, ServerCommand, ServerHello, ServerMessage, ServerState, StreamClear, StreamEnd,
    StreamFormat, UnpairedAccess, VOLUME_MAX,
};
use crate::noise::{self, Responder, SUITE, Transport};
use crate::pairing::{Finalize, PairingFlow, PairingRecord, PairingStore};
use crate::psk::PskCategory;

/// The longest wait for the server's next handshake message, and for the first
/// `server/activate` after the handshake (ADR 0027 §6).
pub const STEP_TIMEOUT_US: i64 = 30_000_000;
/// Audio due further ahead than this is dropped (ADR 0027 §6).
pub const MAX_AHEAD_US: i64 = 30_000_000;
/// The fastest the server's clock may drift from the player's, as a fraction: twice what the
/// media lane follows (±0.5 %, ADR 0027 §11). The server's `server/time` replies set the
/// filter's drift, with no bound of its own; at -1 the conversion to the player's clock would
/// divide by zero, and below it run backward, so that audio due later would count as already
/// played. A server whose clock the filter finds drifting faster is closed.
pub const MAX_DRIFT: f64 = 0.01;

/// A role the player can take, each at version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Player,
    Controller,
    Metadata,
}

impl Role {
    pub fn id(self) -> &'static str {
        match self {
            Self::Player => "player@v1",
            Self::Controller => "controller@v1",
            Self::Metadata => "metadata@v1",
        }
    }
}

/// What the player says about itself.
#[derive(Debug, Clone)]
pub struct Config {
    pub identity: Identity,
    /// Plain text, at most 64 bytes. Should match the mDNS record's `name`.
    pub name: String,
    pub device_info: Option<DeviceInfo>,
    /// In priority order, each at most once.
    pub roles: Vec<Role>,
    /// In priority order, the preferred first. Needed for the player role.
    pub formats: Vec<AudioFormat>,
    /// Bytes of audio, 13-byte header and payload per chunk, the server may have outstanding.
    pub buffer_capacity: u32,
    /// Whether a server without a pairing may play (ADR 0027 §4).
    pub unpaired_access: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("the name must be at most 64 bytes")]
    Name,
    #[error("device_info.{0} must be at most 64 bytes")]
    DeviceInfo(&'static str),
    #[error("device_info.mac_address must be six lowercase hex pairs separated by colons")]
    MacAddress,
    #[error("each role may be listed once")]
    DuplicateRole,
    #[error("the player role needs at least one format")]
    NoFormats,
    #[error("client/hello would exceed 16384 bytes")]
    HelloTooLarge,
    #[error(transparent)]
    Encode(#[from] messages::MessageError),
}

/// What the player reports in `client/state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerState {
    /// False while something outside Sendspin holds the speaker and will not yield.
    pub available: bool,
    /// 0 to 100.
    pub volume: u8,
    pub muted: bool,
    pub required_lead_time_ms: u32,
    /// 0 to 5000.
    pub min_buffer_ms: u32,
    /// One of the configured formats, or `None` for no preference.
    pub format: Option<AudioFormat>,
    /// The commands the server may send.
    pub supported_commands: Vec<PlayerCommandKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    #[error("volume must be 0 to 100")]
    Volume,
    #[error("min_buffer_ms must be 0 to 5000")]
    MinBuffer,
    #[error("the preferred format must be one of the configured formats")]
    Format,
}

/// A command for the group, through the controller role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerCommand {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Volume(u8),
    Mute(bool),
    RepeatOff,
    RepeatOne,
    RepeatAll,
    Shuffle,
    Unshuffle,
    Switch,
    Seek { position_ms: i64 },
    SeekRelative { offset_ms: i64 },
}

impl ControllerCommand {
    pub fn kind(self) -> ControllerCommandKind {
        use ControllerCommandKind as K;
        match self {
            Self::Play => K::Play,
            Self::Pause => K::Pause,
            Self::Stop => K::Stop,
            Self::Next => K::Next,
            Self::Previous => K::Previous,
            Self::Volume(_) => K::Volume,
            Self::Mute(_) => K::Mute,
            Self::RepeatOff => K::RepeatOff,
            Self::RepeatOne => K::RepeatOne,
            Self::RepeatAll => K::RepeatAll,
            Self::Shuffle => K::Shuffle,
            Self::Unshuffle => K::Unshuffle,
            Self::Switch => K::Switch,
            Self::Seek { .. } => K::Seek,
            Self::SeekRelative { .. } => K::SeekRelative,
        }
    }

    fn object(self) -> ControllerCommandObject {
        let mut object = ControllerCommandObject {
            command: self.kind(),
            volume: None,
            mute: None,
            position_ms: None,
            offset_ms: None,
        };
        match self {
            Self::Volume(v) => object.volume = Some(v),
            Self::Mute(m) => object.mute = Some(m),
            Self::Seek { position_ms } => object.position_ms = Some(position_ms),
            Self::SeekRelative { offset_ms } => object.offset_ms = Some(offset_ms),
            _ => {}
        }
        object
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("the connection cannot send it now")]
    NotActive,
    #[error("the server does not accept {0:?} now")]
    NotSupported(ControllerCommandKind),
    #[error("the command's value is out of range")]
    OutOfRange,
}

/// The purposes a server declares for a connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activities {
    pub playback: bool,
    pub pairing: bool,
}

impl Activities {
    /// `None` for an activity this player does not know, or one listed twice: no allowed set.
    fn parse(list: &[String]) -> Option<Self> {
        let mut activities = Self::default();
        for item in list {
            let slot = match item.as_str() {
                "playback" => &mut activities.playback,
                "pairing" => &mut activities.pairing,
                _ => return None,
            };
            if *slot {
                return None;
            }
            *slot = true;
        }
        Some(activities)
    }

    /// Playback ranks above pairing, pairing above nothing.
    fn rank(self) -> u8 {
        if self.playback {
            2
        } else if self.pairing {
            1
        } else {
            0
        }
    }
}

/// The roles a server has activated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActiveRoles {
    pub player: bool,
    pub controller: bool,
    pub metadata: bool,
}

impl ActiveRoles {
    /// Roles the player did not offer are left out.
    fn from_list(list: &[String], offered: &[Role]) -> Self {
        let active = |role: Role| offered.contains(&role) && list.iter().any(|r| r == role.id());
        Self {
            player: active(Role::Player),
            controller: active(Role::Controller),
            metadata: active(Role::Metadata),
        }
    }
}

/// The connection already admitted, as a newcomer's first activation is judged against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rival {
    pub server_id: String,
    pub activities: Activities,
    /// A pairing attempt in progress is never displaced.
    pub pairing_attempt: bool,
}

/// The player stream a `stream/start` begins or changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerStream {
    pub format: AudioFormat,
    /// For FLAC: `fLaC` and the STREAMINFO block.
    pub codec_header: Option<Vec<u8>>,
    /// The stream was already running: the new format applies from the next chunk.
    pub in_place: bool,
    /// When the server sent it; `required_lead_time_ms` is measured from here.
    pub server_transmitted_us: i64,
}

/// One chunk of encoded audio. The player's count against `buffer_capacity` drops a chunk once
/// its first sample is due, so `pinnad` bounds how long a chunk lasts when it decodes it: the
/// specification says 15 to 150 ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioChunk {
    /// Server time in µs at which the first sample is to be heard. Convert it with
    /// [`Connection::clock`].
    pub timestamp_us: i64,
    /// The lead the server had when it sent the chunk, for measuring arrival delay. `None`
    /// when the server could not measure one.
    pub send_ahead_us: Option<u32>,
    /// When the chunk arrived, on the player's clock.
    pub arrival_us: i64,
    pub data: Vec<u8>,
}

/// What happened, for `pinnad` to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The handshake is done and the server introduced itself.
    Connected {
        server_id: String,
        name: String,
    },
    /// A `server/activate` was admitted. `displaces_rival`: the connection set with
    /// [`Connection::set_rival`] lost and must be ended with [`Connection::displace`].
    Activated {
        activities: Activities,
        roles: ActiveRoles,
        paired: bool,
        displaces_rival: bool,
    },
    StreamStart(PlayerStream),
    /// Drop queued audio, all of it or from this server time on.
    StreamClear {
        from_timestamp: Option<i64>,
    },
    /// Stop the player's output and drop its queue.
    StreamEnd,
    Audio(AudioChunk),
    /// The server set the volume, 0 to 100. Already reported back.
    Volume(u8),
    /// The server set mute. Already reported back.
    Mute(bool),
    /// What is playing now, or `None` once the metadata role is gone.
    Metadata(Option<Metadata>),
    /// The group's controls, or `None` once the controller role is gone.
    ControllerState(Option<ControllerState>),
    Group(GroupUpdate),
    /// The clock filter took a new measurement.
    ClockUpdated,
    /// A pairing completed: keep the record, with [`PairingStore::insert`] given the servers of
    /// every open connection.
    PairingAdded(PairingRecord),
    /// The server removed its pairing: drop its record.
    Unpaired(String),
    /// This server is now the last one to have played: keep it.
    LastPlaybackServer(String),
    /// The server spoke the unencrypted wire format from before 1.0.0-rc1, as Music Assistant
    /// before 2.10.7 does.
    LegacyServer,
    Closed(CloseReason),
}

/// Why the connection closed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloseReason {
    #[error("said goodbye: {0:?}")]
    Goodbye(GoodbyeReason),
    #[error("aborted a pairing as concurrent")]
    ConcurrentPairing,
    #[error("the server refused client/init: {0}")]
    ServerError(String),
    #[error("the server speaks the unencrypted protocol from before 1.0.0-rc1")]
    LegacyServer,
    /// What the specification calls a silent failure: a failed handshake, or a message that
    /// does not decrypt. Nothing more is sent.
    #[error("{0}")]
    Silent(&'static str),
    #[error("the server broke the protocol: {0}")]
    Protocol(String),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("{0}")]
    Internal(&'static str),
}

/// What to put on the WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transmit {
    Text(String),
    Binary(Vec<u8>),
    /// Close the WebSocket after everything before it.
    Close(CloseReason),
}

enum Phase {
    /// `client/init` sent; waiting for `server/init`.
    Init,
    /// Waiting for Noise message 1.
    Handshake(Box<Responder>),
    /// Transport mode.
    Open,
    Closed,
}

fn protocol(e: impl std::fmt::Display) -> CloseReason {
    CloseReason::Protocol(e.to_string())
}

/// Admission of one `server/activate`, by the first rule that applies.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Admit(Activities, ActiveRoles),
    PairingRequired,
    Unauthorized,
    MethodNotSupported,
    Malformed(String),
}

/// Whether `activities` is an allowed set for the key the session matched.
fn allowed(activities: Activities, matched: PskCategory, unpaired_access: bool) -> bool {
    match matched {
        PskCategory::LongTerm => !activities.pairing,
        PskCategory::Pairing | PskCategory::Sentinel => !activities.playback || unpaired_access,
    }
}

fn admission(
    activate: &ServerActivate,
    matched: PskCategory,
    unpaired_access: bool,
    first: bool,
    persisted: ActiveRoles,
    offered: &[Role],
) -> Verdict {
    let Some(activities) = Activities::parse(&activate.activities) else {
        return Verdict::Unauthorized;
    };
    let pairing = match (activities.pairing, &activate.pairing) {
        (false, _) => None,
        (true, None) => return Verdict::Malformed("pairing is missing".into()),
        (true, Some(value)) => match PairingParams::from_value(value.clone()) {
            Ok(params) => Some(params),
            Err(e) => return Verdict::Malformed(e.to_string()),
        },
    };
    let check = |unpaired_access: bool| -> Result<ActiveRoles, Verdict> {
        if !allowed(activities, matched, unpaired_access) {
            return Err(Verdict::Unauthorized);
        }
        let mut with_playback = activities;
        with_playback.playback = true;
        let capable = allowed(with_playback, matched, unpaired_access);
        let roles = match &activate.active_roles {
            Some(list) if !list.is_empty() && !capable => return Err(Verdict::Unauthorized),
            Some(list) => ActiveRoles::from_list(list, offered),
            // Omitted roles persist, unless the connection can no longer carry them.
            None if first || !capable => ActiveRoles::default(),
            None => persisted,
        };
        // Pairing PSK is the only method offered, and it pairs only a session keyed with the
        // pairing key.
        if let Some(p) = &pairing
            && (p.method != "pairing_psk" || matched != PskCategory::Pairing || p.format.is_some())
        {
            return Err(Verdict::MethodNotSupported);
        }
        Ok(roles)
    };
    match check(unpaired_access) {
        Ok(roles) => Verdict::Admit(activities, roles),
        Err(_) if matched != PskCategory::LongTerm && !unpaired_access && check(true).is_ok() => {
            Verdict::PairingRequired
        }
        Err(verdict) => verdict,
    }
}

fn check_config(config: &Config) -> Result<(), ConfigError> {
    if config.name.len() > NAME_MAX {
        return Err(ConfigError::Name);
    }
    if let Some(info) = &config.device_info {
        for (field, value) in [
            ("product_name", &info.product_name),
            ("manufacturer", &info.manufacturer),
            ("software_version", &info.software_version),
            ("mac_address", &info.mac_address),
        ] {
            if value.as_ref().is_some_and(|v| v.len() > NAME_MAX) {
                return Err(ConfigError::DeviceInfo(field));
            }
        }
        if let Some(mac) = &info.mac_address {
            let pairs: Vec<&str> = mac.split(':').collect();
            let lower_hex = |p: &&str| {
                p.len() == 2
                    && p.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            };
            if pairs.len() != 6 || !pairs.iter().all(lower_hex) {
                return Err(ConfigError::MacAddress);
            }
        }
    }
    for (i, role) in config.roles.iter().enumerate() {
        if config.roles[..i].contains(role) {
            return Err(ConfigError::DuplicateRole);
        }
    }
    if config.roles.contains(&Role::Player) && config.formats.is_empty() {
        return Err(ConfigError::NoFormats);
    }
    Ok(())
}

fn client_hello(config: &Config) -> Result<String, ConfigError> {
    let hello = ClientHello {
        name: &config.name,
        device_info: config.device_info.as_ref(),
        supported_roles: config.roles.iter().map(|r| r.id()).collect(),
        player_support: config
            .roles
            .contains(&Role::Player)
            .then_some(PlayerSupport {
                supported_formats: &config.formats,
                buffer_capacity: config.buffer_capacity,
            }),
        supported_pair_methods: PairMethods::default(),
        unpaired_access: UnpairedAccess {
            enabled: config.unpaired_access,
        },
    };
    let text = messages::encode("client/hello", &hello)?;
    if text.len() > JSON_MAX {
        return Err(ConfigError::HelloTooLarge);
    }
    Ok(text)
}

/// One connection, from `client/init` to its close.
pub struct Connection {
    config: Config,
    client_init: String,
    client_hello: String,
    store: PairingStore,
    phase: Phase,
    transport: Option<Transport>,
    server_id: String,
    server_key: [u8; 32],
    matched: Option<PskCategory>,
    reassembler: Reassembler,
    got_hello: bool,
    activated: bool,
    /// Between Noise message 1 of a re-handshake and the `server/activate` after it, nothing
    /// else may be sent or received.
    rehandshaking: bool,
    deadline: Option<(i64, &'static str)>,
    activities: Activities,
    roles: ActiveRoles,
    rival: Option<Rival>,
    /// `pinnad`'s last-playback server, as of the last [`Connection::set_rival`].
    last_playback_server: Option<String>,
    filter: TimeFilter,
    bursts: Bursts,
    player_state: Option<PlayerState>,
    /// What the server last heard: `available`, and the player object since the player role
    /// last became active.
    reported_available: Option<bool>,
    reported_player: Option<PlayerStateObject>,
    stream_active: bool,
    budget: Budget,
    controller: Option<ControllerState>,
    pending_metadata: Option<Metadata>,
    pairing: PairingFlow,
    out: VecDeque<Transmit>,
    events: VecDeque<Event>,
}

impl Connection {
    /// Starts a connection the server has just opened, and queues `client/init`. `store` is
    /// copied: pairings made and removed on this connection are reported as events.
    pub fn new(config: Config, store: &PairingStore, now_us: i64) -> Result<Self, ConfigError> {
        check_config(&config)?;
        let client_id = config.identity.client_id();
        let client_init = messages::encode(
            "client/init",
            &ClientInit {
                client_id: &client_id,
                version: 1,
                suite: SUITE,
            },
        )?;
        let client_hello = client_hello(&config)?;
        let audio_limit = config.buffer_capacity as usize;
        let mut connection = Self {
            client_init: client_init.clone(),
            client_hello,
            store: store.clone(),
            phase: Phase::Init,
            transport: None,
            server_id: String::new(),
            server_key: [0; 32],
            matched: None,
            reassembler: Reassembler::new(audio_limit),
            got_hello: false,
            activated: false,
            rehandshaking: false,
            deadline: Some((now_us.saturating_add(STEP_TIMEOUT_US), "server/init")),
            activities: Activities::default(),
            roles: ActiveRoles::default(),
            rival: None,
            last_playback_server: None,
            filter: TimeFilter::new(),
            bursts: Bursts::new(),
            player_state: None,
            reported_available: None,
            reported_player: None,
            stream_active: false,
            budget: Budget::new(audio_limit),
            controller: None,
            pending_metadata: None,
            pairing: PairingFlow::default(),
            out: VecDeque::new(),
            events: VecDeque::new(),
            config,
        };
        connection.out.push_back(Transmit::Text(client_init));
        Ok(connection)
    }

    /// The next thing to put on the WebSocket.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.out.pop_front()
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// When to call [`Connection::on_tick`] next.
    pub fn next_wakeup(&self) -> Option<i64> {
        if self.is_closed() {
            return None;
        }
        let metadata = self
            .pending_metadata
            .as_ref()
            .map(|m| self.filter.compute_client_time(m.timestamp));
        let clock = self
            .can_send_app()
            .then(|| self.bursts.next_wakeup())
            .flatten();
        [
            self.deadline.map(|(at, _)| at),
            self.pairing.deadline(),
            metadata,
            clock,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A WebSocket text message.
    pub fn on_text(&mut self, text: &str, now_us: i64) {
        match self.phase {
            Phase::Closed => {}
            Phase::Init => self.on_server_init(text, now_us),
            Phase::Handshake(_) => self.on_message1(text, now_us),
            Phase::Open => self.close(CloseReason::Silent(
                "a cleartext message after the handshake",
            )),
        }
    }

    /// A WebSocket binary message.
    pub fn on_binary(&mut self, bytes: &[u8], now_us: i64) {
        match self.phase {
            Phase::Closed => return,
            Phase::Init | Phase::Handshake(_) => {
                return self.close(CloseReason::Silent("a binary message during the handshake"));
            }
            Phase::Open => {}
        }
        let Some(transport) = self.transport.as_mut() else {
            return;
        };
        let plain = match transport.decrypt(bytes) {
            Ok(plain) => plain,
            Err(_) => return self.close(CloseReason::Silent("a message failed to decrypt")),
        };
        let message = match self.reassembler.push(plain) {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(e) => return self.close(protocol(e)),
        };
        let expecting_activate = !self.activated || self.rehandshaking;
        match message.kind {
            TYPE_JSON => self.on_json(&message.body, now_us),
            _ if expecting_activate => {
                return self.close(CloseReason::Protocol(
                    "a binary message before server/activate".into(),
                ));
            }
            TYPE_AUDIO => self.on_audio(message.body, now_us),
            // A binary message of a role this player does not implement.
            _ => {}
        }
        self.pump(now_us);
    }

    /// Time passed: timeouts, scheduled metadata, and the clock's bursts.
    pub fn on_tick(&mut self, now_us: i64) {
        if self.is_closed() {
            return;
        }
        if let Some((at, what)) = self.deadline
            && now_us >= at
        {
            return self.close(CloseReason::Timeout(what));
        }
        if self.pairing.deadline().is_some_and(|at| now_us >= at) {
            self.pairing.abort();
            if self.can_send() {
                self.send(
                    "pair/abort",
                    &ClientPairAbort {
                        reason: PairAbortReason::AttemptTimeout,
                    },
                );
            }
        }
        self.pump(now_us);
    }

    /// Reports the player's state. `client/state` goes out once the specification allows it
    /// (after the first activation and, with the player role active, the first clock
    /// measurement) and then whenever what it would say changes. Each message carries every
    /// active role's object in full.
    pub fn set_player_state(&mut self, state: PlayerState) -> Result<(), StateError> {
        if state.volume > VOLUME_MAX {
            return Err(StateError::Volume);
        }
        if state.min_buffer_ms > BUFFER_MS_MAX {
            return Err(StateError::MinBuffer);
        }
        if state
            .format
            .is_some_and(|f| !self.config.formats.contains(&f))
        {
            return Err(StateError::Format);
        }
        self.player_state = Some(state);
        self.send_state();
        Ok(())
    }

    /// Sends a controller command, if the server's latest controller state lists it.
    pub fn command(&mut self, command: ControllerCommand) -> Result<(), CommandError> {
        if !self.can_send_app() || !self.roles.controller {
            return Err(CommandError::NotActive);
        }
        let kind = command.kind();
        let Some(state) = self
            .controller
            .as_ref()
            .filter(|s| s.supported_commands.contains(&kind))
        else {
            return Err(CommandError::NotSupported(kind));
        };
        match command {
            ControllerCommand::Volume(v) if v > VOLUME_MAX => return Err(CommandError::OutOfRange),
            ControllerCommand::Seek { position_ms }
                if position_ms < 0 || state.seek_max_ms.is_none_or(|max| position_ms > max) =>
            {
                return Err(CommandError::OutOfRange);
            }
            _ => {}
        }
        self.send(
            "client/command",
            &ClientCommand {
                controller: command.object(),
            },
        );
        Ok(())
    }

    /// Leaves the group, which plays on without this player (ADR 0027 §14).
    pub fn leave(&mut self) -> Result<(), CommandError> {
        if !self.can_send_app() {
            return Err(CommandError::NotActive);
        }
        self.send("client/leave", &Empty {});
        Ok(())
    }

    /// Says goodbye, where the specification allows a message, and closes.
    pub fn goodbye(&mut self, reason: GoodbyeReason) {
        if self.is_closed() {
            return;
        }
        if self.can_send() {
            self.send("client/goodbye", &ClientGoodbye { reason });
        }
        self.close(CloseReason::Goodbye(reason));
    }

    /// Ends this connection because a newcomer won against it: `pair/abort` if it pairs,
    /// otherwise goodbye with `another_server`.
    pub fn displace(&mut self) {
        if self.is_closed() {
            return;
        }
        if self.can_send() && (self.activities.pairing || self.pairing.in_progress()) {
            self.abort_concurrent_pairing();
        } else {
            self.goodbye(GoodbyeReason::AnotherServer);
        }
    }

    /// The admitted connection, if any, that this one's first activation competes with, and the
    /// last-playback server from `pinnad`'s store, which decides between two idle connections.
    /// The player admits one connection at a time (ADR 0027 §3). Both change while this
    /// connection waits, so `pinnad` sets them again before each message it passes in until
    /// this connection is activated. This connection's own copy of the store is not consulted:
    /// it is as old as the connection.
    pub fn set_rival(&mut self, rival: Option<Rival>, last_playback_server: Option<&str>) {
        self.rival = rival;
        self.last_playback_server = last_playback_server.map(str::to_string);
    }

    /// This connection as a rival for a newcomer, once admitted.
    pub fn as_rival(&self) -> Option<Rival> {
        (self.activated && !self.is_closed()).then(|| Rival {
            server_id: self.server_id.clone(),
            activities: self.activities,
            pairing_attempt: self.pairing.in_progress(),
        })
    }

    /// The server's clock, for converting audio timestamps.
    pub fn clock(&self) -> &TimeFilter {
        &self.filter
    }

    /// The pairing store as this connection has changed it.
    pub fn store(&self) -> &PairingStore {
        &self.store
    }

    /// Known once `server/init` has arrived.
    pub fn server_id(&self) -> Option<&str> {
        (!self.server_id.is_empty()).then_some(self.server_id.as_str())
    }

    /// The key the current session was keyed with.
    pub fn psk_category(&self) -> Option<PskCategory> {
        self.matched
    }

    pub fn activities(&self) -> Activities {
        self.activities
    }

    pub fn active_roles(&self) -> ActiveRoles {
        self.roles
    }

    pub fn is_activated(&self) -> bool {
        self.activated
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.phase, Phase::Closed)
    }

    // The handshake.

    fn on_server_init(&mut self, text: &str, now: i64) {
        let Ok(envelope) = messages::envelope(text.as_bytes()) else {
            return self.close(CloseReason::Silent("server/init is not a valid message"));
        };
        if !matches!(
            envelope.kind.as_str(),
            "server/init" | "server/error" | "noise/handshake"
        ) {
            // A message from the old protocol, which starts with server/hello in clear text.
            self.events.push_back(Event::LegacyServer);
            return self.close(CloseReason::LegacyServer);
        }
        let init = match ServerMessage::from_envelope(envelope) {
            Ok(ServerMessage::Init(init)) => init,
            Ok(ServerMessage::Error(e)) => {
                let reason = e.reason.chars().take(64).collect();
                return self.close(CloseReason::ServerError(reason));
            }
            _ => return self.close(CloseReason::Silent("expected server/init")),
        };
        if init.version != 1 {
            return self.close(CloseReason::Silent(
                "server/init names another core version",
            ));
        }
        let Some(key) = b64::decode_url_32(&init.server_id) else {
            return self.close(CloseReason::Silent("server_id is not a key"));
        };
        // The prologue binds both init messages, byte for byte as they crossed the wire.
        let mut prologue = self.client_init.as_bytes().to_vec();
        prologue.extend_from_slice(text.as_bytes());
        match Responder::new(&self.config.identity, &key, &prologue) {
            Ok(responder) => {
                self.server_id = init.server_id;
                self.server_key = key;
                self.phase = Phase::Handshake(Box::new(responder));
                self.deadline = Some((now.saturating_add(STEP_TIMEOUT_US), "noise/handshake"));
            }
            Err(_) => self.close(CloseReason::Silent("could not start the Noise handshake")),
        }
    }

    fn on_message1(&mut self, text: &str, now: i64) {
        let data = match messages::envelope(text.as_bytes()).map(ServerMessage::from_envelope) {
            Ok(Ok(ServerMessage::Handshake(h))) => h.data,
            _ => return self.close(CloseReason::Silent("expected noise/handshake")),
        };
        let Phase::Handshake(mut responder) = std::mem::replace(&mut self.phase, Phase::Init)
        else {
            return;
        };
        let fail = CloseReason::Silent;
        let Some(bytes) = b64::decode_url(&data) else {
            return self.close(fail("noise/handshake data is not base64url"));
        };
        let Ok(msg1) = responder.read_message1(&bytes) else {
            return self.close(fail("Noise message 1 failed"));
        };
        let candidates = self.store.candidates();
        let Ok((category, psk)) = noise::select_psk(&candidates, &msg1, &self.server_id, true)
        else {
            return self.close(fail("the pairing with this key belongs to another server"));
        };
        let Ok((reply, transport)) = responder.write_message2(&psk) else {
            return self.close(fail("could not write Noise message 2"));
        };
        let text = messages::encode(
            "noise/handshake",
            &NoiseHandshake {
                data: b64::encode_url(&reply),
            },
        );
        let Ok(text) = text else {
            return self.close(CloseReason::Internal("could not encode noise/handshake"));
        };
        self.out.push_back(Transmit::Text(text));
        self.transport = Some(transport);
        self.matched = Some(category);
        self.phase = Phase::Open;
        self.pairing.on_handshake();
        self.deadline = Some((now.saturating_add(STEP_TIMEOUT_US), "server/activate"));
    }

    fn on_rehandshake(&mut self, h: NoiseHandshake, now: i64) -> Result<(), CloseReason> {
        let fail = CloseReason::Silent;
        let bytes =
            b64::decode_url(&h.data).ok_or(fail("noise/handshake data is not base64url"))?;
        let prologue = *self
            .transport
            .as_ref()
            .ok_or(fail("no session to re-key"))?
            .handshake_hash();
        let mut responder = Responder::new(&self.config.identity, &self.server_key, &prologue)
            .map_err(|_| fail("could not start the re-handshake"))?;
        let msg1 = responder
            .read_message1(&bytes)
            .map_err(|_| fail("re-handshake message 1 failed"))?;
        // No Sentinel fallback here: a re-handshake must name a key the player holds.
        let candidates = self.store.candidates();
        let (category, psk) = noise::select_psk(&candidates, &msg1, &self.server_id, false)
            .map_err(|_| fail("the re-handshake names no key the player holds for this server"))?;
        let (reply, transport) = responder
            .write_message2(&psk)
            .map_err(|_| fail("could not write re-handshake message 2"))?;
        // Message 2 still travels under the old keys; everything after it under the new.
        self.send(
            "noise/handshake",
            &NoiseHandshake {
                data: b64::encode_url(&reply),
            },
        );
        if self.is_closed() {
            return Ok(());
        }
        self.transport = Some(transport);
        self.matched = Some(category);
        self.rehandshaking = true;
        self.pairing.on_handshake();
        self.deadline = Some((
            now.saturating_add(STEP_TIMEOUT_US),
            "server/activate after a re-handshake",
        ));
        Ok(())
    }

    // Messages after the handshake.

    fn on_json(&mut self, body: &[u8], now: i64) {
        let envelope = match messages::envelope(body) {
            Ok(envelope) => envelope,
            Err(e) => return self.close(protocol(e)),
        };
        // The order the specification sets: server/hello, then server/activate, and only
        // server/activate after a re-handshake.
        let expected = if !self.got_hello {
            Some("server/hello")
        } else if !self.activated || self.rehandshaking {
            Some("server/activate")
        } else {
            None
        };
        if let Some(expected) = expected
            && envelope.kind != expected
        {
            return self.close(CloseReason::Protocol(format!(
                "{} arrived where {expected} was due",
                envelope.kind
            )));
        }
        let result = ServerMessage::from_envelope(envelope)
            .map_err(protocol)
            .and_then(|message| self.dispatch(message, now));
        if let Err(reason) = result {
            self.close(reason);
        }
    }

    fn dispatch(&mut self, message: ServerMessage, now: i64) -> Result<(), CloseReason> {
        match message {
            ServerMessage::Hello(hello) => self.on_hello(hello),
            ServerMessage::Activate(activate) => self.on_activate(&activate, now),
            ServerMessage::Handshake(h) => self.on_rehandshake(h, now),
            ServerMessage::Time(time) => {
                self.bursts.on_reply(&time, now);
                Ok(())
            }
            ServerMessage::State(state) => self.on_state(state, now),
            ServerMessage::Command(command) => self.on_command(command),
            ServerMessage::Group(group) => {
                self.events.push_back(Event::Group(group));
                Ok(())
            }
            ServerMessage::StreamStart(start) => self.on_stream_start(start),
            ServerMessage::StreamClear(clear) => {
                self.on_stream_clear(&clear);
                Ok(())
            }
            ServerMessage::StreamEnd(end) => {
                self.on_stream_end(&end);
                Ok(())
            }
            ServerMessage::PairFinalize => self.on_pair_finalize(),
            ServerMessage::PairAbort(_) => {
                self.pairing.on_server_abort();
                Ok(())
            }
            ServerMessage::Unpair => {
                self.on_unpair();
                Ok(())
            }
            // After the player aborted, pairing messages in flight are dropped.
            ServerMessage::CodePairing(_) if self.pairing.aborted() => Ok(()),
            ServerMessage::CodePairing(kind) => Err(CloseReason::Protocol(format!(
                "{kind} is out of sequence: only Pairing PSK is offered"
            ))),
            ServerMessage::Init(_) | ServerMessage::Error(_) => Err(CloseReason::Protocol(
                "a handshake message after the handshake".into(),
            )),
            ServerMessage::FromClient(kind) => Err(CloseReason::Protocol(format!(
                "{kind} is a client's message"
            ))),
            ServerMessage::Unknown(_) => Ok(()),
        }
    }

    fn on_hello(&mut self, hello: ServerHello) -> Result<(), CloseReason> {
        if self.got_hello {
            return Err(CloseReason::Protocol("a second server/hello".into()));
        }
        self.got_hello = true;
        self.events.push_back(Event::Connected {
            server_id: self.server_id.clone(),
            name: hello.name,
        });
        let hello = self.client_hello.clone().into_bytes();
        self.send_json(hello);
        Ok(())
    }

    fn on_activate(&mut self, activate: &ServerActivate, now: i64) -> Result<(), CloseReason> {
        let first = !self.activated;
        if std::mem::take(&mut self.rehandshaking) {
            self.deadline = None;
        }
        if activate.activities.iter().any(|a| a == "pairing") {
            self.pairing.count_pairing_activation();
        }
        // Every activation ends a pairing attempt in progress without keeping anything.
        self.pairing.on_activate();
        let Some(matched) = self.matched else {
            return Err(CloseReason::Protocol(
                "server/activate before the handshake".into(),
            ));
        };
        let verdict = admission(
            activate,
            matched,
            self.config.unpaired_access,
            first,
            self.roles,
            &self.config.roles,
        );
        match verdict {
            Verdict::Admit(activities, roles) => self.apply(activities, roles, first, now),
            Verdict::PairingRequired => {
                self.goodbye(GoodbyeReason::PairingRequired);
                Ok(())
            }
            Verdict::Unauthorized => {
                self.goodbye(GoodbyeReason::Unauthorized);
                Ok(())
            }
            Verdict::MethodNotSupported => {
                // Refused, not applied; the connection stays open.
                self.pairing.abort();
                self.send(
                    "pair/abort",
                    &ClientPairAbort {
                        reason: PairAbortReason::MethodNotSupported,
                    },
                );
                Ok(())
            }
            Verdict::Malformed(detail) => {
                Err(CloseReason::Protocol(format!("server/activate: {detail}")))
            }
        }
    }

    /// Whether a newcomer declaring `mine` wins against the admitted `rival`.
    fn wins_against(&self, rival: &Rival, mine: Activities) -> bool {
        if rival.pairing_attempt {
            return false;
        }
        if mine.rank() == 0 && rival.activities.rank() == 0 {
            let last = self.last_playback_server.as_deref();
            return last == Some(self.server_id.as_str()) && last != Some(rival.server_id.as_str());
        }
        mine.rank() >= rival.activities.rank()
    }

    fn apply(
        &mut self,
        activities: Activities,
        roles: ActiveRoles,
        first: bool,
        now: i64,
    ) -> Result<(), CloseReason> {
        let mut displaces_rival = false;
        if first && let Some(rival) = self.rival.clone() {
            if !self.wins_against(&rival, activities) {
                if activities.pairing {
                    self.abort_concurrent_pairing();
                } else {
                    self.goodbye(GoodbyeReason::ConcurrentAttempt);
                }
                return Ok(());
            }
            displaces_rival = true;
        }
        let before = self.roles;
        self.activities = activities;
        self.roles = roles;
        if first {
            self.activated = true;
            self.deadline = None;
            self.bursts.start(now);
        }
        // A removed player role, or dropped playback, ends the stream even if the server sent
        // no stream/end.
        if self.stream_active && (!roles.player || !activities.playback) {
            self.end_stream();
        }
        if roles.player != before.player {
            // A newly active player role must be reported in full, changed or not.
            self.reported_player = None;
        }
        if before.metadata && !roles.metadata {
            self.pending_metadata = None;
            self.events.push_back(Event::Metadata(None));
        }
        if before.controller && !roles.controller {
            self.controller = None;
            self.events.push_back(Event::ControllerState(None));
        }
        self.events.push_back(Event::Activated {
            activities,
            roles,
            paired: self.matched == Some(PskCategory::LongTerm),
            displaces_rival,
        });
        if activities.playback && self.store.set_last_playback_server(&self.server_id) {
            self.events
                .push_back(Event::LastPlaybackServer(self.server_id.clone()));
        }
        if activities.pairing {
            // Admission checked that this session is keyed with the pairing key.
            let (index, psk) = self
                .pairing
                .begin(now, self.store.pairing_psk())
                .map_err(|_| CloseReason::Internal("the random number generator failed"))?;
            self.send(
                "client/pair-init",
                &ClientPairInit {
                    pairing_index: index,
                },
            );
            self.send(
                "client/pair-finalize",
                &ClientPairFinalize {
                    long_term_psk: psk.to_base64url(),
                },
            );
        }
        Ok(())
    }

    fn on_state(&mut self, state: ServerState, now: i64) -> Result<(), CloseReason> {
        if self.roles.metadata
            && let Some(value) = state.metadata
        {
            let metadata = Metadata::from_value(value).map_err(protocol)?;
            // A future timestamp schedules the update; before the first clock measurement
            // there is no way to tell, and the first one is never in the future.
            if self.filter.count() > 0 && self.filter.compute_client_time(metadata.timestamp) > now
            {
                self.pending_metadata = Some(metadata);
            } else {
                self.pending_metadata = None;
                self.events.push_back(Event::Metadata(Some(metadata)));
            }
        }
        if self.roles.controller
            && let Some(value) = state.controller
        {
            let controller = ControllerState::from_value(value).map_err(protocol)?;
            self.controller = Some(controller.clone());
            self.events
                .push_back(Event::ControllerState(Some(controller)));
        }
        Ok(())
    }

    fn on_command(&mut self, command: ServerCommand) -> Result<(), CloseReason> {
        let Some(value) = command.player.filter(|_| self.roles.player) else {
            return Ok(());
        };
        let bad = |what: &str| CloseReason::Protocol(format!("server/command: {what}"));
        let kind = match value.get("command").and_then(|c| c.as_str()) {
            Some("volume") => PlayerCommandKind::Volume,
            Some("mute") => PlayerCommandKind::Mute,
            Some(_) => return Ok(()),
            None => return Err(bad("command is not a string")),
        };
        // A command the player has not reported as supported is ignored, its value unread.
        let offered = self
            .reported_player
            .as_ref()
            .is_some_and(|p| p.supported_commands.contains(&kind));
        if !offered {
            return Ok(());
        }
        let command = PlayerCommand::from_value(value).map_err(protocol)?;
        match kind {
            PlayerCommandKind::Volume => {
                let raw = command.volume.ok_or_else(|| bad("volume is missing"))?;
                let volume = u8::try_from(raw)
                    .ok()
                    .filter(|v| *v <= VOLUME_MAX)
                    .ok_or_else(|| bad("volume is out of range"))?;
                if let Some(state) = self.player_state.as_mut() {
                    state.volume = volume;
                }
                self.events.push_back(Event::Volume(volume));
            }
            PlayerCommandKind::Mute => {
                let muted = command.mute.ok_or_else(|| bad("mute is missing"))?;
                if let Some(state) = self.player_state.as_mut() {
                    state.muted = muted;
                }
                self.events.push_back(Event::Mute(muted));
            }
        }
        Ok(())
    }

    fn on_stream_start(&mut self, start: messages::StreamStart) -> Result<(), CloseReason> {
        // Without the role or without playback, the player object is left unvalidated.
        if !(self.roles.player && self.activities.playback) {
            return Ok(());
        }
        let Some(value) = start.player else {
            return Ok(());
        };
        let StreamFormat {
            format,
            codec_header,
        } = StreamFormat::from_value(value, &self.config.formats).map_err(protocol)?;
        let in_place = self.stream_active;
        self.stream_active = true;
        self.events.push_back(Event::StreamStart(PlayerStream {
            format,
            codec_header,
            in_place,
            server_transmitted_us: start.server_transmitted,
        }));
        Ok(())
    }

    fn on_stream_clear(&mut self, clear: &StreamClear) {
        if !(self.stream_active && clear.targets("player")) {
            return;
        }
        match clear.from_timestamp {
            Some(from) => self.budget.clear_from(from),
            None => self.budget.reset(),
        }
        self.events.push_back(Event::StreamClear {
            from_timestamp: clear.from_timestamp,
        });
    }

    fn on_stream_end(&mut self, end: &StreamEnd) {
        if self.stream_active && end.targets("player") {
            self.end_stream();
        }
    }

    fn end_stream(&mut self) {
        self.stream_active = false;
        self.budget.reset();
        self.events.push_back(Event::StreamEnd);
    }

    fn on_audio(&mut self, mut body: Vec<u8>, now: i64) {
        let header = match frame::split_audio(&body) {
            Ok((header, _)) => header,
            Err(e) => return self.close(protocol(e)),
        };
        // No player stream (the role or playback is inactive, or the server broke the rules),
        // or no clock to place it on: discarded.
        if !self.stream_active || self.filter.count() == 0 {
            return;
        }
        let due = self.filter.compute_client_time(header.timestamp_us);
        if due.saturating_sub(now) > MAX_AHEAD_US {
            return;
        }
        body.drain(..AUDIO_HEADER_LEN - 1);
        if !self
            .budget
            .add(header.timestamp_us, due, AUDIO_HEADER_LEN + body.len(), now)
        {
            return self.close(CloseReason::Protocol(
                "the server sent more audio than buffer_capacity".into(),
            ));
        }
        self.events.push_back(Event::Audio(AudioChunk {
            timestamp_us: header.timestamp_us,
            // 0 and the maximum report that no lead was measured.
            send_ahead_us: Some(header.send_ahead_us).filter(|v| *v != 0 && *v != u32::MAX),
            arrival_us: now,
            data: body,
        }));
    }

    fn on_pair_finalize(&mut self) -> Result<(), CloseReason> {
        match self.pairing.on_finalize() {
            Finalize::Keep(psk) => {
                let record = PairingRecord {
                    server_id: self.server_id.clone(),
                    long_term_psk: psk,
                };
                // This copy only answers this server's re-handshakes, so which other record it
                // evicts does not matter. pinnad applies the event to its own store, keeping the
                // records of every open connection.
                self.store.insert(record.clone(), &[]);
                self.events.push_back(Event::PairingAdded(record));
                Ok(())
            }
            Finalize::Ignore => Ok(()),
            Finalize::OutOfSequence => Err(CloseReason::Protocol(
                "server/pair-finalize without a pairing attempt".into(),
            )),
        }
    }

    fn on_unpair(&mut self) {
        // Only a paired session has a record to remove.
        if self.matched != Some(PskCategory::LongTerm) {
            return;
        }
        self.store.remove(&self.server_id);
        self.events
            .push_back(Event::Unpaired(self.server_id.clone()));
        self.goodbye(GoodbyeReason::Unpaired);
    }

    // Sending.

    fn can_send(&self) -> bool {
        matches!(self.phase, Phase::Open) && !self.rehandshaking
    }

    fn can_send_app(&self) -> bool {
        self.can_send() && self.activated
    }

    /// Clock bursts and the player's state, whenever something may have changed.
    fn pump(&mut self, now: i64) {
        if self.is_closed() {
            return;
        }
        if self
            .pending_metadata
            .as_ref()
            .is_some_and(|m| self.filter.compute_client_time(m.timestamp) <= now)
        {
            let metadata = self.pending_metadata.take();
            self.events.push_back(Event::Metadata(metadata));
        }
        if !self.can_send_app() {
            return;
        }
        let step = self.bursts.poll(now);
        if let Some(sample) = step.feed
            && self.filter.update(sample)
        {
            // Only a drift the conversions use can harm them.
            if self.filter.uses_drift() && self.filter.drift().abs() > MAX_DRIFT {
                return self.close(CloseReason::Protocol(
                    "the server's clock drifts more than 1 % from the player's".into(),
                ));
            }
            self.events.push_back(Event::ClockUpdated);
        }
        if let Some(t1) = step.send {
            self.send(
                "client/time",
                &ClientTime {
                    client_transmitted: t1,
                },
            );
        }
        self.send_state();
    }

    fn send_state(&mut self) {
        if !self.can_send_app() {
            return;
        }
        let Some(state) = &self.player_state else {
            return;
        };
        if self.roles.player && self.filter.count() == 0 {
            return;
        }
        let available = state.available;
        let player = self.roles.player.then(|| PlayerStateObject {
            volume: state.volume,
            muted: state.muted,
            // The speaker is built in: nothing after the output to compensate (ADR 0027 §15).
            output_delay_ms: 0,
            required_lead_time_ms: state.required_lead_time_ms,
            min_buffer_ms: state.min_buffer_ms,
            supported_commands: state.supported_commands.clone(),
            format: state.format,
        });
        // A role coming or going changes nothing the player reports, so it sends nothing by
        // itself, except that a newly active player role is reported at once.
        let changed = self.reported_available != Some(available)
            || (player.is_some() && player != self.reported_player);
        if !changed {
            return;
        }
        self.send(
            "client/state",
            &ClientState {
                available,
                player: player.clone(),
            },
        );
        self.reported_available = Some(available);
        if player.is_some() {
            self.reported_player = player;
        }
    }

    fn send<T: Serialize>(&mut self, kind: &str, payload: &T) {
        match messages::encode(kind, payload) {
            Ok(text) => self.send_json(text.into_bytes()),
            Err(_) => self.close(CloseReason::Internal("could not encode a message")),
        }
    }

    fn send_json(&mut self, json: Vec<u8>) {
        let mut plain = Vec::with_capacity(json.len() + 1);
        plain.push(TYPE_JSON);
        plain.extend_from_slice(&json);
        let Some(transport) = self.transport.as_mut() else {
            return;
        };
        match transport.encrypt(&plain) {
            Ok(message) => self.out.push_back(Transmit::Binary(message)),
            Err(_) => self.close(CloseReason::Internal("could not encrypt a message")),
        }
    }

    fn abort_concurrent_pairing(&mut self) {
        self.send(
            "pair/abort",
            &ClientPairAbort {
                reason: PairAbortReason::ConcurrentAttempt,
            },
        );
        self.close(CloseReason::ConcurrentPairing);
    }

    fn close(&mut self, reason: CloseReason) {
        if self.is_closed() {
            return;
        }
        self.phase = Phase::Closed;
        self.transport = None;
        self.events.push_back(Event::Closed(reason.clone()));
        self.out.push_back(Transmit::Close(reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn activate(value: serde_json::Value) -> ServerActivate {
        serde_json::from_value(value).unwrap()
    }

    const ALL: &[Role] = &[Role::Player, Role::Controller, Role::Metadata];

    fn judge(value: serde_json::Value, matched: PskCategory, unpaired_access: bool) -> Verdict {
        admission(
            &activate(value),
            matched,
            unpaired_access,
            true,
            ActiveRoles::default(),
            ALL,
        )
    }

    fn admit(playback: bool, pairing: bool, player: bool) -> Verdict {
        Verdict::Admit(
            Activities { playback, pairing },
            ActiveRoles {
                player,
                controller: false,
                metadata: false,
            },
        )
    }

    #[test]
    fn admissibility_table() {
        use PskCategory::*;
        let player = json!(["player@v1"]);
        let pairing = json!({"method": "pairing_psk"});
        // Long-term: [] or [playback].
        assert_eq!(
            judge(json!({"activities": []}), LongTerm, false),
            admit(false, false, false)
        );
        assert_eq!(
            judge(
                json!({"activities": ["playback"], "active_roles": player}),
                LongTerm,
                false
            ),
            admit(true, false, true)
        );
        assert_eq!(
            judge(
                json!({"activities": ["pairing"], "pairing": pairing}),
                LongTerm,
                true
            ),
            Verdict::Unauthorized
        );
        // Pairing and Sentinel: playback only with unpaired access.
        for matched in [Pairing, Sentinel] {
            assert_eq!(
                judge(json!({"activities": []}), matched, false),
                admit(false, false, false)
            );
            assert_eq!(
                judge(
                    json!({"activities": ["playback"], "active_roles": player}),
                    matched,
                    true
                ),
                admit(true, false, true)
            );
            assert_eq!(
                judge(
                    json!({"activities": ["playback"], "active_roles": player}),
                    matched,
                    false
                ),
                Verdict::PairingRequired
            );
            // Roles on a connection that cannot play.
            assert_eq!(
                judge(
                    json!({"activities": [], "active_roles": player}),
                    matched,
                    false
                ),
                Verdict::PairingRequired
            );
        }
        // Roles while idle on a connection that could play are fine.
        assert_eq!(
            judge(
                json!({"activities": [], "active_roles": player}),
                Sentinel,
                true
            ),
            admit(false, false, true)
        );
        assert_eq!(
            judge(
                json!({"activities": ["pairing"], "pairing": pairing}),
                Pairing,
                false
            ),
            admit(false, true, false)
        );
        assert_eq!(
            judge(
                json!({"activities": ["playback", "pairing"], "pairing": pairing}),
                Pairing,
                true
            ),
            admit(true, true, false)
        );
    }

    #[test]
    fn admission_refusals_in_the_specified_order() {
        use PskCategory::*;
        let pairing = json!({"method": "pairing_psk"});
        // pairing_psk only on a session keyed with the pairing key.
        assert_eq!(
            judge(
                json!({"activities": ["pairing"], "pairing": pairing}),
                Sentinel,
                false
            ),
            Verdict::MethodNotSupported
        );
        // Methods and formats this player does not offer.
        assert_eq!(
            judge(
                json!({"activities": ["pairing"], "pairing": {"method": "static_pairing_code"}}),
                Sentinel,
                false
            ),
            Verdict::MethodNotSupported
        );
        assert_eq!(
            judge(
                json!({"activities": ["pairing"], "pairing": {"method": "pairing_psk", "format": "digits"}}),
                Pairing,
                false
            ),
            Verdict::MethodNotSupported
        );
        // An unauthorized set is reported before a bad method.
        assert_eq!(
            judge(
                json!({"activities": ["pairing", "playback"], "pairing": {"method": "x"}}),
                LongTerm,
                false
            ),
            Verdict::Unauthorized
        );
        // Unpaired access would not help: unauthorized, not pairing_required.
        assert_eq!(
            judge(
                json!({"activities": ["playback", "pairing"], "pairing": {"method": "x"}}),
                Sentinel,
                false
            ),
            Verdict::Unauthorized
        );
        // Unknown and repeated activities are no allowed set.
        assert_eq!(
            judge(json!({"activities": ["party"]}), Sentinel, true),
            Verdict::Unauthorized
        );
        assert_eq!(
            judge(
                json!({"activities": ["playback", "playback"]}),
                LongTerm,
                true
            ),
            Verdict::Unauthorized
        );
        assert!(matches!(
            judge(json!({"activities": ["pairing"]}), Pairing, false),
            Verdict::Malformed(_)
        ));
        // The pairing object is ignored without the pairing activity.
        assert_eq!(
            judge(json!({"activities": [], "pairing": 5}), LongTerm, false),
            admit(false, false, false)
        );
    }

    #[test]
    fn omitted_roles_persist_unless_the_connection_cannot_play() {
        let persisted = ActiveRoles {
            player: true,
            controller: true,
            metadata: false,
        };
        let later =
            |value, matched| admission(&activate(value), matched, false, false, persisted, ALL);
        assert_eq!(
            later(json!({"activities": []}), PskCategory::LongTerm),
            Verdict::Admit(Activities::default(), persisted)
        );
        // A pairing session without unpaired access cannot play: the roles drop, and the
        // activation is not refused.
        assert_eq!(
            later(json!({"activities": []}), PskCategory::Pairing),
            Verdict::Admit(Activities::default(), ActiveRoles::default())
        );
        // The first activation without active_roles has none.
        assert_eq!(
            admission(
                &activate(json!({"activities": ["playback"]})),
                PskCategory::LongTerm,
                false,
                true,
                persisted,
                ALL
            ),
            admit(true, false, false)
        );
        // Roles the player did not offer stay inactive.
        assert_eq!(
            admission(
                &activate(
                    json!({"activities": ["playback"], "active_roles": ["player@v1", "artwork@v1", "metadata@v1"]})
                ),
                PskCategory::LongTerm,
                false,
                true,
                persisted,
                &[Role::Player]
            ),
            admit(true, false, true)
        );
    }

    #[test]
    fn config_checks() {
        let config = || Config {
            identity: Identity::from_keys([1; 32], [2; 32]),
            name: "Kitchen".into(),
            device_info: Some(DeviceInfo {
                mac_address: Some("aa:bb:cc:dd:ee:0f".into()),
                ..DeviceInfo::default()
            }),
            roles: vec![Role::Player, Role::Metadata],
            formats: vec![AudioFormat {
                codec: messages::Codec::Pcm,
                channels: 2,
                sample_rate: 48000,
                bit_depth: 16,
            }],
            buffer_capacity: 1 << 20,
            unpaired_access: true,
        };
        assert_eq!(check_config(&config()), Ok(()));
        let mut c = config();
        c.name = "x".repeat(65);
        assert_eq!(check_config(&c), Err(ConfigError::Name));
        for mac in [
            "AA:bb:cc:dd:ee:ff",
            "aa:bb:cc:dd:ee",
            "aa-bb-cc-dd-ee-ff",
            "aa:bb:cc:dd:ee:fg",
        ] {
            let mut c = config();
            c.device_info = Some(DeviceInfo {
                mac_address: Some(mac.into()),
                ..DeviceInfo::default()
            });
            assert_eq!(check_config(&c), Err(ConfigError::MacAddress), "{mac}");
        }
        let mut c = config();
        c.device_info = Some(DeviceInfo {
            product_name: Some("x".repeat(65)),
            ..DeviceInfo::default()
        });
        assert_eq!(
            check_config(&c),
            Err(ConfigError::DeviceInfo("product_name"))
        );
        let mut c = config();
        c.roles.push(Role::Player);
        assert_eq!(check_config(&c), Err(ConfigError::DuplicateRole));
        let mut c = config();
        c.formats.clear();
        assert_eq!(check_config(&c), Err(ConfigError::NoFormats));
        c.roles = vec![Role::Controller];
        assert_eq!(check_config(&c), Ok(()));
        let mut c = config();
        c.formats = vec![c.formats[0]; 500];
        assert_eq!(client_hello(&c), Err(ConfigError::HelloTooLarge));
    }
}
