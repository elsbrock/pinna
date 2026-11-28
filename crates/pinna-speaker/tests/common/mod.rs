#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn model_path() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../blobs/models/speaker/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced-f16.onnx",
    );
    if path.exists() {
        Some(path)
    } else {
        eprintln!("SKIP: speaker model missing at {}", path.display());
        None
    }
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn wakeword_wav(name: &str) -> Vec<i16> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pinna-wakeword/tests/fixtures")
        .join(name);
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    assert_eq!(
        (spec.channels, spec.sample_rate, spec.bits_per_sample),
        (1, 16000, 16)
    );
    reader.samples::<i16>().map(Result::unwrap).collect()
}

const FILES: [&str; 3] = ["alexa-twice.wav", "alexa-what-time.wav", "no-wake-word.wav"];

/// The three files back to back, twice: the VAD keeps only 9.5 s of one pass, under the 10 s
/// enrollment needs.
pub fn recording() -> Vec<i16> {
    let once: Vec<i16> = FILES.iter().flat_map(|f| wakeword_wav(f)).collect();
    [once.clone(), once].concat()
}

/// What the VAD keeps of [`recording`], real speech for the checks that follow the gating.
pub fn long_speech() -> Vec<i16> {
    pinna_speaker::speech(&recording())
}
