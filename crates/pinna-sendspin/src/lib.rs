//! The Sendspin protocol for a player that servers connect to (ADR 0027): the messages and
//! their range checks, binary frames and the reassembly of fragments, the Noise responder, the
//! time filter, pairing records, and a connection that ties them together. It does no audio
//! and no I/O: `pinnad` drives a [`Connection`] from its WebSocket task and feeds the audio it
//! hands out to the media lane.
//!
//! Targets the Sendspin specification 1.0.0-rc1 with the player timing rules added on its main
//! branch up to 2026-11-11, as ADR 0027 records. Only the server-initiated direction is
//! built: the player listens, and the server connects and is the Noise initiator.
//!
//! ```ignore
//! let mut conn = Connection::new(config, &store, now())?;
//! loop {
//!     while let Some(t) = conn.poll_transmit() { /* send Text, Binary, or Close */ }
//!     while let Some(e) = conn.poll_event() { /* act on it */ }
//!     // then: conn.on_text / on_binary for each message, conn.on_tick at next_wakeup(),
//!     // conn.on_peer_closed when the server closes the socket
//! }
//! ```

mod b64;
mod budget;
pub mod clock;
pub mod connection;
pub mod frame;
pub mod identity;
pub mod messages;
pub mod noise;
pub mod pairing;
pub mod psk;

pub use clock::TimeFilter;
pub use connection::{
    ActiveRoles, Activities, AudioChunk, CloseReason, CommandError, Config, ConfigError,
    Connection, ControllerCommand, Event, PlayerState, PlayerStream, Rival, Role, StateError,
    Transmit,
};
pub use identity::Identity;
pub use messages::{
    AudioFormat, Codec, ControllerState, DeviceInfo, GoodbyeReason, GroupUpdate, Metadata,
    PlayerCommandKind,
};
pub use pairing::{PairingRecord, PairingStore};
pub use psk::{Psk, PskCategory};
