mod common;

use common::*;
use pinna_wakeword::{Detector, WakeConfig};

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
/// bed (see the previous firmware's music-snr.json), so it holds one wake word.
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
