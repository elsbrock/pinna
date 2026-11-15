//! Run a WAV through the detector; print detections, per-block timing and the
//! real-time factor.
//!
//!     wakebench MODEL_DIR FILE.wav [keyword.onnx]

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use pinna_wakeword::{BLOCK_SAMPLES, Detector, WakeConfig};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    ensure!(
        args.len() >= 3,
        "usage: wakebench MODEL_DIR FILE.wav [keyword.onnx]"
    );
    let keyword = args.get(3).map_or("alexa_v0.1.onnx", String::as_str);

    let mut reader = hound::WavReader::open(&args[2]).context("opening wav")?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1 && spec.sample_rate == 16000 && spec.bits_per_sample == 16,
        "need 16 kHz mono 16-bit"
    );
    let samples: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;

    let t = Instant::now();
    let mut det = Detector::load(Path::new(&args[1]), keyword, WakeConfig::default())?;
    println!("load+warmup: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);

    let (mut total, mut worst, mut blocks) = (0f64, 0f64, 0u32);
    let mut detections = Vec::new();
    for chunk in samples.chunks_exact(BLOCK_SAMPLES) {
        let t = Instant::now();
        detections.extend(det.push(chunk));
        let us = t.elapsed().as_secs_f64() * 1e6;
        total += us;
        worst = worst.max(us);
        blocks += 1;
    }
    for d in &detections {
        println!(
            "detection: score={:.4} at {:.2} s (sample {})",
            d.score,
            d.sample_index as f64 / 16000.0,
            d.sample_index
        );
    }
    let mean = total / f64::from(blocks.max(1));
    println!("detections: {}", detections.len());
    println!(
        "{blocks} blocks: mean {mean:.0} us/block, worst {worst:.0} us/block, realtime factor {:.3} (80 ms/block)",
        mean / 80_000.0
    );
    Ok(())
}
