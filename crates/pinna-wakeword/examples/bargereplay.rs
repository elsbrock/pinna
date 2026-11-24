//! Replays the Echo Dot's microphone path on recordings, for barge-in
//! measurements: the beamformer as PipeWire runs it, and the stream after the
//! echo canceller through pinnad's automatic gain and the wake word detector.
//! The echo canceller itself is `tools/aec-replay` (WebRTC, C++).
//!
//!     bargereplay beam MICS.wav MICCAL.txt OUT.wav [--pad N]
//!     bargereplay wake MICS.wav MICCAL.txt OUT.wav
//!     bargereplay detect MODEL_DIR AEC.wav [--cues CUES.txt] [--sensitivity N]
//!                 [--playback] [--blocks OUT.csv]
//!
//! `beam` reads the 9 raw lanes (dot_mics, float WAV from pw-record) and writes
//! what dot_beam carries: calibration, the 4-microphone superdirective
//! beamformer with 12 beams, the high-pass and +12.04 dB, sample for sample as
//! `pinna_beam` computes them (crates/pinna-ladspa). The beamformer works in
//! 128-sample hops whose phase on the device the recording doesn't show;
//! `--pad N` starts N samples of silence earlier to try another phase.
//!
//! `wake` writes what the wake word's stream carries (ADR 0033): the mean of
//! all calibrated microphones minus the linear echo canceller's estimate from
//! the loopback lane the speaker plays (lane 8, from the Echo Dot (2nd gen)'s
//! board), then the high-pass and +12.04 dB, as `pinna_wake` computes them.
//!
//! `detect` reads dot_mic_aec (float WAV), converts it to 16-bit as pinnad's
//! pw-record does, and runs pinnad's capture loop on it: 20 ms frames, the
//! automatic gain from +12 dB, then the detector with Alexa and Hey Mycroft.
//! For each cue it prints the peak score within 3 s and whether the detector
//! fired. `--playback` applies the detector's playback rule (one supporting
//! score instead of two).

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use pinna_audio::Agc;
use pinna_audio::beamformer::Beamformer;
use pinna_audio::dsp::{
    Calibration, DIGITAL_GAIN_DEFAULT, FRAME_SAMPLES, HighPass, LOGICAL_CHANNELS, MIC_MASK_BEAM,
    apply_digital_gain, saturate_s16,
};
use pinna_audio::echocancel::LinearEchoCanceller;
use pinna_wakeword::{BlockScore, Detector, Keyword, WakeConfig};

const RATE: u32 = 16_000;

fn read_f32(path: &str) -> Result<(u16, Vec<f32>)> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("opening {path}"))?;
    let spec = r.spec();
    ensure!(
        spec.sample_rate == RATE
            && spec.sample_format == hound::SampleFormat::Float
            && spec.bits_per_sample == 32,
        "{path}: need 16 kHz 32-bit float"
    );
    // pw-record may stop mid-frame; keep the complete ones.
    let mut s = Vec::new();
    for x in r.samples::<f32>() {
        match x {
            Ok(v) => s.push(v),
            Err(_) => break,
        }
    }
    s.truncate(s.len() / usize::from(spec.channels) * usize::from(spec.channels));
    Ok((spec.channels, s))
}

fn write_f32(path: &str, s: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).with_context(|| format!("creating {path}"))?;
    for &x in s {
        w.write_sample(x)?;
    }
    w.finalize()?;
    Ok(())
}

/// As `pinna_beam` converts a float input sample.
fn to_i16(x: f32) -> i16 {
    saturate_s16((x * 32768.0).round() as i64)
}

fn beam(mics: &str, miccal: &str, out: &str, pad: usize) -> Result<()> {
    let (channels, mut s) = read_f32(mics)?;
    ensure!(channels == 9, "{mics}: need the 9 lanes of dot_mics");
    s.splice(0..0, std::iter::repeat_n(0.0, pad * 9));
    let q14: Vec<i32> = std::fs::read_to_string(miccal)
        .with_context(|| format!("reading {miccal}"))?
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .context("miccal: need 7 integers")?;
    let q14: [i32; LOGICAL_CHANNELS] = q14
        .try_into()
        .map_err(|_| anyhow::anyhow!("miccal: need 7 integers"))?;
    let cal = Calibration::new(q14);
    // pinna_beam's activate() builds the beamformer from the graph's controls:
    // mask 27, 12 beams (10-echo-dot.conf).
    let mut bf = Beamformer::new(MIC_MASK_BEAM, 12).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut hp = HighPass::new();
    let mut y = Vec::with_capacity(s.len() / 9);
    for lanes in s.chunks_exact(9) {
        let mut logical = [0i16; LOGICAL_CHANNELS];
        for (c, l) in logical.iter_mut().enumerate() {
            *l = cal.apply(c, to_i16(lanes[c]));
        }
        let mut v = [hp.sample(bf.sample(&logical))];
        apply_digital_gain(&mut v, DIGITAL_GAIN_DEFAULT);
        y.push(f32::from(v[0]) / 32768.0);
    }
    write_f32(out, &y[pad..])
}

fn read_miccal(miccal: &str) -> Result<Calibration> {
    let q14: Vec<i32> = std::fs::read_to_string(miccal)
        .with_context(|| format!("reading {miccal}"))?
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .context("miccal: need 7 integers")?;
    let q14: [i32; LOGICAL_CHANNELS] = q14
        .try_into()
        .map_err(|_| anyhow::anyhow!("miccal: need 7 integers"))?;
    Ok(Calibration::new(q14))
}

fn wake(mics: &str, miccal: &str, out: &str) -> Result<()> {
    let board = &pinna_board::ECHO_DOT_2;
    let reference = board
        .speaker_lane()
        .context("the board names no speaker lane")?;
    let (channels, s) = read_f32(mics)?;
    ensure!(channels == 9, "{mics}: need the 9 lanes of dot_mics");
    let cal = read_miccal(miccal)?;
    let lanes: Vec<usize> = (0..LOGICAL_CHANNELS)
        .filter(|c| board.mics.all() & (1 << c) != 0)
        .collect();
    let mut aec = LinearEchoCanceller::new();
    let mut hp = HighPass::new();
    let mut y = Vec::with_capacity(s.len() / 9);
    for f in s.chunks_exact(9) {
        let mean = lanes
            .iter()
            .map(|&c| f32::from(cal.apply(c, to_i16(f[c]))))
            .sum::<f32>()
            / lanes.len() as f32;
        let e = aec.process(mean, f32::from(to_i16(f[reference])));
        let mut v = [hp.sample(saturate_s16(e.round() as i64))];
        apply_digital_gain(&mut v, DIGITAL_GAIN_DEFAULT);
        y.push(f32::from(v[0]) / 32768.0);
    }
    write_f32(out, &y)
}

struct Options {
    cues: Vec<f64>,
    sensitivity: u8,
    playback: bool,
    blocks: Option<String>,
}

fn detect(models: &str, aec: &str, o: &Options) -> Result<()> {
    let (channels, s) = read_f32(aec)?;
    ensure!(channels == 1, "{aec}: need mono");
    let dir = Path::new(models);
    let keywords = ["alexa_v0.1.onnx", "hey_mycroft_v0.1.onnx"]
        .iter()
        .map(|k| Keyword::load(&dir.join(k)))
        .collect::<Result<Vec<_>>>()?;
    let threshold = WakeConfig::threshold_for_sensitivity(o.sensitivity);
    let mut det = Detector::new(
        dir,
        keywords,
        WakeConfig {
            threshold,
            during_playback: o.playback,
            ..Default::default()
        },
    )?;
    // pinnad's pw-record asks PipeWire for s16.
    let pcm: Vec<i16> = s.iter().map(|&x| to_i16(x)).collect();
    let mut agc = Agc::new(12.0);
    let mut blocks: Vec<(BlockScore, f32, f64)> = Vec::new();
    let mut detections = Vec::new();
    let mut pre = 0.0f64;
    for chunk in pcm.chunks_exact(FRAME_SAMPLES) {
        let mut frame = [0i16; FRAME_SAMPLES];
        frame.copy_from_slice(chunk);
        pre += pinna_audio::agc::mean_square(&frame);
        agc.process(&mut frame);
        let g = agc.gain_db();
        let mut seen = Vec::new();
        detections.extend(det.push_with(&frame, |b| seen.push(*b)));
        for b in seen {
            // Mean level before the gain over the block's four frames.
            blocks.push((b, g, pre / 4.0));
            pre = 0.0;
        }
    }
    if let Some(path) = &o.blocks {
        let mut csv = String::from("t,score,keyword,vad,gain_db,pre_gain_dbfs\n");
        for (b, g, ms) in &blocks {
            csv += &format!(
                "{:.3},{:.4},{},{},{:.1},{:.1}\n",
                b.sample_index as f64 / f64::from(RATE),
                b.score,
                b.keyword.map_or(-1, |k| k as i64),
                u8::from(b.vad_active),
                g,
                pinna_audio::agc::mean_square_dbfs(*ms),
            );
        }
        std::fs::write(path, csv)?;
    }
    println!("threshold {threshold:.4} (sensitivity {})", o.sensitivity);
    let at = |i: u64| i as f64 / f64::from(RATE);
    for d in &detections {
        println!(
            "detection: {} score {:.3} at {:.2} s",
            det.keyword(d.keyword).map_or("?", |k| k.name()),
            d.score,
            at(d.sample_index)
        );
    }
    for (n, &c) in o.cues.iter().enumerate() {
        let window = blocks
            .iter()
            .filter(|(b, ..)| (c - 0.5..c + 3.0).contains(&at(b.sample_index)));
        let best = window.max_by(|a, b| a.0.score.total_cmp(&b.0.score));
        let fired = detections
            .iter()
            .any(|d| (c - 0.5..c + 3.0).contains(&at(d.sample_index)));
        match best {
            Some((b, g, _)) => println!(
                "cue {} at {c:.2} s: peak {:.3} at {:.2} s, gain {g:+.1} dB, {}",
                n + 1,
                b.score,
                at(b.sample_index),
                if fired {
                    "detected"
                } else if b.score >= 0.2 {
                    "near miss"
                } else {
                    "nothing"
                }
            ),
            None => println!("cue {} at {c:.2} s: past the end", n + 1),
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("beam") if args.len() == 4 => beam(&args[1], &args[2], &args[3], 0),
        Some("wake") if args.len() == 4 => wake(&args[1], &args[2], &args[3]),
        Some("beam") if args.len() == 6 && args[4] == "--pad" => {
            beam(&args[1], &args[2], &args[3], args[5].parse()?)
        }
        Some("detect") if args.len() >= 3 => {
            let mut o = Options {
                cues: Vec::new(),
                sensitivity: 68,
                playback: false,
                blocks: None,
            };
            let mut rest = args[3..].iter();
            while let Some(a) = rest.next() {
                match a.as_str() {
                    "--cues" => {
                        let p = rest.next().context("--cues needs a file")?;
                        o.cues = std::fs::read_to_string(p)?
                            .split_whitespace()
                            .map(str::parse)
                            .collect::<Result<_, _>>()?;
                    }
                    "--sensitivity" => {
                        o.sensitivity = rest.next().context("--sensitivity needs N")?.parse()?;
                    }
                    "--playback" => o.playback = true,
                    "--blocks" => {
                        o.blocks = Some(rest.next().context("--blocks needs a file")?.clone())
                    }
                    other => bail!("unknown option {other}"),
                }
            }
            detect(&args[1], &args[2], &o)
        }
        _ => bail!(
            "usage: bargereplay beam MICS.wav MICCAL.txt OUT.wav [--pad N]\n       bargereplay detect MODEL_DIR AEC.wav [--cues CUES.txt] [--sensitivity N] [--playback] [--blocks OUT.csv]"
        ),
    }
}
