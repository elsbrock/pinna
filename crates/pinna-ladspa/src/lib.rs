//! LADSPA plugins for PipeWire's filter-chain on the MT8163 Echos:
//! - `pinna_wake`: the satellite's microphone stream (ADR 0004, ADR 0033):
//!   the board's microphones averaged and the speaker's echo cancelled
//!   linearly from the loopback lane it plays ([`wake`]). It also runs the
//!   beamformer for the ring's direction and publishes its state to
//!   `/run/pipewire/pinna-beam` (`pinna_audio::beamview`) after every cycle.
//! - `pinna_beam`: calibration, LibreEcho's beamformer, 80 Hz high-pass and
//!   digital gain on mic lanes 0-6. The Echos' graph no longer loads it
//!   (ADR 0004); it stays for experiments with other graphs.
//!
//! Both run at 16 kHz only and have one mono output. PipeWire hands over floats
//! in [-1, 1]; `x * 32768` is the i16 value the integer front end in
//! `pinna-audio` works on.
//!
//! The LADSPA 1.1 header is small enough to mirror by hand ([`ladspa`]).

pub mod ladspa;
mod plugin;
pub mod wake;

pub use plugin::{
    DEFAULT_BEAMS, DEFAULT_GAIN_DB, DEFAULT_MIC_MASK, LABEL, SAMPLE_RATE, UNIQUE_ID,
    ladspa_descriptor,
};
