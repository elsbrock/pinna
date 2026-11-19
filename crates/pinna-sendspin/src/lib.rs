//! The Sendspin protocol for a player that servers connect to (ADR 0027), without audio and
//! without I/O: the player's identity, the pre-shared keys, the Noise responder, binary frames
//! and the reassembly of fragments, and the messages with their range checks.
//!
//! Targets the Sendspin specification 1.0.0-rc1 with the player timing rules added on its main
//! branch up to 2026-11-11, as ADR 0027 records. Only the server-initiated direction is
//! built: the player listens, and the server connects and is the Noise initiator.

mod b64;
pub mod frame;
pub mod identity;
pub mod messages;
pub mod noise;
pub mod psk;

pub use identity::Identity;
pub use messages::{
    AudioFormat, Codec, ControllerState, DeviceInfo, GoodbyeReason, GroupUpdate, Metadata,
    PlayerCommandKind,
};
pub use psk::{Psk, PskCategory};
