//! LADSPA plugins for PipeWire's filter-chain on the Echo Dot:
//! - `pinna_beam`: the microphone front end (calibration, beamformer, 80 Hz
//!   high-pass, digital gain);
//! - `pinna_wake`: the wake word's stream, the beam's microphones averaged
//!   and the speaker's echo cancelled linearly from the loopback lane
//!   (ADR 0033, [`wake`]).
//!
//! Both run at 16 kHz only and have one mono output. `pinna_beam`'s seven
//! inputs are mic lanes 0-6; `pinna_wake` also takes loopback lane 7.
//! PipeWire hands over floats in [-1, 1]; `x * 32768` is the i16 value the
//! integer front end in `pinna-audio` works on.
//!
//! After every cycle `pinna_beam` publishes the beamformer's state to
//! `/run/pipewire/pinna-beam` (`pinna_audio::beamview`), which pinnad reads to
//! point the listening ring at the talker.
//!
//! The LADSPA 1.1 header is small enough to mirror by hand ([`ladspa`]).

pub mod ladspa;
mod plugin;
pub mod wake;

pub use plugin::{
    DEFAULT_BEAMS, DEFAULT_GAIN_DB, DEFAULT_MIC_MASK, LABEL, SAMPLE_RATE, UNIQUE_ID,
    ladspa_descriptor,
};
