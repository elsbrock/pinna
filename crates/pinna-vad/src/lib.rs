//! End-of-speech building blocks for standalone mode (ADR 0017): the energy VAD
//! moved from pinna-wakeword, and an endpointer that turns a stream of PCM into
//! end-of-speech decisions. No dependencies.

pub mod vad;

pub use vad::Vad;
