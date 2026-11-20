//! Offline bench for the microphone front end of the MT8163 Echos (ADR 0004,
//! ADR 0033). It replays nine-lane recordings through several front ends and
//! echo cancellers, and measures what decides: the talker over the noise and
//! the residual echo in three bands, the echo removed, clipping, CPU, and the
//! wake word detector's hits and false accepts. Modelled on TECHO5's bench on
//! the same hardware (techo5-dot `tools/micbench`, MIT).
//!
//! On your computer, from the repository root:
//!
//! ```text
//! micbench run [--models DIR] [--aec-replay PATH] [--word alexa]
//!              [--never hey_mycroft,hey_jarvis] [--ref 7|8|78] [--cold]
//!              [--jobs N] [--no-wake] [--csv OUT.csv] TAKE.wav...
//! micbench synth BASE.wav OUT.wav --talker SPEECH.wav [--level DBFS]
//!              [--az DEG] [--el DEG] [--first S] [--every S] [--count N]
//!              [--interferer SPEECH.wav --interferer-level DBFS --interferer-az DEG]
//! ```
//!
//! A take is a 16 kHz WAV whose first nine channels are `dot_mics`' lanes:
//! pw-record's file or the dashboard's debugging recording (ADR 0030).
//! `TAKE.cues` lists the talker's utterances, one `start end` in seconds per
//! line; `synth` writes it. Without one, a take without playback finds its
//! talker by level on the centre microphone, and nothing is known about what
//! was said. The loopback lanes come in the microphones' frames, so the
//! reference needs no alignment.
//!
//! `run` takes the reference from the loopback lane the Echo Dot (2nd gen)'s
//! speaker plays, as `pinna_wake` does (lane 8), and defaults to the models in
//! `blobs/models/openwakeword-0.5.1` and to
//! `build/aec-replay/aec_replay`, built as `tools/aec-replay/aec_replay.cc`
//! says; without it the WebRTC rows are left out.

mod cancel;
mod fft;
mod frontend;
mod metrics;
mod synth;
mod wake;
mod wav;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use pinna_audio::dsp::{LOGICAL_CHANNELS, MIC_MASK_ALL, MIC_MASK_BEAM};
use pinna_audio::echocancel::STEP_SIZE;

use cancel::Cancel;
use fft::{BANDS, HOP, SPEECH, WIN, band_powers, frame_centre, frame_count};
use frontend::Front;
use metrics::{Cue, Frames};

const RATE: u32 = 16_000;
/// Detection thresholds swept: sensitivity 100 (0.41) to 23 (0.75). The
/// default sensitivity, 68, is 0.55.
const THRESHOLDS: [f32; 8] = [0.41, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75];
const DEFAULT_THRESHOLD: usize = 3;
const LOW_THRESHOLD: usize = 1;
/// A detection belongs to an utterance from this long before it starts...
const HIT_BEFORE: f64 = 0.5;
/// ...until this long after it ends.
const HIT_AFTER: f64 = 2.0;

struct Options {
    models: PathBuf,
    aec_replay: Option<PathBuf>,
    word: String,
    never: Vec<String>,
    /// The loopback lanes the reference takes: "7", "8" or "78" (their mean).
    /// The default is the one the board's speaker plays.
    reference: String,
    warm: bool,
    jobs: usize,
    wake: bool,
    csv: Option<PathBuf>,
    takes: Vec<PathBuf>,
}

struct Take {
    name: String,
    lanes: Vec<[f32; 9]>,
    cues: Vec<Cue>,
    /// The cues come from a file: what was said, and when, is known.
    scripted: bool,
}

#[derive(Default)]
struct WakeResult {
    hits: [Vec<usize>; 2],
    false_accepts: [Vec<usize>; 2],
    /// Highest target score per utterance.
    peaks: Vec<f32>,
    /// Highest score of a word nobody said, and its detections at the
    /// default threshold.
    never_peak: f32,
    never_detections: usize,
    /// The words nobody said that were detected.
    never_words: Vec<String>,
    /// All target detections at the default threshold, without the relief.
    detections: usize,
}

struct Row {
    front: String,
    cancel: String,
    per_turn: bool,
    snr: [Option<f64>; 4],
    erle: Option<(f64, f64)>,
    residual_db: Option<f64>,
    gain_clips: usize,
    cpu_pct: f64,
    wake: Option<WakeResult>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(parse_run(&args[1..])?),
        Some("synth") => synth_take(&args[1..]),
        _ => bail!(
            "usage: micbench run [options] TAKE.wav...\n       micbench synth BASE.wav OUT.wav --talker SPEECH.wav [options]\n(see the doc comment in crates/pinna-wakeword/examples/micbench/main.rs)"
        ),
    }
}

fn parse_run(args: &[String]) -> Result<Options> {
    let default_aec = PathBuf::from("build/aec-replay/aec_replay");
    let mut o = Options {
        models: PathBuf::from("blobs/models/openwakeword-0.5.1"),
        aec_replay: default_aec.exists().then_some(default_aec),
        word: "alexa".into(),
        never: vec![
            "hey_mycroft".into(),
            "hey_jarvis".into(),
            "hey_rhasspy".into(),
        ],
        // The lane the board's speaker plays, as pinna_wake takes it.
        reference: pinna_board::ECHO_DOT_2
            .speaker_lane()
            .map_or("78".into(), |l| l.to_string()),
        warm: true,
        jobs: std::thread::available_parallelism().map_or(4, |n| n.get()),
        wake: true,
        csv: None,
        takes: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--models" => o.models = value()?.into(),
            "--aec-replay" => o.aec_replay = Some(value()?.into()),
            "--word" => o.word = value()?.clone(),
            "--never" => o.never = value()?.split(',').map(str::to_owned).collect(),
            "--ref" => {
                o.reference = value()?.clone();
                ensure!(
                    ["7", "8", "78"].contains(&o.reference.as_str()),
                    "--ref takes 7, 8 or 78"
                );
            }
            "--cold" => o.warm = false,
            "--jobs" => o.jobs = value()?.parse::<usize>()?.max(1),
            "--no-wake" => o.wake = false,
            "--csv" => o.csv = Some(value()?.into()),
            f if f.starts_with("--") => bail!("unknown option {f}"),
            f => o.takes.push(f.into()),
        }
    }
    ensure!(!o.takes.is_empty(), "no takes given");
    ensure!(
        o.models.join("melspectrogram.onnx").exists(),
        "no wake word models in {} (just models)",
        o.models.display()
    );
    Ok(o)
}

fn read_cues(path: &Path) -> Result<Option<Vec<Cue>>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let mut cues = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let v: Vec<f64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .with_context(|| format!("{}: {line:?}", path.display()))?;
        match v[..] {
            [s] => cues.push((s, s + 1.0)),
            [s, e] => cues.push((s, e)),
            _ => bail!("{}: want `start [end]`, got {line:?}", path.display()),
        }
    }
    Ok(Some(cues))
}

fn load(path: &Path) -> Result<Take> {
    let w = wav::read(path)?;
    ensure!(
        w.rate == RATE,
        "{}: {} Hz, need 16 kHz",
        path.display(),
        w.rate
    );
    ensure!(
        w.channels >= 9,
        "{}: {} channels, need dot_mics' nine lanes",
        path.display(),
        w.channels
    );
    let lanes: Vec<[f32; 9]> = w
        .samples
        .chunks_exact(w.channels)
        .map(|f| std::array::from_fn(|c| f[c]))
        .collect();
    let cues = read_cues(&path.with_extension("cues"))?;
    let name = path.file_stem().map_or_else(
        || path.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let name = match path.parent().and_then(Path::file_name) {
        Some(dir) => format!("{}/{name}", dir.to_string_lossy()),
        None => name,
    };
    Ok(Take {
        name,
        lanes,
        scripted: cues.is_some(),
        cues: cues.unwrap_or_default(),
    })
}

/// Utterances on the centre microphone of a take without playback: frames
/// 10 dB over the quietest fifth, joined across gaps under 0.4 s, at least
/// 0.3 s long.
fn find_talker(centre: &[f64]) -> Vec<Cue> {
    let mut sorted = centre.to_vec();
    sorted.sort_by(f64::total_cmp);
    let floor = sorted.get(sorted.len() / 5).copied().unwrap_or(0.0);
    let on: Vec<bool> = centre.iter().map(|&p| p > floor * 10.0).collect();
    let mut cues: Vec<(usize, usize)> = Vec::new();
    for (k, &a) in on.iter().enumerate() {
        if !a {
            continue;
        }
        match cues.last_mut() {
            Some((_, e)) if k - *e <= 20 => *e = k,
            _ => cues.push((k, k)),
        }
    }
    cues.into_iter()
        .filter(|(s, e)| e - s >= 15)
        .map(|(s, e)| (frame_centre(s) - 0.1, frame_centre(e) + 0.1))
        .collect()
}

/// Mean square per analysis frame.
fn frame_ms(x: &[f32]) -> Vec<f64> {
    (0..frame_count(x.len()))
        .map(|k| {
            x[k * HOP..k * HOP + WIN]
                .iter()
                .map(|&v| f64::from(v).powi(2))
                .sum::<f64>()
                / WIN as f64
        })
        .collect()
}

fn scaled(x: &[f32], by: f32) -> Vec<f32> {
    x.iter().map(|v| v * by).collect()
}

fn delayed(x: &[f32], n: usize) -> Vec<f32> {
    let mut y = vec![0.0; n.min(x.len())];
    y.extend_from_slice(&x[..x.len() - n.min(x.len())]);
    y
}

fn row_wake(o: &Options, take: &Take, frames: &Frames, out: &[i16]) -> Result<WakeResult> {
    let mut keywords = vec![o.word.clone()];
    keywords.extend(o.never.iter().cloned());
    let scored = wake::score(&o.models, &keywords, out, &frames.playback)?;
    let at = |s: u64| s as f64 / f64::from(RATE);
    let in_cue = |t: f64, &(s, e): &Cue| t >= s - HIT_BEFORE && t <= e + HIT_AFTER;
    let mut r = WakeResult::default();
    for relief in [false, true] {
        let i = usize::from(relief);
        for &th in &THRESHOLDS {
            let d = wake::replay(&scored[0].blocks, th, relief);
            let hit = take
                .cues
                .iter()
                .filter(|c| d.iter().any(|x| in_cue(at(x.0), c)))
                .count();
            let fa = d
                .iter()
                .filter(|x| !take.cues.iter().any(|c| in_cue(at(x.0), c)))
                .count();
            r.hits[i].push(hit);
            r.false_accepts[i].push(fa);
        }
    }
    r.detections = wake::replay(&scored[0].blocks, THRESHOLDS[DEFAULT_THRESHOLD], false).len();
    r.peaks = take
        .cues
        .iter()
        .map(|c| {
            scored[0]
                .blocks
                .iter()
                .filter(|b| in_cue(at(b.sample_index), c))
                .map(|b| b.score)
                .fold(0.0, f32::max)
        })
        .collect();
    for (s, word) in scored[1..].iter().zip(&o.never) {
        r.never_peak = s
            .blocks
            .iter()
            .map(|b| b.score)
            .fold(r.never_peak, f32::max);
        let n = wake::replay(&s.blocks, THRESHOLDS[DEFAULT_THRESHOLD], false).len();
        r.never_detections += n;
        if n > 0 {
            r.never_words.push(format!("{word} x{n}"));
        }
    }
    Ok(r)
}

struct Prepared {
    logical: Vec<[i16; LOGICAL_CHANNELS]>,
    alive: u8,
    reference: Vec<f32>,
    frames: Frames,
}

/// One front end through one canceller. `turn` restricts the talker to one
/// utterance (a per-turn front end).
fn row(
    o: &Options,
    take: &Take,
    p: &Prepared,
    front: Front,
    cancel: Cancel,
    turn: Option<usize>,
    scratch: &Path,
) -> Result<Option<Row>> {
    let t = Instant::now();
    let x = frontend::run(front, &p.logical, p.alive)?;
    let front_cpu = t.elapsed().as_secs_f64();
    let reference = delayed(&p.reference, front.latency());
    let Some(c) = cancel::run(
        cancel,
        &x,
        &reference,
        o.warm,
        o.aec_replay.as_deref(),
        scratch,
    )?
    else {
        return Ok(None);
    };
    let unit = 1.0 / 32768.0;
    let out: Vec<f32> = c.out.iter().map(|&s| f32::from(s) * unit).collect();
    let pout = band_powers(&out);
    let f = &p.frames;
    // Frames at the front end's latency line up with the microphones' frames.
    let lag = front.latency() / HOP;
    let talk = |k: usize| {
        k >= lag
            && match turn {
                Some(i) => f.turn[k - lag] == Some(i),
                None => f.talk[k - lag],
            }
    };
    let residual = |k: usize| k >= lag && f.residual[k - lag];
    let echo: Vec<bool> = (0..pout[0].len())
        .map(|k| k >= lag && f.echo[k - lag])
        .collect();
    let snr = std::array::from_fn(|b| {
        (!take.cues.is_empty())
            .then(|| metrics::snr(&pout[b], talk, residual))
            .flatten()
    });
    let (erle, residual_db) = if f.has_playback && cancel != Cancel::None {
        let pb = band_powers(&scaled(&c.before, unit));
        let pa = band_powers(&scaled(&c.after, unit));
        (
            metrics::erle(&pb, &pa, &echo),
            metrics::level(&pa[SPEECH], &echo).map(|l| l - c.after_gain_db),
        )
    } else {
        (None, None)
    };
    let fixed = turn.is_none();
    let wake = if fixed && o.wake {
        Some(row_wake(o, take, f, &c.out)?)
    } else {
        None
    };
    let seconds = p.logical.len() as f64 / f64::from(RATE);
    Ok(Some(Row {
        front: front.name(),
        cancel: cancel.name(),
        per_turn: !fixed,
        snr,
        erle,
        residual_db,
        gain_clips: c.gain_clips,
        cpu_pct: 100.0 * (front_cpu + c.cpu) / seconds,
        wake,
    }))
}

fn run(o: Options) -> Result<()> {
    let mut csv = String::from(
        "take,front,cancel,per_turn,snr_low,snr_mid,snr_high,snr_speech,erle,erle_median,residual_dbfs,gain_clips,cpu_pct,cues",
    );
    for relief in ["", "_relief"] {
        for th in THRESHOLDS {
            let _ = write!(csv, ",hits{relief}_{th},fa{relief}_{th}");
        }
    }
    csv += ",peak_median,never_peak,never_detections,never_words\n";
    if o.aec_replay.is_none() {
        eprintln!("no aec_replay: the WebRTC rows are left out (see tools/aec-replay)");
    }
    for path in &o.takes {
        let take = load(path)?;
        let report = bench(&o, take, &mut csv)?;
        println!("{report}");
    }
    if let Some(p) = &o.csv {
        std::fs::write(p, csv).with_context(|| format!("writing {}", p.display()))?;
    }
    Ok(())
}

fn bench(o: &Options, mut take: Take, csv: &mut String) -> Result<String> {
    let logical = frontend::logical(&take.lanes);
    let alive = frontend::alive_lanes(&logical);
    let reference: Vec<f32> = take
        .lanes
        .iter()
        .map(|f| match o.reference.as_str() {
            "7" => f[7],
            "8" => f[8],
            _ => (f[7] + f[8]) / 2.0,
        })
        .collect();
    let ref_ms = frame_ms(&reference);
    let n = frame_count(take.lanes.len());
    let raw: Vec<[Vec<f64>; 4]> = (0..LOGICAL_CHANNELS)
        .map(|c| band_powers(&take.lanes.iter().map(|f| f[c]).collect::<Vec<_>>()))
        .collect();
    let mut frames = metrics::classify(n, &take.cues, &ref_ms);
    if take.cues.is_empty() && !frames.has_playback {
        take.cues = find_talker(&raw[frontend::CENTRE][SPEECH]);
        frames = metrics::classify(n, &take.cues, &ref_ms);
    }
    let p = Prepared {
        logical,
        alive,
        reference: scaled(&reference, 32768.0),
        frames,
    };

    let mut fixed = Vec::new();
    for front in [
        Front::Centre,
        Front::Mean(MIC_MASK_ALL),
        Front::Mean(MIC_MASK_BEAM),
        Front::Mean(frontend::MIC_MASK_QUIET5),
        Front::Mvdr(MIC_MASK_BEAM),
        Front::Mvdr(MIC_MASK_ALL),
    ] {
        for cancel in [
            Cancel::None,
            Cancel::Pinna(STEP_SIZE),
            Cancel::Pinna(0.5),
            Cancel::WebrtcLinear,
            Cancel::Webrtc,
        ] {
            fixed.push((front, cancel, None));
        }
    }
    // Per-turn front ends: chosen from each utterance, held for it.
    let mut turns = Vec::new();
    let in_turn = |i: usize, s: usize| {
        let k = s / HOP;
        k < p.frames.turn.len() && p.frames.turn[k] == Some(i)
    };
    let quiet = |s: usize| {
        let k = s / HOP;
        k < p.frames.residual.len() && p.frames.residual[k]
    };
    for i in 0..take.cues.len() {
        let lane = (0..LOGICAL_CHANNELS - 1)
            .filter(|&c| p.alive & (1 << c) != 0)
            .max_by(|&a, &b| {
                let r = |c: usize| {
                    metrics::snr(
                        &raw[c][SPEECH],
                        |k| p.frames.turn[k] == Some(i),
                        |k| p.frames.residual[k],
                    )
                    .unwrap_or(-99.0)
                };
                r(a).total_cmp(&r(b))
            })
            .unwrap_or(0);
        let mut fronts = vec![Front::Lane(lane)];
        for mask in [MIC_MASK_BEAM, MIC_MASK_ALL] {
            let az = frontend::best_beam(mask, &p.logical, p.alive, |s| in_turn(i, s), quiet)?;
            fronts.push(Front::MvdrAt(mask, az));
        }
        for front in fronts {
            for cancel in [Cancel::None, Cancel::Pinna(STEP_SIZE)] {
                turns.push((front, cancel, Some(i)));
            }
        }
    }
    let jobs: Vec<_> = fixed.into_iter().chain(turns).collect();

    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let workers = o.jobs;
    let tmp = std::env::temp_dir().join(format!("micbench-{}", std::process::id()));
    std::thread::scope(|s| -> Result<()> {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let (jobs, next, results, take, p, tmp) = (&jobs, &next, &results, &take, &p, &tmp);
                s.spawn(move || -> Result<()> {
                    let scratch = tmp.join(w.to_string());
                    std::fs::create_dir_all(&scratch)?;
                    loop {
                        let j = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(front, cancel, turn)) = jobs.get(j) else {
                            return Ok(());
                        };
                        if let Some(r) = row(o, take, p, front, cancel, turn, &scratch)
                            .with_context(|| {
                                format!("{} through {}", front.name(), cancel.name())
                            })?
                        {
                            results.lock().unwrap().push((j, r));
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker panicked")?;
        }
        Ok(())
    })?;
    let _ = std::fs::remove_dir_all(&tmp);
    let mut rows = results.into_inner().unwrap();
    rows.sort_by_key(|(j, _)| *j);
    let rows: Vec<Row> = merge_turns(rows.into_iter().map(|(_, r)| r).collect());
    Ok(report(&take, &p, &raw, &rows, csv))
}

/// Per-turn rows come one per utterance. Merge them into one row per front
/// end kind and canceller: the median of each measure across utterances,
/// and the choice made for each utterance in the name.
fn merge_turns(rows: Vec<Row>) -> Vec<Row> {
    fn median(mut v: Vec<f64>) -> Option<f64> {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied()
    }
    let mut out: Vec<Row> = Vec::new();
    let mut groups: Vec<(String, String, Vec<Row>)> = Vec::new();
    for r in rows {
        if !r.per_turn {
            out.push(r);
            continue;
        }
        let kind = if r.front.starts_with("lane") {
            "best mic per turn".to_owned()
        } else {
            format!("{} per turn", r.front.split(" at").next().unwrap_or(""))
        };
        match groups.iter_mut().find(|g| g.0 == kind && g.1 == r.cancel) {
            Some(g) => g.2.push(r),
            None => groups.push((kind, r.cancel.clone(), vec![r])),
        }
    }
    for (kind, _, rs) in groups {
        let choices: Vec<String> = rs
            .iter()
            .map(|r| r.front.rsplit(' ').next().unwrap_or("").to_owned())
            .collect();
        let pick = |f: &dyn Fn(&Row) -> Option<f64>| median(rs.iter().filter_map(f).collect());
        out.push(Row {
            front: format!("{kind} ({})", choices.join(",")),
            cancel: rs[0].cancel.clone(),
            per_turn: true,
            snr: std::array::from_fn(|b| pick(&|r: &Row| r.snr[b])),
            erle: pick(&|r: &Row| r.erle.map(|e| e.0)).zip(pick(&|r: &Row| r.erle.map(|e| e.1))),
            residual_db: pick(&|r: &Row| r.residual_db),
            gain_clips: rs.iter().map(|r| r.gain_clips).max().unwrap_or(0),
            cpu_pct: pick(&|r: &Row| Some(r.cpu_pct)).unwrap_or(0.0),
            wake: None,
        });
    }
    out
}

fn fmt(v: Option<f64>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{v:.1}"))
}

fn report(
    take: &Take,
    p: &Prepared,
    raw: &[[Vec<f64>; 4]],
    rows: &[Row],
    csv: &mut String,
) -> String {
    let f = &p.frames;
    let seconds = take.lanes.len() as f64 / f64::from(RATE);
    let dead: Vec<String> = (0..LOGICAL_CHANNELS)
        .filter(|&c| p.alive & (1 << c) == 0)
        .map(|c| c.to_string())
        .collect();
    let live: Vec<usize> = (0..LOGICAL_CHANNELS)
        .filter(|&c| p.alive & (1 << c) != 0)
        .collect();
    let echo_at_mics = metrics::level(
        &(0..raw[0][SPEECH].len())
            .map(|k| live.iter().map(|&c| raw[c][SPEECH][k]).sum::<f64>() / live.len() as f64)
            .collect::<Vec<_>>(),
        &f.echo,
    );
    let raw_clips: usize = take
        .lanes
        .iter()
        .map(|l| {
            l[..LOGICAL_CHANNELS]
                .iter()
                .filter(|v| v.abs() >= 0.999)
                .count()
        })
        .sum();
    let talker = metrics::snr(
        &raw[frontend::CENTRE][SPEECH],
        |k| f.talk[k],
        |k| f.residual[k],
    );
    // Each live lane against the others' mean: echo, and the talker's frames.
    let per_lane = |mask: &[bool]| -> String {
        let l: Vec<(usize, f64)> = live
            .iter()
            .filter_map(|&c| metrics::level(&raw[c][SPEECH], mask).map(|v| (c, v)))
            .collect();
        let mean = l.iter().map(|x| x.1).sum::<f64>() / l.len().max(1) as f64;
        l.iter()
            .map(|(c, v)| format!("{c}: {:+.1}", v - mean))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let talker_level = metrics::level(&raw[frontend::CENTRE][SPEECH], &f.talk);
    let playback_s =
        f.playback.iter().filter(|&&b| b).count() as f64 * HOP as f64 / f64::from(RATE);
    let mut s = String::new();
    let _ = writeln!(s, "## {} ({seconds:.1} s)\n", take.name);
    let _ = writeln!(
        s,
        "- Lanes: {} live{}. Raw samples at full scale: {raw_clips}.",
        live.len(),
        if dead.is_empty() {
            String::new()
        } else {
            format!(", dead: {}", dead.join(", "))
        }
    );
    if f.has_playback {
        let _ = writeln!(
            s,
            "- Playback: {playback_s:.0} s. Echo at the mics (300-3400 Hz, mean of live lanes, after the first 3 s): {} dBFS.",
            fmt(echo_at_mics)
        );
        let _ = writeln!(
            s,
            "- Echo per lane, dB against their mean: {}.",
            per_lane(&f.echo)
        );
    } else {
        let _ = writeln!(s, "- No playback.");
    }
    let _ = writeln!(
        s,
        "- Talker: {} utterances{}. At the centre mic: {} dBFS (300-3400 Hz), {} dB over {}.",
        take.cues.len(),
        if take.scripted {
            " (cues file)"
        } else {
            " found by level; what was said is unknown"
        },
        fmt(talker_level),
        fmt(talker),
        if f.has_playback {
            "the echo"
        } else {
            "the room"
        },
    );
    if !take.cues.is_empty() {
        let _ = writeln!(
            s,
            "- Talker frames per lane, dB against their mean: {}.",
            per_lane(&f.talk)
        );
    }
    s.push('\n');
    let _ = writeln!(
        s,
        "SNR is the talker over {} in each band. Hits are utterances detected, of {}; relief is the detector's playback rule (one supporting score), which pinnad turns on while the satellite plays. False accepts count the target word outside utterances, plus every detection of a word nobody said (\"never\").\n",
        if f.has_playback {
            "the residual echo and noise"
        } else {
            "the room"
        },
        take.cues.len()
    );
    let _ = write!(
        s,
        "| front end | canceller | SNR {} | {} | {} | {} |",
        BANDS[0].name, BANDS[1].name, BANDS[2].name, BANDS[3].name
    );
    s += " ERLE (median/s) | residual dBFS | clips +12 dB | CPU % | hits @0.55 | @0.55 relief | @0.45 relief | peak median | false @0.45 relief | never peak |\n";
    s += "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n";
    for r in rows {
        let erle = r
            .erle
            .map_or_else(|| "–".into(), |(a, m)| format!("{a:.1} ({m:.1})"));
        let w = r.wake.as_ref();
        let wcol = |g: &dyn Fn(&WakeResult) -> String| w.map_or_else(|| "–".into(), g);
        let scripted = take.scripted;
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {erle} | {} | {} | {:.1} | {} | {} | {} | {} | {} | {} |",
            r.front,
            r.cancel,
            fmt(r.snr[0]),
            fmt(r.snr[1]),
            fmt(r.snr[2]),
            fmt(r.snr[3]),
            fmt(r.residual_db),
            r.gain_clips,
            r.cpu_pct,
            wcol(&|w| if scripted {
                w.hits[0][DEFAULT_THRESHOLD].to_string()
            } else {
                format!("{} det.", w.detections)
            }),
            wcol(&|w| if scripted {
                w.hits[1][DEFAULT_THRESHOLD].to_string()
            } else {
                "–".into()
            }),
            wcol(&|w| if scripted {
                w.hits[1][LOW_THRESHOLD].to_string()
            } else {
                "–".into()
            }),
            wcol(&|w| {
                let mut v = w.peaks.clone();
                v.sort_by(f32::total_cmp);
                v.get(v.len() / 2)
                    .map_or_else(|| "–".into(), |m| format!("{m:.2}"))
            }),
            wcol(&|w| {
                let fa = if scripted {
                    w.false_accepts[1][LOW_THRESHOLD]
                } else {
                    0
                };
                if w.never_words.is_empty() {
                    format!("{fa} + 0 never")
                } else {
                    format!("{fa} + {}", w.never_words.join(" "))
                }
            }),
            wcol(&|w| format!("{:.2}", w.never_peak)),
        );
        let _ = write!(
            csv,
            "{},{},{},{},{},{},{},{},{},{},{},{},{:.2},{}",
            take.name,
            r.front,
            r.cancel,
            r.per_turn,
            fmt(r.snr[0]),
            fmt(r.snr[1]),
            fmt(r.snr[2]),
            fmt(r.snr[3]),
            r.erle.map_or("–".into(), |e| format!("{:.1}", e.0)),
            r.erle.map_or("–".into(), |e| format!("{:.1}", e.1)),
            fmt(r.residual_db),
            r.gain_clips,
            r.cpu_pct,
            take.cues.len()
        );
        match w {
            Some(w) => {
                for i in 0..2 {
                    for t in 0..THRESHOLDS.len() {
                        let _ = write!(csv, ",{},{}", w.hits[i][t], w.false_accepts[i][t]);
                    }
                }
                let mut v = w.peaks.clone();
                v.sort_by(f32::total_cmp);
                let _ = writeln!(
                    csv,
                    ",{},{:.3},{},{}",
                    v.get(v.len() / 2).map_or("–".into(), |m| format!("{m:.3}")),
                    w.never_peak,
                    w.never_detections,
                    w.never_words.join(" ")
                );
            }
            None => {
                *csv += &",–".repeat(4 * THRESHOLDS.len() + 4);
                csv.push('\n');
            }
        }
    }
    s
}

fn synth_take(args: &[String]) -> Result<()> {
    ensure!(
        args.len() >= 2,
        "usage: micbench synth BASE.wav OUT.wav --talker SPEECH.wav [options]"
    );
    let (base, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    let mut talker: Option<PathBuf> = None;
    let (mut level, mut az, mut el, mut first, mut every) = (-45.0, 0.0, 20.0, 6.0, 6.0);
    let mut count = usize::MAX;
    let mut interferer: Option<PathBuf> = None;
    let (mut i_level, mut i_az) = (-55.0, 90.0);
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--talker" => talker = Some(value()?.into()),
            "--level" => level = value()?.parse()?,
            "--az" => az = value()?.parse()?,
            "--el" => el = value()?.parse()?,
            "--first" => first = value()?.parse()?,
            "--every" => every = value()?.parse()?,
            "--count" => count = value()?.parse()?,
            "--interferer" => interferer = Some(value()?.into()),
            "--interferer-level" => i_level = value()?.parse()?,
            "--interferer-az" => i_az = value()?.parse()?,
            other => bail!("unknown option {other}"),
        }
    }
    let talker = talker.context("--talker is required")?;
    let w = wav::read(&base)?;
    ensure!(
        w.rate == RATE && w.channels >= 9,
        "{}: need 16 kHz and nine lanes",
        base.display()
    );
    let mut lanes: Vec<[f32; 9]> = w
        .samples
        .chunks_exact(w.channels)
        .map(|f| std::array::from_fn(|c| f[c]))
        .collect();
    let alive = frontend::alive_lanes(&frontend::logical(&lanes));
    let speech = wav::read(&talker)?;
    ensure!(speech.rate == RATE, "{}: need 16 kHz", talker.display());
    let speech = speech.channel(0);
    let (_, on, off) = synth::active(&speech);
    let source = synth::Source {
        signal: speech.clone(),
        level_db: level,
        az_deg: az,
        el_deg: el,
    };
    if let Some(p) = interferer {
        let s = wav::read(&p)?.channel(0);
        let mut looped = Vec::with_capacity(lanes.len());
        while looped.len() < lanes.len() {
            looped.extend_from_slice(&s);
            looped.extend(std::iter::repeat_n(0.0, RATE as usize / 2));
        }
        looped.truncate(lanes.len());
        synth::add(
            &mut lanes,
            &synth::Source {
                signal: looped,
                level_db: i_level,
                az_deg: i_az,
                el_deg: 0.0,
            },
            0,
            alive,
        );
    }
    let mut cues = String::from("# start end (s) of each utterance; written by micbench synth\n");
    let mut t = first;
    let mut n = 0;
    while n < count && ((t * f64::from(RATE)) as usize + speech.len()) < lanes.len() {
        let at = (t * f64::from(RATE)) as usize;
        synth::add(&mut lanes, &source, at, alive);
        let _ = writeln!(
            cues,
            "{:.3} {:.3}",
            (at + on) as f64 / f64::from(RATE),
            (at + off) as f64 / f64::from(RATE)
        );
        t += every;
        n += 1;
    }
    let flat: Vec<f32> = lanes.iter().flatten().copied().collect();
    wav::write(&out, RATE, 9, &flat)?;
    std::fs::write(out.with_extension("cues"), cues)?;
    eprintln!(
        "{}: {n} utterances of {} at {level} dBFS from {az} degrees",
        out.display(),
        talker.display()
    );
    Ok(())
}
