//! Echo-cancellation reference and speaker EQ for the Echo Dot's PipeWire graph.
//!
//! The speaker is driven straight through ALSA at 48 kHz (click-free amp
//! sequencing, full bandwidth); PipeWire's echo canceller only needs the speech
//! band, so every chunk that goes to the speaker is also sent, as 16 kHz mono,
//! to the echo-cancel sink (`dot_ref`). A slow or missing PipeWire must never
//! stall the speaker: chunks go through a bounded queue and are dropped when it
//! is full. A reference that arrives after its echo cancels nothing, so a chunk
//! is also dropped when pw-play hasn't yet read the ones before it (ADR 0004).

use std::os::fd::{AsRawFd, RawFd};
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;

/// Second-order filter (RBJ cookbook), one per channel.
#[derive(Clone, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn highpass(rate: f32, freq: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Band-pass with 0 dB gain at `freq`.
    pub fn bandpass(rate: f32, freq: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Speaker EQ: keep energy the 40 mm driver cannot reproduce out of it.
#[derive(Clone, Debug)]
pub struct SpeakerEq {
    l: Biquad,
    r: Biquad,
}

impl SpeakerEq {
    pub const HIGHPASS_HZ: f32 = 150.0;

    pub fn new() -> Self {
        let hp = Biquad::highpass(48_000.0, Self::HIGHPASS_HZ, std::f32::consts::FRAC_1_SQRT_2);
        Self {
            l: hp.clone(),
            r: hp,
        }
    }

    /// One stereo frame, unclipped.
    pub fn process_frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        (self.l.process(l), self.r.process(r))
    }

    /// In place on interleaved S16LE stereo bytes.
    pub fn process(&mut self, bytes: &mut [u8]) {
        for f in bytes.chunks_exact_mut(4) {
            let l = f32::from(i16::from_le_bytes([f[0], f[1]]));
            let r = f32::from(i16::from_le_bytes([f[2], f[3]]));
            let (l, r) = self.process_frame(l, r);
            let l = l.round().clamp(-32768.0, 32767.0) as i16;
            let r = r.round().clamp(-32768.0, 32767.0) as i16;
            f[..2].copy_from_slice(&l.to_le_bytes());
            f[2..].copy_from_slice(&r.to_le_bytes());
        }
    }
}

impl Default for SpeakerEq {
    fn default() -> Self {
        Self::new()
    }
}

/// 48 kHz stereo S16LE -> 16 kHz mono S16LE: downmix, low-pass FIR, then pick
/// output samples every `step` input samples (3.0 nominally; adjusted so the
/// output runs at the microphone clock while the input runs at the speaker's).
#[derive(Clone, Debug)]
pub struct Decimator3 {
    taps: Vec<f32>,
    hist: Vec<f32>,
    pos: usize,
    /// Index of the next input sample.
    n: f64,
    /// Position (in input samples) of the next output sample.
    next: f64,
    step: f64,
    prev: f32,
}

impl Decimator3 {
    pub fn new() -> Self {
        // Windowed sinc, cutoff 7 kHz at 48 kHz, 47 taps (Blackman).
        let n = 47usize;
        let fc = 7_000.0 / 48_000.0;
        let m = (n - 1) as f32;
        let mut taps: Vec<f32> = (0..n)
            .map(|i| {
                let x = i as f32 - m / 2.0;
                let sinc = if x == 0.0 {
                    2.0 * fc
                } else {
                    (2.0 * std::f32::consts::PI * fc * x).sin() / (std::f32::consts::PI * x)
                };
                let t = i as f32 / m;
                let w = 0.42 - 0.5 * (2.0 * std::f32::consts::PI * t).cos()
                    + 0.08 * (4.0 * std::f32::consts::PI * t).cos();
                sinc * w
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Self {
            hist: vec![0.0; n],
            taps,
            pos: 0,
            n: 0.0,
            next: 0.0,
            step: 3.0,
            prev: 0.0,
        }
    }

    /// Input samples per output sample (3.0 = exactly 48 kHz -> 16 kHz).
    pub fn set_step(&mut self, step: f64) {
        self.step = step.clamp(2.97, 3.03);
    }

    pub fn process(&mut self, stereo: &[u8], out: &mut Vec<u8>) {
        let n = self.taps.len();
        for f in stereo.chunks_exact(4) {
            let l = f32::from(i16::from_le_bytes([f[0], f[1]]));
            let r = f32::from(i16::from_le_bytes([f[2], f[3]]));
            self.hist[self.pos] = (l + r) * 0.5;
            self.pos = (self.pos + 1) % n;
            let mut y = 0.0;
            for (k, t) in self.taps.iter().enumerate() {
                y += t * self.hist[(self.pos + k) % n];
            }
            // y is the filtered sample at index self.n, prev at self.n - 1.
            while self.next <= self.n {
                let frac = (self.next - (self.n - 1.0)) as f32;
                let s = self.prev + (y - self.prev) * frac;
                out.extend_from_slice(&(s.round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
                self.next += self.step;
            }
            self.prev = y;
            self.n += 1.0;
            if self.n > 1e6 {
                self.n -= 1e6;
                self.next -= 1e6;
            }
        }
    }
}

impl Default for Decimator3 {
    fn default() -> Self {
        Self::new()
    }
}

/// A running `pw-play` into the echo-cancel reference sink, fed through a bounded queue.
pub struct RefTap {
    tx: mpsc::Sender<Vec<u8>>,
    decim: Decimator3,
    task: tokio::task::JoinHandle<()>,
}

/// Queue depth in 20 ms chunks (~0.5 s).
const QUEUE: usize = 25;

/// The most reference audio that may wait in pw-play's input pipe: 40 ms of
/// 16 kHz mono s16. The speaker hands each chunk to its player 100 ms before
/// it sounds (`SpeakerConfig::lead`), so the reference reaches the echo
/// canceller ahead of its echo. Without a bound, the pipe (64 KiB, 2 s) filled
/// while pw-play started and never drained, because the speaker writes exactly
/// as fast as PipeWire reads. Measured on an Echo Dot (2nd gen), 2026-11-21:
/// the reference came 2.05–2.16 s after its echo, and the canceller removed
/// nothing at any volume.
const MAX_WAITING: usize = 1_280;

/// Bytes written into the pipe `fd` and not yet read, by `FIONREAD`.
fn unread_bytes(fd: RawFd) -> Option<usize> {
    let mut bytes: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int through the pointer, which is valid
    // for the call; a bad fd only makes the call fail.
    let r = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut bytes) };
    (r == 0).then_some(bytes as usize)
}

/// The wait before starting pw-play again after it exited, doubling while it
/// keeps failing.
const RESTART_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
const RESTART_MAX: std::time::Duration = std::time::Duration::from_secs(30);

fn spawn_player(pw_play: &str, target: &str) -> std::io::Result<tokio::process::Child> {
    Command::new(pw_play)
        .args([
            "--raw",
            "--rate",
            "16000",
            "--channels",
            "1",
            "--format",
            "s16",
            "--latency",
            "100ms",
            "--target",
            target,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

/// Why [`feed`] stopped.
enum Fed {
    /// The tap was dropped.
    Closed,
    /// pw-play stopped taking audio; `any` says whether it took some first.
    Exited { any: bool },
}

/// Feed `child` until its input closes or the tap is dropped.
async fn feed(child: &mut tokio::process::Child, rx: &mut mpsc::Receiver<Vec<u8>>) -> Fed {
    let Some(mut stdin) = child.stdin.take() else {
        return Fed::Exited { any: false };
    };
    let fd = stdin.as_raw_fd();
    let mut any = false;
    while let Some(chunk) = rx.recv().await {
        if unread_bytes(fd).is_some_and(|n| n + chunk.len() > MAX_WAITING) {
            continue;
        }
        if stdin.write_all(&chunk).await.is_err() {
            return Fed::Exited { any };
        }
        any = true;
    }
    Fed::Closed
}

impl RefTap {
    /// Start pw-play into `target`. When it exits (PipeWire restarted, for
    /// one), a new one is started: without it the echo canceller gets no
    /// reference until pinnad restarts.
    pub fn spawn(pw_play: &str, target: &str) -> std::io::Result<Self> {
        let first = spawn_player(pw_play, target)?;
        let (pw_play, target) = (pw_play.to_owned(), target.to_owned());
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(QUEUE);
        let task = tokio::spawn(async move {
            let mut child = Some(first);
            let mut wait = RESTART_FIRST;
            loop {
                if let Some(mut c) = child.take() {
                    let fed = feed(&mut c, &mut rx).await;
                    let _ = c.kill().await;
                    match fed {
                        Fed::Closed => return,
                        Fed::Exited { any: true } => {
                            tracing::warn!("echo reference: pw-play exited; starting it again");
                            wait = RESTART_FIRST;
                        }
                        Fed::Exited { any: false } => {
                            tracing::debug!("echo reference: pw-play took no audio")
                        }
                    }
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(RESTART_MAX);
                // What queued up meanwhile is late, and a late reference cancels nothing.
                while rx.try_recv().is_ok() {}
                match spawn_player(&pw_play, &target) {
                    Ok(c) => child = Some(c),
                    Err(e) => tracing::debug!("echo reference: pw-play: {e}"),
                }
            }
        });
        Ok(Self {
            tx,
            decim: Decimator3::new(),
            task,
        })
    }

    /// See [`Decimator3::set_step`].
    pub fn set_step(&mut self, step: f64) {
        self.decim.set_step(step);
    }

    /// Queue the 16 kHz mono copy of a 48 kHz stereo chunk; drops it when the queue is full.
    pub fn push(&mut self, stereo: &[u8]) {
        let mut out = Vec::with_capacity(stereo.len() / 12 * 2 + 2);
        self.decim.process(stereo, &mut out);
        let _ = self.tx.try_send(out);
    }
}

impl Drop for RefTap {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_tone(hz: f32, frames: usize, amp: f32) -> Vec<u8> {
        let mut v = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let s = (amp * (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin()) as i16;
            v.extend_from_slice(&s.to_le_bytes());
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    fn rms(b: &[u8], skip: usize) -> f32 {
        let s: Vec<f32> = b
            .chunks_exact(2)
            .skip(skip)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])))
            .collect();
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn decimator_keeps_speech_band_and_rejects_aliases() {
        let mut d = Decimator3::new();
        let mut out = Vec::new();
        d.process(&stereo_tone(1_000.0, 48_000, 10_000.0), &mut out);
        assert_eq!(out.len(), 16_000 * 2);
        let pass = rms(&out, 100);
        assert!((pass - 7071.0).abs() < 300.0, "1 kHz rms {pass}");
        let mut d = Decimator3::new();
        let mut out = Vec::new();
        d.process(&stereo_tone(12_000.0, 48_000, 10_000.0), &mut out);
        let stop = rms(&out, 100);
        assert!(stop < 200.0, "12 kHz leaks {stop}");
    }

    #[test]
    fn decimator_step_sets_the_output_rate() {
        let mut d = Decimator3::new();
        d.set_step(3.0 * (1.0 + 320e-6) / (1.0 + 5e-6));
        let mut out = Vec::new();
        d.process(&stereo_tone(1_000.0, 48_000 * 10, 10_000.0), &mut out);
        // 480000 input frames / 3.000945 = 159949.6 output samples.
        let got = out.len() / 2;
        assert!((159_948..=159_951).contains(&got), "{got}");
    }

    /// A stand-in for pw-play: a shell script, started like the real one.
    async fn fake_player(dir: &std::path::Path, script: &str) -> RefTap {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("pw-play");
        std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        loop {
            match RefTap::spawn(bin.to_str().unwrap(), "dot_ref") {
                // Another test's fork may still hold the new script open.
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                r => break r.unwrap(),
            }
        }
    }

    #[tokio::test]
    async fn the_reference_never_waits_seconds_behind_the_speaker() {
        // A pw-play that reads nothing for half a second, as while it connects.
        let dir = tempfile::tempdir().unwrap();
        let got = dir.path().join("got.raw");
        let mut tap = fake_player(
            dir.path(),
            &format!("sleep 0.5\nexec cat > '{}'", got.display()),
        )
        .await;
        // Meanwhile the speaker plays 2 s, 20 ms at a time.
        let chunk = stereo_tone(1_000.0, 960, 10_000.0);
        for _ in 0..100 {
            tap.push(&chunk);
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
        // What pw-play found waiting once it read: at most 40 ms, not 2 s.
        let waited = std::fs::metadata(&got).unwrap().len();
        assert!(waited > 0, "no reference at all");
        assert!(waited <= MAX_WAITING as u64, "{waited} bytes waited");
    }

    #[tokio::test]
    async fn a_new_pw_play_takes_over_when_the_old_one_exits() {
        // The first pw-play exits after one chunk, as when PipeWire restarts.
        let dir = tempfile::tempdir().unwrap();
        let (runs, got) = (dir.path().join("runs"), dir.path().join("got.raw"));
        let mut tap = fake_player(
            dir.path(),
            &format!(
                "echo run >> '{runs}'\n\
                 [ \"$(wc -l < '{runs}')\" -eq 1 ] && {{ head -c 640 > /dev/null; exit 1; }}\n\
                 exec cat >> '{got}'",
                runs = runs.display(),
                got = got.display()
            ),
        )
        .await;
        // The speaker keeps playing, 20 ms at a time, for 3 s.
        let chunk = stereo_tone(1_000.0, 960, 10_000.0);
        for _ in 0..150 {
            tap.push(&chunk);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let started = std::fs::read_to_string(&runs).unwrap().lines().count();
        assert_eq!(started, 2, "pw-play runs");
        let fed = std::fs::metadata(&got).map(|m| m.len()).unwrap_or(0);
        assert!(fed > 0, "the second pw-play got no reference");
    }

    #[test]
    fn eq_cuts_bass_and_passes_mids() {
        let mut eq = SpeakerEq::new();
        let mut low = stereo_tone(50.0, 48_000, 10_000.0);
        eq.process(&mut low);
        let mut eq = SpeakerEq::new();
        let mut mid = stereo_tone(1_000.0, 48_000, 10_000.0);
        eq.process(&mut mid);
        let (l, m) = (rms(&low, 4_800), rms(&mid, 4_800));
        assert!(l < 0.15 * 7071.0, "50 Hz rms {l}");
        assert!((m - 7071.0).abs() < 150.0, "1 kHz rms {m}");
    }
}
