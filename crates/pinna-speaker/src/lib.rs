//! Recognizes who is speaking (ADR 0038): a voiceprint is the 192 numbers 3D-Speaker's CAM++
//! model gives for 1.5 s of speech, and a request matches the enrolled person whose voiceprint
//! is closest. Audio is 16 kHz mono `i16` PCM throughout.
//!
//! Cost: one voiceprint of 1.5 s took 764 ms on one core of the Echo Dot (2nd gen), with tract
//! 0.23.10, measured 2026-11-27. Peak memory was about 71 MB.

pub mod fbank;

mod enroll;
mod matching;
mod model;
mod voiceprint;

pub use enroll::{EnrollError, Enrolled, check_speech, enroll};
pub use matching::{CLOSE_PAIR, Match, Thresholds, best_match, close};
pub use model::Model;
pub use voiceprint::Voiceprint;

use pinna_vad::Vad;

/// 1.5 s, the audio one voiceprint is computed from.
pub const WINDOW_SAMPLES: usize = 24_000;
/// 0.5 s, the least speech a voiceprint is computed from.
pub const MIN_SPEECH_SAMPLES: usize = 8_000;

const FRAME: usize = 160;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Can't read the speaker model {path}: {cause}. Check that the file exists.")]
    Read { path: String, cause: String },
    #[error("Can't load the speaker model {path}: {cause}. Download it again with `just models`.")]
    Load { path: String, cause: String },
    #[error("The speaker model failed to run: {0}")]
    Run(String),
    #[error("A voiceprint needs exactly {WINDOW_SAMPLES} samples (1.5 s at 16 kHz), got {0}.")]
    WindowLength(usize),
    #[error("Voiceprint bytes are invalid: {0}")]
    Bytes(&'static str),
}

/// Keeps the frames the VAD calls speech. Requests and enrollment both use it, so leading
/// silence doesn't count as speech. `process` rather than `classify`: its onset and hangover
/// keep a word's quiet ends and drop isolated clicks.
pub fn speech(pcm: &[i16]) -> Vec<i16> {
    let mut vad = Vad::new();
    let mut out = Vec::new();
    for frame in pcm.chunks_exact(FRAME) {
        if vad.process(frame) {
            out.extend_from_slice(frame);
        }
    }
    out
}

/// One voiceprint window from speech: its first 1.5 s, or the speech repeated end to end until
/// it fills 1.5 s. `None` under 0.5 s, too little to say who spoke.
pub fn fill_window(speech: &[i16]) -> Option<Vec<i16>> {
    if speech.len() < MIN_SPEECH_SAMPLES {
        return None;
    }
    Some(
        speech
            .iter()
            .copied()
            .cycle()
            .take(WINDOW_SAMPLES)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_window_rejects_short_speech() {
        assert!(fill_window(&vec![1; MIN_SPEECH_SAMPLES - 1]).is_none());
    }

    #[test]
    fn fill_window_repeats_and_truncates() {
        let short: Vec<i16> = (0..12_800).map(|i| i as i16).collect();
        let w = fill_window(&short).unwrap();
        assert_eq!(w.len(), WINDOW_SAMPLES);
        assert_eq!(&w[..12_800], &short[..]);
        assert_eq!(w[12_800], short[0]);
        let long: Vec<i16> = (0..48_000).map(|i| i as i16).collect();
        assert_eq!(fill_window(&long).unwrap(), long[..WINDOW_SAMPLES]);
    }
}
