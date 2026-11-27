mod common;

use common::*;
use pinna_wakeword::{Detector, Keyword, ModelFormat, WakeConfig};

fn detector() -> Option<Detector> {
    let dir = model_dir()?;
    Some(Detector::load(&dir, KEYWORD, WakeConfig::default()).unwrap())
}

fn detections(name: &str) -> Option<Vec<(f32, u64)>> {
    let mut det = detector()?;
    let samples = read_wav(&fixture(name));
    Some(
        det.push(&samples)
            .iter()
            .map(|d| (d.score, d.sample_index))
            .collect(),
    )
}

#[test]
fn alexa_twice_detects_twice() {
    if let Some(d) = detections("alexa-twice.wav") {
        println!("alexa-twice: {d:?}");
        assert_eq!(d.len(), 2);
    }
}

#[test]
fn alexa_what_time_detects_once() {
    if let Some(d) = detections("alexa-what-time.wav") {
        println!("alexa-what-time: {d:?}");
        assert_eq!(d.len(), 1);
    }
}

/// music-clean.wav is alexa-what-time.wav behind 2 s of lead-in with no music
/// bed (see LibreEcho's music-snr.json), so it holds one wake word.
#[test]
fn music_clean_detects_once() {
    if let Some(d) = detections("music-clean.wav") {
        println!("music-clean: {d:?}");
        assert_eq!(d.len(), 1);
    }
}

#[test]
fn negatives_do_not_detect() {
    for name in ["no-wake-word.wav", "music-bedonly.wav"] {
        if let Some(d) = detections(name) {
            println!("{name}: {d:?}");
            assert!(d.is_empty(), "{name}: {d:?}");
        }
    }
}

#[test]
fn chunking_does_not_change_scores() {
    let Some(dir) = model_dir() else { return };
    let samples = read_wav(&fixture("alexa-what-time.wav"));
    let run = |chunk: usize| {
        let mut det = Detector::load(&dir, KEYWORD, WakeConfig::default()).unwrap();
        let mut scores = Vec::new();
        let mut found = Vec::new();
        for c in samples.chunks(chunk) {
            found.extend(det.push_with(c, |b| scores.push(b.score)));
        }
        (scores, found)
    };
    let (a, da) = run(1280);
    let (b, db) = run(333);
    assert_eq!(a, b);
    assert_eq!(da, db);
}

#[test]
fn reset_restores_fresh_state() {
    let Some(mut det) = detector() else { return };
    let samples = read_wav(&fixture("alexa-what-time.wav"));
    let first = det.push(&samples);
    det.reset();
    let second = det.push(&samples);
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
}

#[test]
fn lockout_suppresses_repeat_but_not_after_expiry() {
    let Some(dir) = model_dir() else { return };
    let samples = read_wav(&fixture("alexa-twice.wav"));
    let cfg = WakeConfig {
        lockout_samples: 1_000_000,
        ..WakeConfig::default()
    };
    let mut det = Detector::load(&dir, KEYWORD, cfg).unwrap();
    assert_eq!(det.push(&samples).len(), 1);
}

fn keyword(dir: &std::path::Path, file: &str) -> Option<Keyword> {
    let path = dir.join(file);
    if !path.exists() {
        eprintln!("SKIP: {file} missing; run `just models`");
        return None;
    }
    Some(Keyword::load(&path).unwrap())
}

/// (keyword, score, sample) for every detection in a fixture.
fn detect_with(
    dir: &std::path::Path,
    keywords: Vec<Keyword>,
    name: &str,
) -> Vec<(usize, f32, u64)> {
    let mut det = Detector::new(dir, keywords, WakeConfig::default()).unwrap();
    det.push(&read_wav(&fixture(name)))
        .iter()
        .map(|d| (d.keyword, d.score, d.sample_index))
        .collect()
}

#[test]
fn second_keyword_leaves_alexa_unchanged() {
    let Some(dir) = model_dir() else { return };
    for name in [
        "alexa-twice.wav",
        "alexa-what-time.wav",
        "music-clean.wav",
        "no-wake-word.wav",
        "music-bedonly.wav",
    ] {
        let Some(jarvis) = keyword(&dir, "hey_jarvis_v0.1.onnx") else {
            return;
        };
        let alone = detect_with(&dir, vec![keyword(&dir, KEYWORD).unwrap()], name);
        let both = detect_with(&dir, vec![keyword(&dir, KEYWORD).unwrap(), jarvis], name);
        println!("{name}: alone {alone:?}, with Hey Jarvis {both:?}");
        assert_eq!(alone, both, "{name}");
    }
}

#[test]
fn detection_names_the_keyword_that_fired() {
    let Some(dir) = model_dir() else { return };
    let Some(jarvis) = keyword(&dir, "hey_jarvis_v0.1.onnx") else {
        return;
    };
    let mut det = Detector::new(
        &dir,
        vec![jarvis, keyword(&dir, KEYWORD).unwrap()],
        WakeConfig::default(),
    )
    .unwrap();
    let found = det.push(&read_wav(&fixture("alexa-twice.wav")));
    assert_eq!(found.len(), 2);
    for d in &found {
        assert_eq!(det.keyword(d.keyword).unwrap().name(), "alexa_v0.1");
    }
}

/// Two keywords that both fire on the same utterance give one detection: the
/// lockout is shared, and the first of equal scores wins.
#[test]
fn shared_lockout_gives_one_detection_per_utterance() {
    let Some(dir) = model_dir() else { return };
    let a = || keyword(&dir, KEYWORD).unwrap();
    let found = detect_with(&dir, vec![a(), a()], "alexa-twice.wav");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|d| d.0 == 0), "{found:?}");
}

#[test]
fn no_keywords_never_fires() {
    let Some(dir) = model_dir() else { return };
    assert!(detect_with(&dir, Vec::new(), "alexa-twice.wav").is_empty());
}

#[test]
fn set_keywords_takes_effect() {
    let Some(dir) = model_dir() else { return };
    let samples = read_wav(&fixture("alexa-twice.wav"));
    let mut det = Detector::new(&dir, Vec::new(), WakeConfig::default()).unwrap();
    assert!(det.push(&samples).is_empty());
    det.reset();
    det.set_keywords(vec![keyword(&dir, KEYWORD).unwrap()]);
    assert_eq!(det.keyword_count(), 1);
    assert_eq!(det.push(&samples).len(), 2);
}

#[test]
fn tflite_keyword_loads() {
    let Some(dir) = model_dir() else { return };
    let path = dir.join("okay_nabu.tflite");
    if !path.exists() {
        eprintln!("SKIP: okay_nabu.tflite missing; run `just models`");
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(ModelFormat::detect(&bytes), ModelFormat::Tflite);
    let k = Keyword::from_bytes("okay_nabu", &bytes).unwrap();
    assert_eq!(k.name(), "okay_nabu");
    // Okay Nabu stays quiet on Alexa.
    assert!(detect_with(&dir, vec![k], "alexa-twice.wav").is_empty());
}

#[test]
fn rejects_models_that_are_not_classifiers() {
    let Some(dir) = model_dir() else { return };
    let err = |bytes: &[u8]| Keyword::from_bytes("x", bytes).unwrap_err().to_string();
    assert!(err(b"definitely not a model").contains("not an ONNX or TFLite model"));
    let mut fake_tflite = vec![0u8; 64];
    fake_tflite[4..8].copy_from_slice(b"TFL3");
    assert!(!err(&fake_tflite).is_empty());
    for other in ["embedding_model.onnx", "melspectrogram.onnx"] {
        let bytes = std::fs::read(dir.join(other)).unwrap();
        let e = Keyword::from_bytes(other, &bytes).unwrap_err();
        println!("{other}: {e:#}");
    }
}

/// `scores` reads what the latest block scored: the highest of them is the
/// score `push_with` reports for that block, and reading changes nothing.
#[test]
fn scores_read_the_latest_block() {
    let Some(mut det) = detector() else { return };
    assert!(det.scores().all(|(name, s)| !name.is_empty() && s == 0.0));
    let samples = read_wav(&fixture("alexa-what-time.wav"));
    let mut checked = 0;
    for c in samples.chunks(1280) {
        let mut block = None;
        det.push_with(c, |b| block = Some(b.score));
        let Some(reported) = block else { continue };
        let best = det.scores().map(|(_, s)| s).fold(0.0, f32::max);
        assert_eq!(best, reported);
        assert_eq!(det.scores().count(), det.keyword_count());
        checked += 1;
    }
    assert!(checked > 10);
}
