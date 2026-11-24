//! Microphone capture: 16 kHz mono s16 frames of 20 ms from a subprocess
//! (`arecord` on the Echo Dot, `pw-record` elsewhere) or a WAV file.

use crate::dsp::{
    FRAME_SAMPLES, FrontEnd, FrontEndConfig, RAW_CHANNELS, RAW_FRAME_BYTES, SAMPLE_RATE,
    SimpleFrontEnd,
};
use crate::mixer::{self, MICPGA_DEFAULT, MixerRunner};
use crate::{Backend, Error, Result};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{Notify, mpsc, watch};

pub type Frame = [i16; FRAME_SAMPLES];

/// How long after the first captured audio the microphone settings are read
/// back: PipeWire has configured the converters by then.
const MIXER_CHECK_AFTER: std::time::Duration = std::time::Duration::from_secs(2);
/// Before its first audio the processed stream gets this long, however short
/// the fallback's time: PipeWire may still be linking it at boot.
const FIRST_AUDIO_WITHIN: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub backend: Backend,
    pub front_end: FrontEndConfig,
    /// Echo Dot analogue mic PGA volume (clamped to 80).
    pub micpga: u32,
    pub mixer: MixerRunner,
    /// Apply (and check) the Echo Dot's microphone mixer settings. A second
    /// stream from the same microphones leaves them to the first.
    pub apply_mixer: bool,
    pub arecord_bin: String,
    /// ALSA capture device on the Echo Dot.
    pub alsa_device: String,
    pub pw_record_bin: String,
    /// `pw-record --target` (node name or serial); default routing if `None`.
    pub pw_target: Option<String>,
    /// Where to turn when the PipeWire stream delivers nothing (ADR 0004).
    pub raw_fallback: Option<RawFallback>,
    /// WAV backend: pace frames at real time (otherwise as fast as possible).
    pub wav_realtime: bool,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Frames buffered towards the consumer before new ones are dropped.
    pub queue_frames: usize,
}

impl CaptureConfig {
    pub fn new(backend: Backend) -> Self {
        CaptureConfig {
            backend,
            front_end: FrontEndConfig::default(),
            micpga: MICPGA_DEFAULT,
            mixer: MixerRunner::default(),
            apply_mixer: true,
            arecord_bin: "arecord".into(),
            alsa_device: "hw:0,24".into(),
            pw_record_bin: "pw-record".into(),
            pw_target: None,
            raw_fallback: None,
            wav_realtime: true,
            backoff_min: Duration::from_millis(250),
            backoff_max: Duration::from_secs(5),
            queue_frames: 100,
        }
    }
}

/// When the processed PipeWire stream ends without audio, or delivers nothing
/// for `after`, record the raw lanes of `target` instead and mix them with the
/// capture's front end (its microphone mask, calibration, high-pass and gain),
/// without echo cancellation. After `retry_after` on the raw lanes, or when
/// they end, the capture tries the processed stream again.
#[derive(Clone, Debug)]
pub struct RawFallback {
    /// The raw lanes' node, `dot_mics`, delivering [`crate::dsp::RAW_CHANNELS`]
    /// lanes of 24-bit samples.
    pub target: String,
    pub after: Duration,
    pub retry_after: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureStatus {
    Starting,
    /// Audio is flowing.
    Running,
    /// The capture process died or failed to start; retrying after backoff.
    Restarting {
        attempt: u32,
        reason: String,
    },
    /// WAV input ended.
    Finished,
    Stopped,
}

pub struct CaptureStream {
    frames: mpsc::Receiver<Frame>,
    status: watch::Receiver<CaptureStatus>,
    restart: Arc<Notify>,
    stop: watch::Sender<bool>,
}

impl CaptureStream {
    /// Next 20 ms frame; `None` once capture has ended (stopped or WAV done).
    pub async fn recv(&mut self) -> Option<Frame> {
        self.frames.recv().await
    }

    /// The next frame if one is waiting.
    pub fn try_recv(&mut self) -> Option<Frame> {
        self.frames.try_recv().ok()
    }

    /// Status updates, including subprocess death and restarts.
    pub fn status(&self) -> watch::Receiver<CaptureStatus> {
        self.status.clone()
    }

    /// Kill and relaunch the capture process now (e.g. after un-muting).
    pub fn restart(&self) {
        self.restart.notify_one();
    }

    /// Stop capture and release the device.
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start capturing with the default [`SimpleFrontEnd`] built from the config.
pub async fn start_capture(cfg: CaptureConfig) -> Result<CaptureStream> {
    let fe_cfg = cfg.front_end.clone();
    start_capture_with(cfg, move || Box::new(SimpleFrontEnd::new(fe_cfg.clone())))
}

/// Like [`start_capture`] with a custom multi-channel front end (beamforming,
/// AEC...) created on every (re)start. Only used by the Echo Dot backend.
pub fn start_capture_with(
    cfg: CaptureConfig,
    front_end: impl Fn() -> Box<dyn FrontEnd> + Send + Sync + 'static,
) -> Result<CaptureStream> {
    let (tx, rx) = mpsc::channel(cfg.queue_frames.max(1));
    let (status_tx, status_rx) = watch::channel(CaptureStatus::Starting);
    let (stop_tx, stop_rx) = watch::channel(false);
    let restart = Arc::new(Notify::new());
    tokio::spawn(supervise(
        cfg,
        Box::new(front_end),
        tx,
        status_tx,
        restart.clone(),
        stop_rx,
    ));
    Ok(CaptureStream {
        frames: rx,
        status: status_rx,
        restart,
        stop: stop_tx,
    })
}

enum RunEnd {
    /// The process ended or failed; restart after backoff. Bool: it had
    /// produced audio.
    Died(String, bool),
    RestartRequested,
    Stopped,
    ConsumerGone,
    WavDone,
    /// The processed stream delivered nothing for the fallback's time.
    Silent,
    /// The raw lanes ran for the fallback's `retry_after`.
    Retry,
}

type FrontEndFactory = Box<dyn Fn() -> Box<dyn FrontEnd> + Send + Sync>;

async fn supervise(
    cfg: CaptureConfig,
    make_fe: FrontEndFactory,
    tx: mpsc::Sender<Frame>,
    status: watch::Sender<CaptureStatus>,
    restart: Arc<Notify>,
    mut stop: watch::Receiver<bool>,
) {
    let mut attempt = 0u32;
    let mut backoff = cfg.backoff_min;
    // On the raw lanes since the processed stream delivered nothing.
    let mut raw = false;
    // Said so in the log, until the processed stream delivers again.
    let mut warned = false;
    loop {
        let _ = status.send(CaptureStatus::Starting);
        let started = Instant::now();
        let end = match &cfg.backend {
            Backend::WavFile(path) => run_wav(&cfg, path, &tx, &status, &mut stop).await,
            _ => {
                run_process(
                    &cfg,
                    raw,
                    &mut warned,
                    &make_fe,
                    &tx,
                    &status,
                    &restart,
                    &mut stop,
                )
                .await
            }
        };
        // The processed stream ended or stalled without audio: pw-record
        // exits at once when its node is missing (node.dont-fallback).
        let no_audio = matches!(end, RunEnd::Silent | RunEnd::Died(_, false));
        if !raw && cfg.raw_fallback.is_some() && no_audio {
            raw = true;
            if !warned {
                warned = true;
                tracing::warn!(
                    "microphone: the processed stream delivers nothing; recording the raw lanes, without echo cancellation"
                );
            }
            continue;
        }
        if raw {
            // Try the processed stream again; if the raw lanes failed, after
            // the usual backoff.
            raw = false;
            if let RunEnd::Retry = end {
                continue;
            }
        }
        match end {
            RunEnd::Stopped | RunEnd::ConsumerGone => {
                let _ = status.send(CaptureStatus::Stopped);
                return;
            }
            RunEnd::WavDone => {
                let _ = status.send(CaptureStatus::Finished);
                return;
            }
            RunEnd::RestartRequested | RunEnd::Silent | RunEnd::Retry => {
                attempt = 0;
                backoff = cfg.backoff_min;
            }
            RunEnd::Died(reason, had_audio) => {
                if had_audio && started.elapsed() > Duration::from_secs(5) {
                    attempt = 0;
                    backoff = cfg.backoff_min;
                }
                attempt += 1;
                tracing::warn!(attempt, %reason, "capture died; restarting in {backoff:?}");
                let _ = status.send(CaptureStatus::Restarting { attempt, reason });
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = restart.notified() => {}
                    _ = stop.changed() => {
                        let _ = status.send(CaptureStatus::Stopped);
                        return;
                    }
                }
                backoff = (backoff * 2).min(cfg.backoff_max);
            }
        }
    }
}

/// Splits a sample stream into frames and forwards them without ever
/// blocking the reader (a slow consumer drops frames, never stalls arecord).
struct Framer {
    pending: Vec<i16>,
    dropped: u64,
}

impl Framer {
    fn push(&mut self, samples: &[i16], tx: &mpsc::Sender<Frame>) -> bool {
        self.pending.extend_from_slice(samples);
        let mut used = 0;
        while self.pending.len() - used >= FRAME_SAMPLES {
            let mut f = [0i16; FRAME_SAMPLES];
            f.copy_from_slice(&self.pending[used..used + FRAME_SAMPLES]);
            used += FRAME_SAMPLES;
            match tx.try_send(f) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.dropped += 1;
                    if self.dropped.is_power_of_two() {
                        tracing::warn!(
                            dropped = self.dropped,
                            "capture consumer too slow; dropping frames"
                        );
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return false,
            }
        }
        self.pending.drain(..used);
        true
    }
}

/// Command line for the capture subprocess; `raw` records the fallback's raw
/// lanes instead of the processed stream.
pub fn capture_command(cfg: &CaptureConfig, raw: bool) -> Option<(String, Vec<String>)> {
    if let (true, Some(f)) = (raw, &cfg.raw_fallback) {
        return Some((
            cfg.pw_record_bin.clone(),
            [
                "--raw",
                "--rate",
                "16000",
                "--channels",
                &RAW_CHANNELS.to_string(),
                "--format",
                "s24",
                "--target",
                &f.target,
                "-",
            ]
            .map(String::from)
            .to_vec(),
        ));
    }
    match cfg.backend {
        Backend::EchoDot => Some((
            cfg.arecord_bin.clone(),
            [
                "-D",
                &cfg.alsa_device,
                "-f",
                "S24_3LE",
                "-c",
                "9",
                "-r",
                "16000",
                "--period-size=640",
                "--buffer-size=2560",
                "-t",
                "raw",
                "-q",
            ]
            .map(String::from)
            .to_vec(),
        )),
        Backend::PipeWire | Backend::EchoDotPipeWire => {
            // `--raw`: without it pw-record writes a Sun AU container to stdout.
            let mut a: Vec<String> = [
                "--raw",
                "--rate",
                "16000",
                "--channels",
                "1",
                "--format",
                "s16",
            ]
            .map(String::from)
            .to_vec();
            let target = cfg
                .pw_target
                .clone()
                .or_else(|| cfg.backend.pw_capture_target().map(String::from));
            if let Some(t) = target {
                a.push("--target".into());
                a.push(t);
            }
            if cfg.raw_fallback.is_some() {
                // A missing node must stay silent, not be swapped for the
                // default source: the raw lanes are the fallback.
                a.push("-P".into());
                a.push("{ node.dont-fallback = true }".into());
            }
            a.push("-".into());
            Some((cfg.pw_record_bin.clone(), a))
        }
        Backend::WavFile(_) => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_process(
    cfg: &CaptureConfig,
    raw: bool,
    warned: &mut bool,
    make_fe: &FrontEndFactory,
    tx: &mpsc::Sender<Frame>,
    status: &watch::Sender<CaptureStatus>,
    restart: &Notify,
    stop: &mut watch::Receiver<bool>,
) -> RunEnd {
    if cfg.apply_mixer
        && cfg.backend.echo_dot_mixer()
        && let Err(e) = cfg.mixer.apply(&mixer::capture_setup(cfg.micpga)).await
    {
        return RunEnd::Died(format!("mixer setup: {e}"), false);
    }
    let (bin, args) = capture_command(cfg, raw).expect("process backend");
    // How long the processed stream may stay silent before the raw lanes, and
    // how long the raw lanes run before the processed stream is tried again.
    let silent_after = cfg.raw_fallback.as_ref().filter(|_| !raw).map(|f| f.after);
    let retry_at = cfg
        .raw_fallback
        .as_ref()
        .filter(|_| raw)
        .map(|f| tokio::time::Instant::now() + f.retry_after);
    let mut child: Child = match Command::new(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return RunEnd::Died(format!("spawn {bin}: {e}"), false),
    };
    let mut stdout = child.stdout.take().expect("piped");
    let mut fe = make_fe();
    // Raw lanes of 24-bit samples, mixed by the front end.
    let echo = cfg.backend == Backend::EchoDot || raw;
    let mut framer = Framer {
        pending: Vec::new(),
        dropped: 0,
    };
    let mut buf = vec![0u8; 8192];
    let mut carry: Vec<u8> = Vec::new();
    let mut samples: Vec<i16> = Vec::new();
    let mut running = false;
    loop {
        let silence = async {
            match silent_after {
                Some(d) if running => tokio::time::sleep(d).await,
                Some(d) => tokio::time::sleep(d.max(FIRST_AUDIO_WITHIN)).await,
                None => std::future::pending().await,
            }
        };
        let retry = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        let n = tokio::select! {
            _ = silence => {
                let _ = child.kill().await;
                return RunEnd::Silent;
            }
            _ = retry => {
                let _ = child.kill().await;
                return RunEnd::Retry;
            }
            r = stdout.read(&mut buf) => match r {
                Ok(0) => {
                    let st = child.wait().await.map(|s| s.to_string()).unwrap_or_default();
                    return RunEnd::Died(format!("{bin} exited ({st})"), running);
                }
                Ok(n) => n,
                Err(e) => return RunEnd::Died(format!("read: {e}"), running),
            },
            _ = restart.notified() => {
                let _ = child.kill().await;
                return RunEnd::RestartRequested;
            }
            _ = stop.changed() => {
                let _ = child.kill().await;
                return RunEnd::Stopped;
            }
        };
        if !running {
            running = true;
            let _ = status.send(CaptureStatus::Running);
            if !raw && *warned {
                *warned = false;
                tracing::info!("microphone: the processed stream delivers again");
            }
            if cfg.apply_mixer && cfg.backend.echo_dot_mixer() {
                // Once PipeWire has opened the microphones, check that every
                // setting held (see MixerRunner::check).
                let (runner, setup) = (cfg.mixer.clone(), mixer::capture_setup(cfg.micpga));
                tokio::spawn(async move {
                    tokio::time::sleep(MIXER_CHECK_AFTER).await;
                    runner.check(&setup).await;
                });
            }
        }
        carry.extend_from_slice(&buf[..n]);
        samples.clear();
        if echo {
            let whole = carry.len() / RAW_FRAME_BYTES * RAW_FRAME_BYTES;
            fe.process(&carry[..whole], &mut samples);
            carry.drain(..whole);
        } else {
            let whole = carry.len() / 2 * 2;
            samples.extend(
                carry[..whole]
                    .chunks_exact(2)
                    .map(|c| i16::from_le_bytes([c[0], c[1]])),
            );
            carry.drain(..whole);
        }
        if !framer.push(&samples, tx) {
            let _ = child.kill().await;
            return RunEnd::ConsumerGone;
        }
    }
}

/// Load a WAV as 16 kHz mono (mixing channels, resampling if needed).
pub fn load_wav_16k_mono(path: &Path) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    let spec = reader.spec();
    let ch = usize::from(spec.channels.max(1));
    let raw: Vec<i16> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .collect::<std::result::Result<_, _>>(),
        (hound::SampleFormat::Int, b @ (8 | 24 | 32)) => reader
            .samples::<i32>()
            .map(|s| {
                s.map(|v| {
                    if b == 8 {
                        (v << 8) as i16
                    } else {
                        (v >> (b - 16)) as i16
                    }
                })
            })
            .collect::<std::result::Result<_, _>>(),
        (hound::SampleFormat::Float, _) => reader
            .samples::<f32>()
            .map(|s| s.map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16))
            .collect::<std::result::Result<_, _>>(),
        _ => return Err(Error::Other("unsupported WAV sample format".into())),
    }
    .map_err(|e| Error::Other(e.to_string()))?;
    let mono: Vec<i16> = raw
        .chunks_exact(ch)
        .map(|f| (f.iter().map(|&s| i32::from(s)).sum::<i32>() / ch as i32) as i16)
        .collect();
    Ok(crate::decode::resample(
        &mono,
        spec.sample_rate,
        SAMPLE_RATE,
    ))
}

async fn run_wav(
    cfg: &CaptureConfig,
    path: &Path,
    tx: &mpsc::Sender<Frame>,
    status: &watch::Sender<CaptureStatus>,
    stop: &mut watch::Receiver<bool>,
) -> RunEnd {
    let p = path.to_path_buf();
    let samples = match tokio::task::spawn_blocking(move || load_wav_16k_mono(&p)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::error!("{e}");
            return RunEnd::WavDone;
        }
        Err(_) => return RunEnd::WavDone,
    };
    let _ = status.send(CaptureStatus::Running);
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    for chunk in samples.chunks(FRAME_SAMPLES) {
        let mut f = [0i16; FRAME_SAMPLES];
        f[..chunk.len()].copy_from_slice(chunk);
        if cfg.wav_realtime {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.changed() => return RunEnd::Stopped,
            }
        } else if *stop.borrow() {
            return RunEnd::Stopped;
        }
        // File input is lossless: wait for the consumer rather than drop.
        tokio::select! {
            r = tx.send(f) => if r.is_err() { return RunEnd::ConsumerGone },
            _ = stop.changed() => return RunEnd::Stopped,
        }
    }
    RunEnd::WavDone
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("pinna-cap-{name}-{}.wav", std::process::id()))
    }

    #[test]
    fn echodot_command_matches_spec() {
        let (bin, args) = capture_command(&CaptureConfig::new(Backend::EchoDot), false).unwrap();
        assert_eq!(bin, "arecord");
        assert_eq!(
            args.join(" "),
            "-D hw:0,24 -f S24_3LE -c 9 -r 16000 --period-size=640 --buffer-size=2560 -t raw -q"
        );
    }

    #[test]
    fn pipewire_command_and_target() {
        let mut cfg = CaptureConfig::new(Backend::PipeWire);
        let (_, args) = capture_command(&cfg, false).unwrap();
        assert_eq!(
            args.join(" "),
            "--raw --rate 16000 --channels 1 --format s16 -"
        );
        cfg.pw_target = Some("alsa_input.x".into());
        let (_, args) = capture_command(&cfg, false).unwrap();
        assert_eq!(
            args.join(" "),
            "--raw --rate 16000 --channels 1 --format s16 --target alsa_input.x -"
        );
    }

    #[tokio::test]
    async fn wav_frames_fast() {
        let path = tmp("fast");
        let samples: Vec<i16> = (0..1000).map(|i| i as i16).collect();
        write_wav(&path, 16000, 1, &samples);
        let mut cfg = CaptureConfig::new(Backend::WavFile(path.clone()));
        cfg.wav_realtime = false;
        let mut s = start_capture(cfg).await.unwrap();
        let mut got = Vec::new();
        while let Some(f) = s.recv().await {
            got.extend_from_slice(&f);
        }
        assert_eq!(got.len(), 1280); // 4 frames, last one zero padded
        assert_eq!(&got[..1000], &samples[..]);
        assert!(got[1000..].iter().all(|&x| x == 0));
        assert_eq!(*s.status().borrow(), CaptureStatus::Finished);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn wav_realtime_is_paced() {
        let path = tmp("rt");
        write_wav(&path, 16000, 1, &vec![0i16; 320 * 5]);
        let cfg = CaptureConfig::new(Backend::WavFile(path.clone()));
        let t0 = Instant::now();
        let mut s = start_capture(cfg).await.unwrap();
        let mut n = 0;
        while s.recv().await.is_some() {
            n += 1;
        }
        assert_eq!(n, 5);
        assert!(t0.elapsed() >= Duration::from_millis(75));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn wav_downmix_and_resample() {
        let path = tmp("st");
        // 32 kHz stereo, L = 1000, R = 3000 -> mono mean 2000 at 16 kHz.
        let mut st = Vec::new();
        for _ in 0..6400 {
            st.push(1000);
            st.push(3000);
        }
        write_wav(&path, 32000, 2, &st);
        let mono = load_wav_16k_mono(&path).unwrap();
        assert_eq!(mono.len(), 3200);
        assert!((i32::from(mono[1600]) - 2000).abs() < 5);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn stop_ends_stream() {
        let path = tmp("stop");
        write_wav(&path, 16000, 1, &vec![0i16; 16000 * 10]);
        let cfg = CaptureConfig::new(Backend::WavFile(path.clone()));
        let mut s = start_capture(cfg).await.unwrap();
        assert!(s.recv().await.is_some());
        s.stop();
        let mut rest = 0;
        while s.recv().await.is_some() {
            rest += 1;
            assert!(rest < 10);
        }
        std::fs::remove_file(path).unwrap();
    }

    /// A subprocess that dies is restarted with backoff and reported.
    #[tokio::test]
    async fn dead_process_is_restarted_and_reported() {
        let mut cfg = CaptureConfig::new(Backend::PipeWire);
        cfg.pw_record_bin = "/bin/sh".into(); // args are invalid for sh -> exits
        cfg.backoff_min = Duration::from_millis(20);
        cfg.backoff_max = Duration::from_millis(40);
        let s = start_capture(cfg).await.unwrap();
        let mut st = s.status();
        let mut max_attempt = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while max_attempt < 3 && Instant::now() < deadline {
            if tokio::time::timeout(Duration::from_millis(500), st.changed())
                .await
                .is_err()
            {
                break;
            }
            if let CaptureStatus::Restarting { attempt, .. } = &*st.borrow() {
                max_attempt = *attempt;
            }
        }
        assert!(max_attempt >= 3, "attempts {max_attempt}");
    }

    #[test]
    fn echo_pipewire_records_dot_wake_and_falls_back_to_the_raw_lanes() {
        let mut cfg = CaptureConfig::new(Backend::EchoDotPipeWire);
        cfg.raw_fallback = Some(RawFallback {
            target: "dot_mics".into(),
            after: Duration::from_secs(1),
            retry_after: Duration::from_secs(60),
        });
        let (_, args) = capture_command(&cfg, false).unwrap();
        assert_eq!(
            args.join(" "),
            "--raw --rate 16000 --channels 1 --format s16 --target dot_wake -P { node.dont-fallback = true } -"
        );
        let (_, args) = capture_command(&cfg, true).unwrap();
        assert_eq!(
            args.join(" "),
            "--raw --rate 16000 --channels 9 --format s24 --target dot_mics -"
        );
    }

    /// The processed stream delivers one frame and then nothing: the capture
    /// turns to the raw lanes and mixes them.
    #[tokio::test]
    async fn a_silent_processed_stream_gives_way_to_the_raw_lanes() {
        let dir = std::env::temp_dir().join(format!("pinna-fakepwrecord-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("pw-record");
        // dot_mics: 640 frames of 9 x S24_3LE, every byte 0x10 (octal 020).
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$*\" in\n*dot_mics*) head -c 17280 /dev/zero | tr '\\000' '\\020'; sleep 1 ;;\n*) head -c 640 /dev/zero; sleep 10 ;;\nesac\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = CaptureConfig::new(Backend::EchoDotPipeWire);
        cfg.pw_record_bin = script.to_string_lossy().into();
        cfg.apply_mixer = false;
        cfg.raw_fallback = Some(RawFallback {
            target: "dot_mics".into(),
            after: Duration::from_millis(200),
            retry_after: Duration::from_secs(60),
        });
        cfg.front_end.highpass = false;
        cfg.front_end.digital_gain = 100;
        let mut s = start_capture(cfg).await.unwrap();
        let first = tokio::time::timeout(Duration::from_secs(3), s.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            first.iter().all(|&x| x == 0),
            "the processed stream's frame"
        );
        let next = tokio::time::timeout(Duration::from_secs(3), s.recv())
            .await
            .unwrap()
            .unwrap();
        // The mean of lanes whose samples are all 0x101010 >> 8 = 0x1010.
        assert!(next.iter().all(|&x| x == 0x1010), "{:?}", &next[..4]);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// dot_wake is missing at first, so pw-record exits at once (as with
    /// node.dont-fallback): the raw lanes stand in, and once the processed
    /// stream is there again, the capture returns to it.
    #[tokio::test]
    async fn a_missing_processed_stream_gives_way_to_the_raw_lanes_and_comes_back() {
        let dir = std::env::temp_dir().join(format!("pinna-fakepwmissing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("pw-record");
        let marker = dir.join("dot_wake-tried");
        // dot_wake: missing the first time, then frames of 0x0202; dot_mics:
        // lanes whose bytes are all 0x10 (octal 020).
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n*dot_mics*) while :; do head -c 17280 /dev/zero | tr '\\000' '\\020'; sleep 0.05; done ;;\n*) if [ -e {m} ]; then head -c 64000 /dev/zero | tr '\\000' '\\002'; sleep 10; else touch {m}; echo 'target not found' >&2; exit 1; fi ;;\nesac\n",
                m = marker.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = CaptureConfig::new(Backend::EchoDotPipeWire);
        cfg.pw_record_bin = script.to_string_lossy().into();
        cfg.apply_mixer = false;
        cfg.raw_fallback = Some(RawFallback {
            target: "dot_mics".into(),
            after: Duration::from_secs(1),
            retry_after: Duration::from_millis(300),
        });
        cfg.front_end.highpass = false;
        cfg.front_end.digital_gain = 100;
        let mut s = start_capture(cfg).await.unwrap();
        let (mut raw, mut back) = (false, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !back && Instant::now() < deadline {
            let Ok(Some(f)) = tokio::time::timeout(Duration::from_secs(2), s.recv()).await else {
                break;
            };
            raw |= f.iter().all(|&x| x == 0x1010);
            back |= raw && f.iter().all(|&x| x == 0x0202);
        }
        assert!(raw, "the raw lanes never stood in");
        assert!(back, "the processed stream never came back");
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fake_arecord_stream_is_decoded_into_frames() {
        // A shell "arecord" emitting 640 raw frames of 9 x S24_3LE.
        let dir = std::env::temp_dir().join(format!("pinna-fakearecord-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("arecord");
        // Every byte is 0x10 (octal 020).
        std::fs::write(
            &script,
            "#!/bin/sh\nhead -c 17280 /dev/zero | tr '\\000' '\\020' | cat\nsleep 1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = CaptureConfig::new(Backend::EchoDot);
        cfg.arecord_bin = script.to_string_lossy().into();
        cfg.mixer.bin = "true".into();
        cfg.front_end.highpass = false;
        cfg.front_end.digital_gain = 100;
        let mut s = start_capture(cfg).await.unwrap();
        let f = tokio::time::timeout(Duration::from_secs(3), s.recv())
            .await
            .unwrap()
            .unwrap();
        // Every byte is 0x10: sample = 0x101010 >> 8 = 0x1010 = 4112.
        assert!(f.iter().all(|&x| x == 0x1010), "{:?}", &f[..4]);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
