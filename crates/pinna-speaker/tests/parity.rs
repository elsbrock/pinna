mod common;

use pinna_speaker::{Model, WINDOW_SAMPLES, fbank::Fbank};

#[test]
fn fbank_matches_kaldi_native_fbank() {
    let pcm = common::wakeword_wav("alexa-what-time.wav");
    let samples: Vec<f32> = pcm[..WINDOW_SAMPLES]
        .iter()
        .map(|&s| f32::from(s) / 32768.0)
        .collect();
    let ours = Fbank::new().compute(&samples);
    let bytes = std::fs::read(common::fixture("fbank-alexa-what-time.f32")).unwrap();
    let reference: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert_eq!(ours.len(), reference.len());
    let worst = ours
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    println!("fbank worst absolute difference: {worst:e}");
    assert!(worst < 1e-2, "fbank differs by {worst}");
}

#[test]
fn voiceprint_matches_sherpa_onnx() {
    let Some(path) = common::model_path() else {
        return;
    };
    let model = Model::load(&path).unwrap();
    println!("model sha256 {}", model.sha256());
    assert_eq!(
        model.sha256(),
        "53cab20ba8f2eb2f6e2d89ecc18e88ef3222169b61b4419cbc13666744a4574b"
    );
    let pcm = common::wakeword_wav("alexa-what-time.wav");
    let ours = model.voiceprint(&pcm[..WINDOW_SAMPLES]).unwrap();
    let json = std::fs::read_to_string(common::fixture("voiceprint-alexa-what-time.json")).unwrap();
    let reference: Vec<f32> = serde_json::from_str(&json).unwrap();
    assert_eq!(ours.dim(), reference.len());
    let cosine: f32 = ours
        .as_slice()
        .iter()
        .zip(&reference)
        .map(|(a, b)| a * b)
        .sum();
    println!("voiceprint cosine to sherpa-onnx: {cosine:.6}");
    assert!(cosine >= 0.9999, "cosine {cosine}");
}
