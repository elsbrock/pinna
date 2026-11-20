//! The JSON messages of a Sendspin player with the roles `player@v1`, `controller@v1` and
//! `metadata@v1`, and of the Pairing PSK method (ADR 0027 §5, §8).
//!
//! Parsing follows the specification's forward-compatibility rules: unknown fields and unknown
//! message types are ignored, while a known field outside its range or length limit is a
//! protocol error that closes the connection. Every JSON message the server sends must fit in
//! 16 KiB and nest at most 10 levels (ADR 0027 §6). The objects of a role are parsed only when
//! that role is active, because the specification says to leave an inactive role's values
//! unvalidated. Optional fields that are absent are never written.
//!
//! The ranges are 1.0.0-rc1's, the version servers speak (ADR 0027 §6). A limit that only a
//! later draft sets is not held against a server: Music Assistant from 2.10.7 to 2.11 sends a
//! UUID as `group_id`, not 43 base64url characters, and passes on names and track details at
//! any length. Such text is cut to the later limit, an over-long artwork URL is dropped, a
//! track number below 1 is taken as unknown, and a group's id is taken as it is.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::b64;
use crate::frame::JSON_MAX;

/// The deepest nesting allowed: the root object is level 1.
pub const JSON_MAX_DEPTH: usize = 10;
/// The limit on a name, a group's name and each `device_info` string, in UTF-8 bytes.
pub const NAME_MAX: usize = 64;
/// The limit on a metadata title, artist, album artist or album, in UTF-8 bytes.
pub const METADATA_TEXT_MAX: usize = 255;
/// The limit on a metadata artwork URL, in UTF-8 bytes.
pub const ARTWORK_URL_MAX: usize = 2048;
/// The highest volume.
pub const VOLUME_MAX: u8 = 100;
/// The highest `min_buffer_ms` (and `output_delay_ms`).
pub const BUFFER_MS_MAX: u32 = 5000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MessageError {
    #[error("a JSON message exceeds 16384 bytes")]
    TooLarge,
    #[error("a JSON message nests deeper than 10 levels")]
    TooDeep,
    #[error("not a JSON object with a type string and a payload object")]
    NotEnvelope,
    #[error("{kind} does not have the defined shape: {detail}")]
    Malformed { kind: String, detail: String },
    #[error("{kind}: {field} is out of range")]
    OutOfRange {
        kind: &'static str,
        field: &'static str,
    },
    #[error("could not encode {0}")]
    Encode(String),
}

/// A JSON message's type and payload, before the payload is read.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub kind: String,
    pub payload: Map<String, Value>,
}

/// Checks the limits and the envelope: an object with a `type` string and a `payload` object.
pub fn envelope(bytes: &[u8]) -> Result<Envelope, MessageError> {
    if bytes.len() > JSON_MAX {
        return Err(MessageError::TooLarge);
    }
    if too_deep(bytes) {
        return Err(MessageError::TooDeep);
    }
    let mut root: Map<String, Value> =
        serde_json::from_slice(bytes).map_err(|_| MessageError::NotEnvelope)?;
    let (Some(Value::String(kind)), Some(Value::Object(payload))) =
        (root.remove("type"), root.remove("payload"))
    else {
        return Err(MessageError::NotEnvelope);
    };
    Ok(Envelope { kind, payload })
}

/// Whether objects and arrays nest deeper than [`JSON_MAX_DEPTH`]. Counted on the bytes, so
/// the parser never recurses that far.
fn too_deep(bytes: &[u8]) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &b in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > JSON_MAX_DEPTH {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

fn read<T: DeserializeOwned>(kind: &str, payload: Value) -> Result<T, MessageError> {
    serde_json::from_value(payload).map_err(|e| MessageError::Malformed {
        kind: kind.to_string(),
        detail: e.to_string(),
    })
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn cut(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

/// The messages only a client sends. One of them from the server breaks the direction rules.
const CLIENT_TYPES: &[&str] = &[
    "client/init",
    "client/hello",
    "client/time",
    "client/state",
    "client/command",
    "client/leave",
    "client/goodbye",
    "client-stream/start",
    "client-stream/end",
    "client/pair-pending",
    "client/pair-init",
    "client/pair-auth",
    "client/pair-confirm",
    "client/pair-retry",
    "client/pair-finalize",
];

/// A message from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Init(ServerInit),
    Error(ServerError),
    Handshake(NoiseHandshake),
    Hello(ServerHello),
    Activate(ServerActivate),
    Time(ServerTime),
    State(ServerState),
    Command(ServerCommand),
    Group(GroupUpdate),
    StreamStart(StreamStart),
    StreamClear(StreamClear),
    StreamEnd(StreamEnd),
    PairFinalize,
    PairAbort(PairAbort),
    Unpair,
    /// A message of a code-based pairing method, which this player never offers.
    CodePairing(String),
    /// A message only a client sends.
    FromClient(String),
    /// A type this player does not know.
    Unknown(String),
}

impl ServerMessage {
    /// Reads the payload of a known type and checks its client-level fields.
    pub fn from_envelope(envelope: Envelope) -> Result<Self, MessageError> {
        let Envelope { kind, payload } = envelope;
        let payload = Value::Object(payload);
        let k = kind.as_str();
        Ok(match k {
            "server/init" => Self::Init(read(k, payload)?),
            "server/error" => Self::Error(read(k, payload)?),
            "noise/handshake" => Self::Handshake(read(k, payload)?),
            "server/hello" => Self::Hello(ServerHello::checked(read(k, payload)?)?),
            "server/activate" => Self::Activate(read(k, payload)?),
            "server/time" => Self::Time(read(k, payload)?),
            "server/state" => Self::State(read(k, payload)?),
            "server/command" => Self::Command(read(k, payload)?),
            "group/update" => Self::Group(GroupUpdate::checked(read(k, payload)?)?),
            "stream/start" => Self::StreamStart(read(k, payload)?),
            "stream/clear" => Self::StreamClear(StreamClear::checked(read(k, payload)?)?),
            "stream/end" => Self::StreamEnd(StreamEnd::checked(read(k, payload)?)?),
            "server/pair-finalize" => Self::PairFinalize,
            "pair/abort" => Self::PairAbort(read(k, payload)?),
            "server/unpair" => Self::Unpair,
            "server/pair-init" | "server/pair-auth" | "server/pair-confirm" => {
                Self::CodePairing(kind)
            }
            _ if CLIENT_TYPES.contains(&k) => Self::FromClient(kind),
            _ => Self::Unknown(kind),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerInit {
    pub server_id: String,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerError {
    pub reason: String,
}

/// One Noise handshake message, in base64url. Sent both ways.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoiseHandshake {
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerHello {
    pub name: String,
    pub languages: Option<Vec<String>>,
}

impl ServerHello {
    fn checked(mut self) -> Result<Self, MessageError> {
        cut(&mut self.name, NAME_MAX);
        if self.languages.as_ref().is_some_and(Vec::is_empty) {
            return Err(MessageError::OutOfRange {
                kind: "server/hello",
                field: "languages",
            });
        }
        Ok(self)
    }
}

/// Kept as the server wrote it: the connection judges it against the session's key.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServerActivate {
    pub activities: Vec<String>,
    pub active_roles: Option<Vec<String>>,
    /// Read only when `activities` includes `pairing`.
    pub pairing: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PairingParams {
    pub method: String,
    pub format: Option<String>,
}

impl PairingParams {
    pub fn from_value(value: Value) -> Result<Self, MessageError> {
        read("server/activate", value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ServerTime {
    pub client_transmitted: i64,
    pub server_received: i64,
    pub server_transmitted: i64,
}

/// Role objects stay unread until the connection knows the role is active.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServerState {
    pub metadata: Option<Value>,
    pub controller: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServerCommand {
    pub player: Option<Value>,
}

/// The player object of `server/command`. The connection checks `command` against what it
/// reported, then the value that goes with it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PlayerCommand {
    pub command: String,
    pub volume: Option<i64>,
    pub mute: Option<bool>,
}

impl PlayerCommand {
    pub fn from_value(value: Value) -> Result<Self, MessageError> {
        read("server/command", value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Playing,
    /// Also a paused group.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupUpdate {
    pub playback_state: PlaybackState,
    pub group_id: String,
    pub group_name: String,
}

impl GroupUpdate {
    fn checked(mut self) -> Result<Self, MessageError> {
        cut(&mut self.group_name, NAME_MAX);
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamStart {
    pub server_transmitted: i64,
    pub player: Option<Value>,
}

/// Whether a `roles` list, absent meaning every stream, names `role`.
fn names(roles: &Option<Vec<String>>, role: &str) -> bool {
    roles.as_ref().is_none_or(|r| r.iter().any(|x| x == role))
}

fn non_empty(roles: &Option<Vec<String>>, kind: &'static str) -> Result<(), MessageError> {
    if roles.as_ref().is_some_and(Vec::is_empty) {
        return Err(MessageError::OutOfRange {
            kind,
            field: "roles",
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StreamClear {
    pub server_transmitted: i64,
    pub from_timestamp: Option<i64>,
    pub roles: Option<Vec<String>>,
}

impl StreamClear {
    fn checked(self) -> Result<Self, MessageError> {
        non_empty(&self.roles, "stream/clear")?;
        Ok(self)
    }

    pub fn targets(&self, role: &str) -> bool {
        names(&self.roles, role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StreamEnd {
    pub roles: Option<Vec<String>>,
}

impl StreamEnd {
    fn checked(self) -> Result<Self, MessageError> {
        non_empty(&self.roles, "stream/end")?;
        Ok(self)
    }

    pub fn targets(&self, role: &str) -> bool {
        names(&self.roles, role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PairAbort {
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Opus,
    Flac,
    Pcm,
}

/// One entry of `supported_formats`, and the format a stream uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub codec: Codec,
    pub channels: u16,
    pub sample_rate: u32,
    /// Meaningful for PCM and FLAC only.
    pub bit_depth: u16,
}

/// The format a `stream/start` sets for the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFormat {
    pub format: AudioFormat,
    /// For FLAC: `fLaC` and the STREAMINFO block, decoded.
    pub codec_header: Option<Vec<u8>>,
}

impl StreamFormat {
    /// The format must be one the player offered, and FLAC must come with its header.
    pub fn from_value(value: Value, offered: &[AudioFormat]) -> Result<Self, MessageError> {
        #[derive(Deserialize)]
        struct Raw {
            codec: Codec,
            sample_rate: i64,
            channels: i64,
            bit_depth: i64,
            codec_header: Option<String>,
        }
        let raw: Raw = read("stream/start", value)?;
        let format = offered
            .iter()
            .find(|f| {
                f.codec == raw.codec
                    && i64::from(f.sample_rate) == raw.sample_rate
                    && i64::from(f.channels) == raw.channels
                    && (f.codec == Codec::Opus || i64::from(f.bit_depth) == raw.bit_depth)
            })
            .copied()
            .ok_or(MessageError::OutOfRange {
                kind: "stream/start",
                field: "player format",
            })?;
        let malformed = |detail: &str| MessageError::Malformed {
            kind: "stream/start".into(),
            detail: detail.into(),
        };
        let codec_header = match raw.codec_header {
            Some(text) => Some(
                b64::decode_standard(&text)
                    .ok_or_else(|| malformed("codec_header is not standard base64"))?,
            ),
            None if format.codec == Codec::Flac => {
                return Err(malformed("FLAC needs a codec_header"));
            }
            None => None,
        };
        Ok(Self {
            format,
            codec_header,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Progress {
    /// Position in ms at `timestamp`.
    pub track_progress: i64,
    /// In ms; 0 when unknown or unlimited.
    pub track_duration: i64,
    /// Speed × 1000; 0 while paused.
    pub playback_speed: i64,
}

/// What is playing, from the metadata role.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Metadata {
    /// Server time in µs at which this takes effect.
    pub timestamp: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
    pub year: Option<i64>,
    pub track: Option<i64>,
    pub progress: Option<Progress>,
}

impl Metadata {
    pub fn from_value(value: Value) -> Result<Self, MessageError> {
        let mut m: Self = read("server/state", value)?;
        for text in [
            &mut m.title,
            &mut m.artist,
            &mut m.album_artist,
            &mut m.album,
        ]
        .into_iter()
        .flatten()
        {
            cut(text, METADATA_TEXT_MAX);
        }
        // A cut URL would name something else.
        m.artwork_url = m.artwork_url.filter(|u| u.len() <= ARTWORK_URL_MAX);
        m.track = m.track.filter(|&t| t >= 1);
        Ok(m)
    }

    /// The track position in ms at server time `server_now_us`, extrapolated as the
    /// specification computes it. `None` without progress.
    pub fn position_ms(&self, server_now_us: i64) -> Option<i64> {
        let p = self.progress?;
        let elapsed = (i128::from(server_now_us) - i128::from(self.timestamp))
            * i128::from(p.playback_speed)
            / 1_000_000;
        let mut position = i128::from(p.track_progress) + elapsed;
        if p.track_duration != 0 {
            position = position.min(i128::from(p.track_duration));
        }
        i64::try_from(position.max(0)).ok()
    }
}

/// A command a controller can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerCommandKind {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Volume,
    Mute,
    RepeatOff,
    RepeatOne,
    RepeatAll,
    Shuffle,
    Unshuffle,
    Switch,
    Seek,
    SeekRelative,
}

impl ControllerCommandKind {
    fn from_wire(text: &str) -> Option<Self> {
        Some(match text {
            "play" => Self::Play,
            "pause" => Self::Pause,
            "stop" => Self::Stop,
            "next" => Self::Next,
            "previous" => Self::Previous,
            "volume" => Self::Volume,
            "mute" => Self::Mute,
            "repeat_off" => Self::RepeatOff,
            "repeat_one" => Self::RepeatOne,
            "repeat_all" => Self::RepeatAll,
            "shuffle" => Self::Shuffle,
            "unshuffle" => Self::Unshuffle,
            "switch" => Self::Switch,
            "seek" => Self::Seek,
            "seek_relative" => Self::SeekRelative,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    Off,
    One,
    All,
}

/// The group's controls, from the controller role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerState {
    /// The commands the server accepts now. Values this player does not know are left out.
    pub supported_commands: Vec<ControllerCommandKind>,
    pub volume: u8,
    pub muted: bool,
    pub repeat: Repeat,
    pub shuffle: bool,
    pub seek_max_ms: Option<i64>,
}

impl ControllerState {
    pub fn from_value(value: Value) -> Result<Self, MessageError> {
        #[derive(Deserialize)]
        struct Raw {
            supported_commands: Vec<String>,
            volume: i64,
            muted: bool,
            repeat: Repeat,
            shuffle: bool,
            seek_max_ms: Option<i64>,
        }
        let raw: Raw = read("server/state", value)?;
        let volume = u8::try_from(raw.volume)
            .ok()
            .filter(|v| *v <= VOLUME_MAX)
            .ok_or(MessageError::OutOfRange {
                kind: "server/state",
                field: "controller.volume",
            })?;
        Ok(Self {
            supported_commands: raw
                .supported_commands
                .iter()
                .filter_map(|c| ControllerCommandKind::from_wire(c))
                .collect(),
            volume,
            muted: raw.muted,
            repeat: raw.repeat,
            shuffle: raw.shuffle,
            seek_max_ms: raw.seek_max_ms,
        })
    }
}

// What the player sends.

#[derive(Serialize)]
struct Outgoing<'a, T> {
    #[serde(rename = "type")]
    kind: &'a str,
    payload: &'a T,
}

/// One message as the wire carries it.
pub fn encode<T: Serialize>(kind: &str, payload: &T) -> Result<String, MessageError> {
    serde_json::to_string(&Outgoing { kind, payload })
        .map_err(|e| MessageError::Encode(format!("{kind}: {e}")))
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientInit<'a> {
    pub client_id: &'a str,
    pub version: u8,
    pub suite: &'a str,
}

/// What `client/hello` says about the device. Each string is plain text of at most 64 bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software_version: Option<String>,
    /// Lowercase and colon-separated, such as `aa:bb:cc:dd:ee:ff`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientHello<'a> {
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_info: Option<&'a DeviceInfo>,
    pub supported_roles: Vec<&'static str>,
    #[serde(rename = "player@v1_support", skip_serializing_if = "Option::is_none")]
    pub player_support: Option<PlayerSupport<'a>>,
    pub supported_pair_methods: PairMethods,
    pub unpaired_access: UnpairedAccess,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerSupport<'a> {
    pub supported_formats: &'a [AudioFormat],
    pub buffer_capacity: u32,
}

/// Only the Pairing PSK method, without `locations`: nothing hands the token to an owner yet
/// (ADR 0027 §5).
#[derive(Debug, Clone, Default, Serialize)]
pub struct PairMethods {
    pub pairing_psk: Empty,
}

/// `{}`.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Empty {}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct UnpairedAccess {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ClientTime {
    pub client_transmitted: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientState {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerStateObject>,
}

/// The player's state as `client/state` reports it, always in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlayerStateObject {
    pub volume: u8,
    pub muted: bool,
    pub output_delay_ms: u32,
    pub required_lead_time_ms: u32,
    pub min_buffer_ms: u32,
    pub supported_commands: Vec<PlayerCommandKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<AudioFormat>,
}

/// The player commands a server may send. `set_output_delay` is not offered: the speaker is
/// built in (ADR 0027 §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerCommandKind {
    Volume,
    Mute,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientCommand {
    pub controller: ControllerCommandObject,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControllerCommandObject {
    pub command: ControllerCommandKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<i64>,
}

/// Why the player ends a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoodbyeReason {
    AnotherServer,
    Shutdown,
    Restart,
    UserRequest,
    Unauthorized,
    PairingRequired,
    ConcurrentAttempt,
    Unpaired,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ClientGoodbye {
    pub reason: GoodbyeReason,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ClientPairInit {
    pub pairing_index: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientPairFinalize {
    pub long_term_psk: String,
}

/// Why the player aborts a pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairAbortReason {
    AttemptTimeout,
    ConcurrentAttempt,
    MethodNotSupported,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ClientPairAbort {
    pub reason: PairAbortReason,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> Result<ServerMessage, MessageError> {
        ServerMessage::from_envelope(envelope(value.to_string().as_bytes())?)
    }

    fn out_of_range(result: Result<ServerMessage, MessageError>) -> &'static str {
        match result {
            Err(MessageError::OutOfRange { field, .. }) => field,
            other => panic!("expected out of range, got {other:?}"),
        }
    }

    const GROUP: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA-";

    #[test]
    fn envelope_rules() {
        assert!(envelope(br#"{"type":"x","payload":{}}"#).is_ok());
        for bad in [
            &br#"{"type":"x"}"#[..],
            br#"{"payload":{}}"#,
            br#"{"type":1,"payload":{}}"#,
            br#"{"type":"x","payload":[]}"#,
            br#"["x",{}]"#,
            br#"nope"#,
            b"{\"type\":\"\xff\",\"payload\":{}}",
        ] {
            assert_eq!(envelope(bad), Err(MessageError::NotEnvelope), "{bad:?}");
        }
    }

    #[test]
    fn size_and_depth_limits() {
        let pad = " ".repeat(JSON_MAX - r#"{"type":"x","payload":{}}"#.len());
        let fits = format!(r#"{{"type":"x","payload":{{}}}}{pad}"#);
        assert_eq!(fits.len(), JSON_MAX);
        assert!(envelope(fits.as_bytes()).is_ok());
        assert_eq!(
            envelope(format!("{fits} ").as_bytes()),
            Err(MessageError::TooLarge)
        );

        // The root is level 1; `payload` is level 2; eight more arrays reach 10.
        let deep = |levels: usize| {
            format!(
                r#"{{"type":"x","payload":{{"a":{}{}}}}}"#,
                "[".repeat(levels),
                "]".repeat(levels)
            )
        };
        assert!(envelope(deep(8).as_bytes()).is_ok());
        assert_eq!(envelope(deep(9).as_bytes()), Err(MessageError::TooDeep));
        // Brackets inside strings, escaped quotes included, do not count.
        let quoted = format!(r#"{{"type":"x","payload":{{"a":"\"{}"}}}}"#, "[".repeat(20));
        assert!(envelope(quoted.as_bytes()).is_ok());
    }

    #[test]
    fn unknown_fields_and_types_are_ignored() {
        let m = parse(json!({"type": "server/time", "payload": {
            "client_transmitted": 1, "server_received": 2, "server_transmitted": 3, "new": [1]}}));
        assert!(matches!(m, Ok(ServerMessage::Time(_))));
        assert_eq!(
            parse(json!({"type": "server/future", "payload": {"x": 1}})),
            Ok(ServerMessage::Unknown("server/future".into()))
        );
        assert_eq!(
            parse(json!({"type": "client/state", "payload": {}})),
            Ok(ServerMessage::FromClient("client/state".into()))
        );
        assert_eq!(
            parse(json!({"type": "server/pair-auth", "payload": {}})),
            Ok(ServerMessage::CodePairing("server/pair-auth".into()))
        );
    }

    #[test]
    fn wrong_types_in_known_fields_are_malformed() {
        assert!(matches!(
            parse(json!({"type": "server/time", "payload": {"client_transmitted": "1"}})),
            Err(MessageError::Malformed { .. })
        ));
        assert!(matches!(
            parse(json!({"type": "group/update", "payload": {
                "playback_state": "paused", "group_id": GROUP, "group_name": "x"}})),
            Err(MessageError::Malformed { .. })
        ));
    }

    #[test]
    fn server_hello_limits() {
        let hello = |name: &str, languages: Value| {
            parse(
                json!({"type": "server/hello", "payload": {"name": name, "languages": languages}}),
            )
        };
        let name = |message| match message {
            Ok(ServerMessage::Hello(h)) => h.name,
            other => panic!("expected a server/hello, got {other:?}"),
        };
        assert_eq!(name(hello(&"é".repeat(32), json!(["en"]))), "é".repeat(32));
        // rc1 sets no limit: a longer name is cut to the later one, on a character boundary.
        assert_eq!(name(hello(&"é".repeat(33), json!(null))), "é".repeat(32));
        assert_eq!(out_of_range(hello("x", json!([]))), "languages");
    }

    #[test]
    fn group_updates_are_read_as_rc1_defines_them() {
        let group = |id: &str, name: &str| match parse(json!({"type": "group/update", "payload": {
                "playback_state": "playing", "group_id": id, "group_name": name}}))
        {
            Ok(ServerMessage::Group(g)) => g,
            other => panic!("expected a group/update, got {other:?}"),
        };
        assert_eq!(group(GROUP, &"x".repeat(64)).group_name, "x".repeat(64));
        // Music Assistant 2.10.7 to 2.11 (aiosendspin 10.2 and 10.3) sends a UUID.
        let uuid = "6b1d7a52-3c1e-4f43-9a51-0d1c2b8e7f60";
        assert_eq!(group(uuid, "x").group_id, uuid);
        assert_eq!(group(&GROUP[1..], "x").group_id, GROUP[1..]);
        assert_eq!(group(GROUP, &"x".repeat(65)).group_name, "x".repeat(64));
    }

    #[test]
    fn stream_roles_must_not_be_empty() {
        assert_eq!(
            out_of_range(parse(json!({"type": "stream/clear", "payload": {
                "server_transmitted": 1, "roles": []}}))),
            "roles"
        );
        assert_eq!(
            out_of_range(parse(
                json!({"type": "stream/end", "payload": {"roles": []}})
            )),
            "roles"
        );
        let Ok(ServerMessage::StreamEnd(end)) =
            parse(json!({"type": "stream/end", "payload": {"roles": ["artwork"]}}))
        else {
            panic!()
        };
        assert!(!end.targets("player"));
        let Ok(ServerMessage::StreamClear(clear)) =
            parse(json!({"type": "stream/clear", "payload": {"server_transmitted": 1}}))
        else {
            panic!()
        };
        assert!(clear.targets("player"));
    }

    #[test]
    fn metadata_limits() {
        let base = json!({"timestamp": 5});
        assert!(Metadata::from_value(base.clone()).is_ok());
        let texts = |m: Metadata| [m.title, m.artist, m.album_artist, m.album];
        let mut ok = base.clone();
        let mut long = base.clone();
        for field in ["title", "artist", "album_artist", "album"] {
            ok[field] = json!("x".repeat(METADATA_TEXT_MAX));
            // rc1 sets no limit: longer text is cut to the later one, on a character boundary.
            long[field] = json!(format!("{}é", "x".repeat(METADATA_TEXT_MAX - 1)));
        }
        let want = Some("x".repeat(METADATA_TEXT_MAX));
        assert_eq!(
            texts(Metadata::from_value(ok).unwrap()),
            [(); 4].map(|_| want.clone())
        );
        let want = Some("x".repeat(METADATA_TEXT_MAX - 1));
        assert_eq!(
            texts(Metadata::from_value(long).unwrap()),
            [(); 4].map(|_| want.clone())
        );
        let url = |len: usize| {
            let mut m = base.clone();
            m["artwork_url"] = json!("x".repeat(len));
            Metadata::from_value(m).unwrap().artwork_url
        };
        assert_eq!(url(ARTWORK_URL_MAX), Some("x".repeat(ARTWORK_URL_MAX)));
        // A cut URL would name something else.
        assert_eq!(url(ARTWORK_URL_MAX + 1), None);
        let track = |t: i64| {
            Metadata::from_value(json!({"timestamp": 5, "track": t}))
                .unwrap()
                .track
        };
        assert_eq!(track(1), Some(1));
        assert_eq!(track(0), None);
        assert!(matches!(
            Metadata::from_value(json!({"title": "x"})),
            Err(MessageError::Malformed { .. })
        ));
    }

    #[test]
    fn metadata_position() {
        let m = Metadata::from_value(json!({"timestamp": 1_000_000, "progress": {
            "track_progress": 10_000, "track_duration": 12_000, "playback_speed": 1000}}))
        .unwrap();
        assert_eq!(m.position_ms(1_000_000), Some(10_000));
        assert_eq!(m.position_ms(2_500_000), Some(11_500));
        assert_eq!(m.position_ms(9_000_000), Some(12_000));
        assert_eq!(m.position_ms(-20_000_000), Some(0));
        // A negative duration from the server must not panic.
        let odd = Metadata::from_value(json!({"timestamp": 0, "progress": {
            "track_progress": 5, "track_duration": -3, "playback_speed": i64::MAX}}))
        .unwrap();
        assert_eq!(odd.position_ms(i64::MAX), Some(0));
        assert_eq!(
            Metadata::from_value(json!({"timestamp": 0}))
                .unwrap()
                .position_ms(0),
            None
        );
    }

    #[test]
    fn controller_state() {
        let state = |volume: i64, commands: Value| {
            ControllerState::from_value(json!({"supported_commands": commands, "volume": volume,
                "muted": false, "repeat": "all", "shuffle": true}))
        };
        let s = state(100, json!(["play", "teleport", "seek"])).unwrap();
        assert_eq!(
            s.supported_commands,
            vec![ControllerCommandKind::Play, ControllerCommandKind::Seek]
        );
        assert_eq!(s.repeat, Repeat::All);
        assert!(state(0, json!([])).is_ok());
        for volume in [-1, 101, 300] {
            assert!(matches!(
                state(volume, json!([])),
                Err(MessageError::OutOfRange { .. })
            ));
        }
        assert!(matches!(
            ControllerState::from_value(json!({"supported_commands": [], "volume": 1,
                "muted": false, "repeat": "sometimes", "shuffle": true})),
            Err(MessageError::Malformed { .. })
        ));
    }

    #[test]
    fn stream_format() {
        let offered = [
            AudioFormat {
                codec: Codec::Flac,
                channels: 2,
                sample_rate: 48000,
                bit_depth: 16,
            },
            AudioFormat {
                codec: Codec::Pcm,
                channels: 2,
                sample_rate: 44100,
                bit_depth: 16,
            },
        ];
        let flac = StreamFormat::from_value(
            json!({"codec": "flac", "sample_rate": 48000, "channels": 2, "bit_depth": 16,
                "codec_header": "ZkxhQw=="}),
            &offered,
        )
        .unwrap();
        assert_eq!(flac.format, offered[0]);
        assert_eq!(flac.codec_header.as_deref(), Some(&b"fLaC"[..]));
        let pcm = StreamFormat::from_value(
            json!({"codec": "pcm", "sample_rate": 44100, "channels": 2, "bit_depth": 16}),
            &offered,
        )
        .unwrap();
        assert_eq!(pcm.codec_header, None);

        let refused = |value: Value| StreamFormat::from_value(value, &offered).unwrap_err();
        assert!(matches!(
            refused(json!({"codec": "pcm", "sample_rate": 48000, "channels": 2, "bit_depth": 16})),
            MessageError::OutOfRange { .. }
        ));
        assert!(matches!(
            refused(json!({"codec": "pcm", "sample_rate": 44100, "channels": 2, "bit_depth": 24})),
            MessageError::OutOfRange { .. }
        ));
        assert!(matches!(
            refused(json!({"codec": "flac", "sample_rate": 48000, "channels": 2, "bit_depth": 16})),
            MessageError::Malformed { .. }
        ));
        assert!(matches!(
            refused(
                json!({"codec": "flac", "sample_rate": 48000, "channels": 2, "bit_depth": 16,
                "codec_header": "ZkxhQw"})
            ),
            MessageError::Malformed { .. }
        ));
        assert!(matches!(
            refused(json!({"codec": "mp3", "sample_rate": 48000, "channels": 2, "bit_depth": 16})),
            MessageError::Malformed { .. }
        ));
    }

    #[test]
    fn absent_optional_fields_are_not_written() {
        let state = ClientState {
            available: true,
            player: None,
        };
        assert_eq!(
            encode("client/state", &state).unwrap(),
            r#"{"type":"client/state","payload":{"available":true}}"#
        );
        let command = ClientCommand {
            controller: ControllerCommandObject {
                command: ControllerCommandKind::SeekRelative,
                volume: None,
                mute: None,
                position_ms: None,
                offset_ms: Some(-5000),
            },
        };
        assert_eq!(
            encode("client/command", &command).unwrap(),
            r#"{"type":"client/command","payload":{"controller":{"command":"seek_relative","offset_ms":-5000}}}"#
        );
        assert_eq!(serde_json::to_string(&DeviceInfo::default()).unwrap(), "{}");
        assert_eq!(
            serde_json::to_string(&PairMethods::default()).unwrap(),
            r#"{"pairing_psk":{}}"#
        );
    }
}
