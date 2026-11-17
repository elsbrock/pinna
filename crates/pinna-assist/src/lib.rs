//! One standalone voice request, without device state (ADR 0017).
//!
//! The crate is protocol-only. It knows the preset table, the run limits, the
//! system prompt, the sentence splitter and the `standalone.toml` codec; the
//! Brain in `pinnad` owns the phase, the microphone and the voice lane.

pub mod limits;
pub mod presets;
pub mod prompt;
pub mod sentences;
