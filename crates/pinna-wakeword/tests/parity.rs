//! tract vs onnxruntime: runs `tests/parity/ref_scores.py` (via `uv`) on each
//! fixture and compares per-block mel, embedding and score tensors.

mod common;

use std::path::Path;
use std::process::Command;

use common::*;
use pinna_wakeword::{BLOCK_SAMPLES, Detector, WakeConfig};

const WAVS: &[&str] = &[
    "alexa-twice.wav",
    "alexa-what-time.wav",
    "no-wake-word.wav",
    "music-clean.wav",
    "music-+06.wav",
    "music--12.wav",
    "music-bedonly.wav",
];

fn max_diff(a: &[f32], b: &serde_json::Value) -> f32 {
    let b = b.as_array().unwrap();
    assert_eq!(a.len(), b.len(), "tensor length");
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y.as_f64().unwrap() as f32).abs())
        .fold(0.0, f32::max)
}

#[test]
fn tract_matches_onnxruntime() {
    let Some(dir) = model_dir() else { return };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/ref_scores.py");
    let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let (mut worst_mel, mut worst_emb, mut worst_score) = (0f32, 0f32, 0f32);

    for name in WAVS {
        let wav = fixture(name);
        let json = out_dir.join(format!("ref-{name}.json"));
        let status = Command::new("uv")
            .args([
                "run",
                "--quiet",
                "--with",
                "onnxruntime",
                "--with",
                "numpy",
                "--with",
                "soundfile",
            ])
            .arg(&script)
            .arg(&dir)
            .arg(KEYWORD)
            .arg(&wav)
            .arg(&json)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => panic!("ref_scores.py failed: {s}"),
            Err(e) => {
                eprintln!("SKIP: cannot run uv ({e})");
                return;
            }
        }
        let reference: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        let blocks = reference["blocks"].as_array().unwrap();

        let samples = read_wav(&wav);
        let mut det = Detector::load(&dir, KEYWORD, WakeConfig::default()).unwrap();
        let (mut mel_d, mut emb_d, mut score_d) = (0f32, 0f32, 0f32);
        let mut n = 0;
        for (chunk, r) in samples.chunks_exact(BLOCK_SAMPLES).zip(blocks) {
            let mut score = f32::NAN;
            det.push_with(chunk, |b| score = b.score);
            mel_d = mel_d.max(max_diff(det.last_mel(), &r["mel"]));
            emb_d = emb_d.max(max_diff(det.last_embedding(), &r["embedding"]));
            score_d = score_d.max((score - r["score"].as_f64().unwrap() as f32).abs());
            n += 1;
        }
        assert_eq!(n, blocks.len());
        println!(
            "{name:22} blocks={n:4} max|mel|={mel_d:.2e} max|emb|={emb_d:.2e} max|score|={score_d:.2e}"
        );
        worst_mel = worst_mel.max(mel_d);
        worst_emb = worst_emb.max(emb_d);
        worst_score = worst_score.max(score_d);
    }
    println!("worst: mel={worst_mel:.2e} embedding={worst_emb:.2e} score={worst_score:.2e}");
    assert!(
        worst_score <= 1e-3,
        "score diverges (mel={worst_mel:e}, embedding={worst_emb:e}, score={worst_score:e})"
    );
}
