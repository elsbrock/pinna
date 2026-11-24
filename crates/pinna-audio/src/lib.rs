//! Audio capture and playback for Pinna.
//!
//! No C bindings: audio moves through subprocesses (`arecord`/`aplay`/
//! `amixer` from alsa-utils on the device, `pw-record`/`pw-play` on a laptop).

pub mod agc;
pub mod beamformer;
pub mod beamview;
pub mod capture;
pub mod chain;
pub mod chunk;
pub mod clock;
pub mod decode;
pub mod dsp;
pub mod echocancel;
pub mod media;
pub(crate) mod mix;
pub mod mixer;
pub mod playlist;
pub mod refsink;
pub mod speaker;
pub mod spectrum;
pub mod timed;
pub mod tone;
pub mod track;
pub mod upsample;
pub mod volume;

use std::io;

pub use agc::Agc;
pub use capture::{CaptureConfig, CaptureStatus, CaptureStream, start_capture};
pub use decode::{Pcm, decode};
pub use media::{MediaHandle, MediaPhase, MediaState, STREAM_URL};
pub use speaker::{PlaybackState, Speaker, SpeakerConfig, VoiceLevel, VoicePlayback};
pub use spectrum::{Bands, Spectrum};
pub use timed::{EndReason, Refused, ServerClock, TimedSink, TimedState, TimedStats};
pub use track::Track;

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
    /// Echo Dot with the device's PipeWire microphone graph: the microphones'
    /// mean and echo cancellation run in PipeWire, so capture reads one
    /// processed mono stream (`dot_wake`). The speaker is still driven through
    /// ALSA at 48 kHz (with the amp and mixer sequences here); the echo canceller
    /// takes what it plays from the microphones' loopback lanes (ADR 0004).
    EchoDotPipeWire,
    /// PipeWire tools (`pw-record` / `pw-play`) for laptops and VMs.
    PipeWire,
    /// Read the microphone signal from a WAV file; the speaker plays to
    /// nothing, in real time (tests).
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
        matches!(self, Backend::EchoDotPipeWire).then_some("dot_wake")
    }

    /// The PipeWire node of the raw microphone lanes, which the capture falls
    /// back to when the processed stream delivers nothing (ADR 0004).
    pub fn pw_raw_target(&self) -> Option<&'static str> {
        matches!(self, Backend::EchoDotPipeWire).then_some("dot_mics")
    }

    /// Default PipeWire playback node for this backend (the plain `PipeWire` backend
    /// uses the default sink).
    pub fn pw_playback_target(&self) -> Option<&'static str> {
        None
    }
}
