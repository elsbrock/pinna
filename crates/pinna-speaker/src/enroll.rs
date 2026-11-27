//! Enrollment: checks a recording and turns it into one voiceprint (ADR 0038).

use crate::{Error, Model, Voiceprint, WINDOW_SAMPLES, speech};

/// All provisional (ADR 0038): the owner's recordings set them.
const MIN_SPEECH_SECONDS: f32 = 10.0;
const MIN_DBFS: f32 = -45.0;
const CLIP_LEVEL: i32 = 32_700;
const MAX_CLIPPED_FRACTION: f32 = 0.001;
/// A window whose cosine to the mean is lower isn't the same voice as the rest.
const MIN_WINDOW_AGREEMENT: f32 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub struct Enrolled {
    pub voiceprint: Voiceprint,
    pub speech_seconds: f32,
    pub windows: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    #[error("Too short: {speech_seconds:.1} s of speech. Speak for at least 10 s.")]
    TooShort { speech_seconds: f32 },
    #[error(
        "Too quiet: the speech is at {dbfs:.0} dBFS. Stand closer to the satellite, within 1 m."
    )]
    TooQuiet { dbfs: f32 },
    #[error(
        "Clipped: {:.1}% of the speech is at full volume. Speak more softly or stand farther from the satellite.",
        fraction * 100.0
    )]
    Clipped { fraction: f32 },
    #[error(
        "Inconsistent: part of the recording doesn't sound like the rest (agreement {worst:.2}). Record again with only one person speaking."
    )]
    Inconsistent { worst: f32 },
    #[error(transparent)]
    Model(#[from] Error),
}

pub fn enroll(model: &Model, pcm: &[i16]) -> Result<Enrolled, EnrollError> {
    let speech = speech(pcm);
    check_speech(&speech)?;
    let voiceprints = speech
        .chunks_exact(WINDOW_SAMPLES)
        .map(|w| model.voiceprint(w))
        .collect::<Result<Vec<_>, _>>()?;
    let mean = Voiceprint::mean(&voiceprints).ok_or(Error::Run(
        "the model gave voiceprints that can't be averaged".into(),
    ))?;
    let worst = voiceprints
        .iter()
        .map(|v| v.cosine(&mean))
        .fold(f32::MAX, f32::min);
    if worst < MIN_WINDOW_AGREEMENT {
        return Err(EnrollError::Inconsistent { worst });
    }
    Ok(Enrolled {
        voiceprint: mean,
        speech_seconds: seconds(&speech),
        windows: voiceprints.len(),
    })
}

fn seconds(speech: &[i16]) -> f32 {
    speech.len() as f32 / 16_000.0
}

/// The checks that need no model, in the order the owner can act on them. `speech` is what
/// [`speech`] kept. Public so a caller can reject a recording before it loads the model.
pub fn check_speech(speech: &[i16]) -> Result<(), EnrollError> {
    let speech_seconds = seconds(speech);
    if speech_seconds < MIN_SPEECH_SECONDS {
        return Err(EnrollError::TooShort { speech_seconds });
    }
    let mean_square = speech
        .iter()
        .map(|&s| f64::from(s) * f64::from(s))
        .sum::<f64>()
        / speech.len() as f64;
    let dbfs = (10.0 * (mean_square / (32768.0 * 32768.0)).log10()) as f32;
    if dbfs < MIN_DBFS {
        return Err(EnrollError::TooQuiet { dbfs });
    }
    let clipped = speech
        .iter()
        .filter(|&&s| i32::from(s).abs() >= CLIP_LEVEL)
        .count();
    let fraction = clipped as f32 / speech.len() as f32;
    if fraction > MAX_CLIPPED_FRACTION {
        return Err(EnrollError::Clipped { fraction });
    }
    Ok(())
}
