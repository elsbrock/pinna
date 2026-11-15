//! Prints beamformer throughput on the host: 4 mics x 12 beams, real time = 16000 frames/s.
//! Run with `cargo run --release -p pinna-audio --example beamformer_bench`.
use pinna_audio::beamformer::Beamformer;
use pinna_audio::dsp::LOGICAL_CHANNELS;
use std::time::Instant;

fn main() {
    let mut bf = Beamformer::default_echo_dot();
    let seconds = 20usize;
    let mut x = 12345u32;
    let frames: Vec<[i16; LOGICAL_CHANNELS]> = (0..16_000 * seconds)
        .map(|_| {
            std::array::from_fn(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 20) as i16 - 2048
            })
        })
        .collect();
    let mut out = Vec::with_capacity(frames.len());
    let t = Instant::now();
    bf.process(&frames, &mut out);
    let dt = t.elapsed().as_secs_f64();
    let fps = frames.len() as f64 / dt;
    println!(
        "{} frames in {dt:.3} s: {fps:.0} frames/s ({:.1}x real time)",
        frames.len(),
        fps / 16000.0
    );
}
