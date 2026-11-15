//! Audio capture and playback for Pinna.
//!
//! No C bindings: audio moves through subprocesses (`arecord`/`aplay`/
//! `amixer` from alsa-utils on the device, `pw-record`/`pw-play` on a laptop).

pub mod capture;
pub mod decode;
pub mod dsp;
pub mod mixer;
pub mod speaker;
pub mod tone;
pub mod volume;

use std::io;

pub use capture::{CaptureConfig, CaptureStatus, CaptureStream, start_capture};
pub use decode::{Pcm, decode};
pub use speaker::{PlaybackState, Speaker, SpeakerConfig};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("mixer control failed: {0}")]
    Mixer(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Which audio stack to talk to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Echo Dot: alsa-utils against the MT8163 sound card.
    EchoDot,
    /// PipeWire tools (`pw-record` / `pw-play`) for laptops and VMs.
    PipeWire,
    /// Capture only: read a WAV file (tests).
    WavFile(std::path::PathBuf),
}
