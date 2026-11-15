//! LADSPA plugin `pinna_beam`: the Echo Dot microphone front end (calibration,
//! beamformer, 80 Hz high-pass, digital gain) for PipeWire's filter-chain.
//!
//! Runs at 16 kHz only. Seven inputs are mic lanes 0-6, one output is the
//! beamformed mono signal. PipeWire hands over floats in [-1, 1]; `x * 32768`
//! is the i16 value the integer front end in `pinna-audio` works on.
//!
//! After every cycle the plugin publishes the beamformer's state to
//! `/run/pipewire/pinna-beam` (`pinna_audio::beamview`), which pinnad reads to
//! point the listening ring at the talker.
//!
//! The LADSPA 1.1 header is small enough to mirror by hand ([`ladspa`]).

pub mod ladspa;
mod plugin;

pub use plugin::{
    DEFAULT_BEAMS, DEFAULT_GAIN_DB, DEFAULT_MIC_MASK, LABEL, SAMPLE_RATE, UNIQUE_ID,
    ladspa_descriptor,
};
