//! Audio capture and playback for Pinna.
//!
//! No C bindings: audio moves through subprocesses (`arecord`/`aplay`/
//! `amixer` from alsa-utils on the device, `pw-record`/`pw-play` on a laptop).

pub mod beamformer;
pub mod capture;
pub mod decode;
pub mod dsp;
pub mod media;
pub(crate) mod mix;
pub mod mixer;
pub mod speaker;
pub mod tone;
pub mod volume;

use std::io;

pub use capture::{CaptureConfig, CaptureStatus, CaptureStream, start_capture};
pub use decode::{Pcm, decode};
pub use media::{MediaHandle, MediaPhase, MediaState};
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
    /// Echo Dot through the device's PipeWire graph: the microphone front end
    /// (beamformer) and echo cancellation run in PipeWire, so capture reads one
    /// processed mono stream (`dot_mic_aec`) and playback goes to the echo-cancel
    /// reference sink (`dot_out`). The amp and mic mixer sequences stay here.
    EchoDotPipeWire,
    /// PipeWire tools (`pw-record` / `pw-play`) for laptops and VMs.
    PipeWire,
    /// Capture only: read a WAV file (tests).
    WavFile(std::path::PathBuf),
}

impl Backend {
    /// Runs the Echo Dot codec/amp/mic mixer sequences.
    pub fn echo_dot_mixer(&self) -> bool {
        matches!(self, Backend::EchoDot | Backend::EchoDotPipeWire)
    }

    /// Streams through `pw-record` / `pw-play`.
    pub fn pipewire(&self) -> bool {
        matches!(self, Backend::PipeWire | Backend::EchoDotPipeWire)
    }

    /// Default PipeWire capture node for this backend.
    pub fn pw_capture_target(&self) -> Option<&'static str> {
        matches!(self, Backend::EchoDotPipeWire).then_some("dot_mic_aec")
    }

    /// Default PipeWire playback node for this backend.
    pub fn pw_playback_target(&self) -> Option<&'static str> {
        matches!(self, Backend::EchoDotPipeWire).then_some("dot_out")
    }
}
