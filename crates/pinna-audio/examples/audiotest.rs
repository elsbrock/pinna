//! Audio smoke test.
//!
//!   audiotest [--backend pipewire|echodot] [--volume N] record <secs> out.wav
//!   audiotest [--backend pipewire|echodot] [--volume N] play <file>
//!   audiotest [--backend pipewire|echodot] [--volume N] tone
//!
//! `--backend wav:<file>` is accepted for `record` (reads the WAV instead of a
//! microphone). `PINNA_PW_TARGET` selects a PipeWire node.

use pinna_audio::capture::CaptureConfig;
use pinna_audio::speaker::SpeakerConfig;
use pinna_audio::{Backend, CaptureStatus, Speaker, start_capture, tone};
use std::time::Duration;

fn usage() -> ! {
    eprintln!(
        "usage: audiotest [--backend pipewire|echodot|wav:<file>] [--volume N] \
         (record <secs> <out.wav> | play <file> | tone)"
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut backend = Backend::PipeWire;
    let mut volume = 50u8;
    while args.first().is_some_and(|a| a.starts_with("--")) {
        let flag = args.remove(0);
        if args.is_empty() {
            usage();
        }
        let val = args.remove(0);
        match flag.as_str() {
            "--backend" => {
                backend = match val.as_str() {
                    "pipewire" => Backend::PipeWire,
                    "echodot" => Backend::EchoDot,
                    v if v.starts_with("wav:") => Backend::WavFile(v[4..].into()),
                    _ => usage(),
                }
            }
            "--volume" => volume = val.parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    let target = std::env::var("PINNA_PW_TARGET").ok();

    match args.first().map(String::as_str) {
        Some("record") if args.len() == 3 => {
            let secs: f32 = args[1].parse().unwrap_or_else(|_| usage());
            record(backend, target, secs, &args[2]).await;
        }
        Some("play") if args.len() == 2 => {
            let bytes = std::fs::read(&args[1]).expect("read file");
            let speaker = speaker(backend, target, volume);
            if let Err(e) = speaker.play_bytes(&bytes).await {
                eprintln!("play failed: {e}");
                std::process::exit(1);
            }
        }
        Some("tone") => {
            let speaker = speaker(backend, target, volume);
            for (name, pcm) in [
                ("wake chime", tone::wake_chime()),
                ("error tone", tone::error_tone()),
                ("timer alarm", tone::timer_alarm()),
            ] {
                eprintln!("{name}");
                if let Err(e) = speaker.play_pcm(pcm).await {
                    eprintln!("play failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => usage(),
    }
}

fn speaker(backend: Backend, target: Option<String>, volume: u8) -> Speaker {
    let mut cfg = SpeakerConfig::new(backend);
    cfg.pw_target = target;
    cfg.initial_volume = volume;
    Speaker::new(cfg)
}

async fn record(backend: Backend, target: Option<String>, secs: f32, out: &str) {
    let mut cfg = CaptureConfig::new(backend);
    cfg.pw_target = target;
    cfg.wav_realtime = false;
    let mut stream = start_capture(cfg).await.expect("start capture");
    let mut status = stream.status();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(out, spec).expect("create wav");
    let want = (secs * 50.0).round() as usize;
    let (mut n, mut peak) = (0usize, 0i32);
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f32(secs + 5.0);
    while n < want {
        tokio::select! {
            f = stream.recv() => match f {
                Some(f) => {
                    for s in f {
                        peak = peak.max(i32::from(s).abs());
                        w.write_sample(s).unwrap();
                    }
                    n += 1;
                }
                None => break,
            },
            _ = status.changed() => {
                if let CaptureStatus::Restarting { attempt, reason } = &*status.borrow() {
                    eprintln!("capture died (attempt {attempt}): {reason}");
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                eprintln!("timed out waiting for audio");
                break;
            }
        }
    }
    stream.stop();
    w.finalize().unwrap();
    eprintln!(
        "recorded {n} frames ({:.2} s), peak {peak} -> {out}",
        n as f32 / 50.0
    );
}
