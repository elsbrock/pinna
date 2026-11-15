//! Speaker output as a long-lived session.
//!
//! Opening the speaker from idle takes ~1.4 s on the Dot (mixer arm, PCM open,
//! amp power-up), so one `aplay` stays open while a turn is active. Silence is
//! written while idle (no underrun pops) and the session is closed after
//! [`SpeakerConfig::idle_timeout`] without audio. Volume is applied digitally
//! in this pipeline using LibreEcho's curve; the codec stays at its 0 dB
//! reference.

use crate::decode::{OUT_RATE, Pcm};
use crate::mixer::{self, AMP_SETTLE, MixerRunner, SpeakerProfile};
use crate::volume::volume_gain;
use crate::{Backend, Error, Result, decode};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch};

/// Frames per write (20 ms at 48 kHz).
const CHUNK_FRAMES: usize = 960;

#[derive(Clone, Debug)]
pub struct SpeakerConfig {
    pub backend: Backend,
    pub profile: SpeakerProfile,
    pub mixer: MixerRunner,
    pub aplay_bin: String,
    /// ALSA playback device on the Echo Dot.
    pub alsa_device: String,
    pub pw_play_bin: String,
    /// `pw-play --target`.
    pub pw_target: Option<String>,
    /// Close the session after this long without audio.
    pub idle_timeout: Duration,
    /// Logical volume 0..=100 at startup.
    pub initial_volume: u8,
    /// How far ahead of real time audio is handed to the player.
    pub lead: Duration,
}

impl SpeakerConfig {
    pub fn new(backend: Backend) -> Self {
        SpeakerConfig {
            backend,
            profile: SpeakerProfile::default(),
            mixer: MixerRunner::default(),
            aplay_bin: "aplay".into(),
            alsa_device: "hw:0,23".into(),
            pw_play_bin: "pw-play".into(),
            pw_target: None,
            idle_timeout: Duration::from_secs(10),
            initial_volume: 50,
            lead: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing queued or sounding (the session may still be open, on silence).
    Idle,
    /// Audio is waiting for the speaker session to open.
    Opening,
    /// Audio is queued or still sounding.
    Playing,
}

struct Clip {
    samples: Vec<i16>,
    pos: usize,
    done: oneshot::Sender<Result<()>>,
}

enum Cmd {
    Play(Clip),
    Stop,
}

struct Inner {
    tx: mpsc::UnboundedSender<Cmd>,
    volume: Arc<AtomicU8>,
    state: watch::Receiver<PlaybackState>,
    session_open: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct Speaker {
    inner: Arc<Inner>,
}

impl Speaker {
    /// Create a speaker and start its task. Must be called inside a tokio
    /// runtime. Nothing is opened until the first clip is played.
    pub fn new(cfg: SpeakerConfig) -> Speaker {
        let (tx, rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = watch::channel(PlaybackState::Idle);
        let volume = Arc::new(AtomicU8::new(cfg.initial_volume.min(100)));
        let session_open = Arc::new(AtomicBool::new(false));
        let actor = Actor {
            cfg,
            rx,
            state: state_tx,
            volume: volume.clone(),
            session_open: session_open.clone(),
            feeder: Feeder::default(),
            finishing: VecDeque::new(),
            session: None,
            idle_since: Instant::now(),
        };
        tokio::spawn(actor.run());
        Speaker {
            inner: Arc::new(Inner {
                tx,
                volume,
                state: state_rx,
                session_open,
            }),
        }
    }

    /// Play a clip of any rate/channel layout (converted to 48 kHz stereo).
    /// Resolves when it has finished sounding, or `Ok` early if [`stop`](Self::stop)
    /// cancelled it; `Err` if the speaker could not be opened.
    pub async fn play_pcm(&self, pcm: Pcm) -> Result<()> {
        let pcm = if pcm.rate == OUT_RATE && pcm.channels == 2 {
            pcm
        } else {
            tokio::task::spawn_blocking(move || pcm.to_48k_stereo())
                .await
                .map_err(|e| Error::Other(e.to_string()))?
        };
        let (done, wait) = oneshot::channel();
        self.inner
            .tx
            .send(Cmd::Play(Clip {
                samples: pcm.samples,
                pos: 0,
                done,
            }))
            .map_err(|_| Error::Other("speaker task ended".into()))?;
        wait.await
            .map_err(|_| Error::Other("speaker task ended".into()))?
    }

    /// Decode (wav/mp3/flac/ogg) and play.
    pub async fn play_bytes(&self, encoded: &[u8]) -> Result<()> {
        let bytes = encoded.to_vec();
        let pcm = tokio::task::spawn_blocking(move || decode::decode(&bytes))
            .await
            .map_err(|e| Error::Other(e.to_string()))??;
        self.play_pcm(pcm).await
    }

    /// Logical volume 0..=100 (0 mutes). Takes effect within one 20 ms chunk.
    pub fn set_volume(&self, percent: u8) {
        self.inner.volume.store(percent.min(100), Ordering::Relaxed);
    }

    pub fn volume(&self) -> u8 {
        self.inner.volume.load(Ordering::Relaxed)
    }

    /// Drop everything queued and playing; the session stays open until idle.
    pub fn stop(&self) {
        let _ = self.inner.tx.send(Cmd::Stop);
    }

    pub fn is_playing(&self) -> bool {
        *self.inner.state.borrow() != PlaybackState::Idle
    }

    /// Whether the hardware session (aplay + amp) is currently open.
    pub fn is_session_open(&self) -> bool {
        self.inner.session_open.load(Ordering::Relaxed)
    }

    pub fn state(&self) -> watch::Receiver<PlaybackState> {
        self.inner.state.clone()
    }
}

// ---------------------------------------------------------------------------
// Feeder: queued clips -> volume-scaled s16le stereo chunks (pure, testable)

#[derive(Default)]
struct Feeder {
    queue: VecDeque<Clip>,
    gain: f32,
}

/// A clip that ended inside a chunk, at `frame` (frames into the chunk).
struct Ended {
    done: oneshot::Sender<Result<()>>,
    frame: usize,
}

impl Feeder {
    fn has_audio(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Fill `out` with `frames` frames of s16le stereo (silence when nothing is
    /// queued), ramping the gain linearly to `target_gain` across the chunk.
    fn next_chunk(&mut self, frames: usize, target_gain: f32, out: &mut Vec<u8>) -> Vec<Ended> {
        out.clear();
        let mut ended = Vec::new();
        let start_gain = self.gain;
        let mut i = 0;
        while i < frames {
            let Some(clip) = self.queue.front_mut() else {
                break;
            };
            let take = ((clip.samples.len() - clip.pos) / 2).min(frames - i);
            for f in 0..take {
                let t = (i + f) as f32 / frames as f32;
                let g = start_gain + (target_gain - start_gain) * t;
                for c in 0..2 {
                    let s = f32::from(clip.samples[clip.pos + f * 2 + c]) * g;
                    let s = s.round().clamp(-32768.0, 32767.0) as i16;
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
            clip.pos += take * 2;
            i += take;
            if clip.pos + 1 >= clip.samples.len() {
                let clip = self.queue.pop_front().unwrap();
                ended.push(Ended {
                    done: clip.done,
                    frame: i,
                });
            }
        }
        out.resize(frames * 4, 0);
        self.gain = target_gain;
        ended
    }

    fn cancel_all(&mut self) {
        for c in self.queue.drain(..) {
            let _ = c.done.send(Ok(()));
        }
    }

    fn fail_all(&mut self, msg: &str) {
        for c in self.queue.drain(..) {
            let _ = c.done.send(Err(Error::Other(msg.to_string())));
        }
    }
}

// ---------------------------------------------------------------------------
// Session and actor

enum Sink {
    Process {
        child: Box<Child>,
        stdin: ChildStdin,
    },
    /// Discard (WavFile backend has no playback hardware).
    Null,
}

struct Session {
    sink: Sink,
    /// Wall-clock origin of the frame counter.
    t0: Instant,
    frames_written: u64,
}

impl Session {
    fn time_of(&self, frame: u64) -> Instant {
        self.t0 + Duration::from_secs_f64(frame as f64 / f64::from(OUT_RATE))
    }

    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Sink::Process { stdin, .. } = &mut self.sink {
            stdin.write_all(bytes).await?;
        }
        self.frames_written += (bytes.len() / 4) as u64;
        Ok(())
    }
}

struct Actor {
    cfg: SpeakerConfig,
    rx: mpsc::UnboundedReceiver<Cmd>,
    state: watch::Sender<PlaybackState>,
    volume: Arc<AtomicU8>,
    session_open: Arc<AtomicBool>,
    feeder: Feeder,
    /// Clips fully handed to the player, completing at the given frame.
    finishing: VecDeque<(u64, oneshot::Sender<Result<()>>)>,
    session: Option<Session>,
    idle_since: Instant,
}

/// Player command line.
pub fn playback_command(cfg: &SpeakerConfig) -> Option<(String, Vec<String>)> {
    match cfg.backend {
        Backend::EchoDot => Some((
            cfg.aplay_bin.clone(),
            [
                "-D",
                &cfg.alsa_device,
                "-f",
                "S16_LE",
                "-c",
                "2",
                "-r",
                "48000",
                "-t",
                "raw",
                "-q",
            ]
            .map(String::from)
            .to_vec(),
        )),
        Backend::PipeWire => {
            // `--raw`: otherwise pw-play expects a container on stdin.
            let mut a: Vec<String> = [
                "--raw",
                "--rate",
                "48000",
                "--channels",
                "2",
                "--format",
                "s16",
            ]
            .map(String::from)
            .to_vec();
            if let Some(t) = &cfg.pw_target {
                a.push("--target".into());
                a.push(t.clone());
            }
            a.push("-".into());
            Some((cfg.pw_play_bin.clone(), a))
        }
        Backend::WavFile(_) => None,
    }
}

impl Actor {
    fn set_state(&self, s: PlaybackState) {
        self.state.send_if_modified(|cur| {
            let changed = *cur != s;
            *cur = s;
            changed
        });
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play(clip) => {
                if clip.samples.len() < 2 {
                    let _ = clip.done.send(Ok(()));
                } else {
                    self.feeder.queue.push_back(clip);
                }
            }
            Cmd::Stop => {
                self.feeder.cancel_all();
                for (_, d) in self.finishing.drain(..) {
                    let _ = d.send(Ok(()));
                }
            }
        }
    }

    async fn run(mut self) {
        let mut chunk = Vec::with_capacity(CHUNK_FRAMES * 4);
        loop {
            if self.session.is_none() {
                if !self.feeder.has_audio() {
                    self.set_state(PlaybackState::Idle);
                    match self.rx.recv().await {
                        Some(c) => self.handle(c),
                        None => return,
                    }
                    continue;
                }
                self.set_state(PlaybackState::Opening);
                match self.open_session().await {
                    Ok(s) => {
                        self.session = Some(s);
                        self.session_open.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::error!("speaker open failed: {e}");
                        self.feeder.fail_all(&format!("speaker open failed: {e}"));
                        self.set_state(PlaybackState::Idle);
                        continue;
                    }
                }
            }

            // Session open: pace writes, complete clips, idle out.
            let s = self.session.as_ref().unwrap();
            let now = Instant::now();
            let busy = self.feeder.has_audio() || !self.finishing.is_empty();
            if busy {
                self.set_state(PlaybackState::Playing);
            } else {
                self.set_state(PlaybackState::Idle);
            }

            let mut wake = s
                .time_of(s.frames_written)
                .checked_sub(self.cfg.lead)
                .unwrap_or(now);
            if let Some((end, _)) = self.finishing.front() {
                wake = wake.min(s.time_of(*end));
            }
            if !busy {
                wake = wake.min(self.idle_since + self.cfg.idle_timeout);
            }

            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    Some(c) => self.handle(c),
                    None => {
                        self.close_session().await;
                        return;
                    }
                },
                _ = tokio::time::sleep_until(wake.into()) => {}
            }

            let now = Instant::now();
            // Complete clips whose last frame has sounded.
            while let Some((end, _)) = self.finishing.front() {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.time_of(*end) <= now)
                {
                    let (_, d) = self.finishing.pop_front().unwrap();
                    let _ = d.send(Ok(()));
                } else {
                    break;
                }
            }
            if self.feeder.has_audio() || !self.finishing.is_empty() {
                self.idle_since = now;
            } else if now >= self.idle_since + self.cfg.idle_timeout {
                self.close_session().await;
                continue;
            }

            let s = self.session.as_mut().unwrap();
            if s.time_of(s.frames_written).saturating_duration_since(now) > self.cfg.lead {
                continue; // not due yet
            }
            let gain = volume_gain(self.volume.load(Ordering::Relaxed));
            let ended = self.feeder.next_chunk(CHUNK_FRAMES, gain, &mut chunk);
            let base = s.frames_written;
            if let Err(e) = s.write(&chunk).await {
                tracing::error!("speaker write failed: {e}");
                self.feeder.fail_all("speaker process died");
                for e in ended {
                    let _ = e
                        .done
                        .send(Err(Error::Other("speaker process died".into())));
                }
                for (_, d) in self.finishing.drain(..) {
                    let _ = d.send(Err(Error::Other("speaker process died".into())));
                }
                self.close_session().await;
                continue;
            }
            for e in ended {
                self.finishing.push_back((base + e.frame as u64, e.done));
            }
        }
    }

    /// Mixer arm -> player process -> amp on -> prime silence -> unmute.
    async fn open_session(&mut self) -> Result<Session> {
        let echo = self.cfg.backend == Backend::EchoDot;
        if echo
            && let Err(e) = self
                .cfg
                .mixer
                .apply(&mixer::playback_arm(&self.cfg.profile))
                .await
        {
            let _ = self.cfg.mixer.apply(&mixer::playback_disable()).await;
            return Err(e);
        }
        let sink = match playback_command(&self.cfg) {
            None => Sink::Null,
            Some((bin, args)) => {
                let mut child = Command::new(&bin)
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| Error::Other(format!("spawn {bin}: {e}")))?;
                let stdin = child.stdin.take().expect("piped");
                Sink::Process {
                    child: Box::new(child),
                    stdin,
                }
            }
        };
        let mut session = Session {
            sink,
            t0: Instant::now(),
            frames_written: 0,
        };
        let result: Result<()> = async {
            if echo {
                self.cfg.mixer.apply(&mixer::playback_power()).await?;
                tokio::time::sleep(AMP_SETTLE).await;
            }
            // Pre-roll: twice the lead of silence, written before the unmute.
            session.t0 = Instant::now();
            let silence = vec![0u8; CHUNK_FRAMES * 4];
            let prime_chunks = (self.cfg.lead.as_millis() as usize * 2).div_ceil(20).max(1);
            for _ in 0..prime_chunks {
                session.write(&silence).await?;
            }
            if echo {
                self.cfg.mixer.apply(&mixer::playback_unmute()).await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            Self::close(session, echo.then_some(&self.cfg.mixer)).await;
            return Err(e);
        }
        self.idle_since = Instant::now();
        Ok(session)
    }

    async fn close_session(&mut self) {
        if let Some(s) = self.session.take() {
            let echo = self.cfg.backend == Backend::EchoDot;
            Self::close(s, echo.then_some(&self.cfg.mixer)).await;
        }
        self.session_open.store(false, Ordering::Relaxed);
    }

    /// Mute and cut the amp first (no shutdown pop), then end the player.
    async fn close(session: Session, mixer: Option<&MixerRunner>) {
        if let Some(m) = mixer
            && let Err(e) = m.apply(&mixer::playback_disable()).await
        {
            tracing::warn!("speaker disable controls: {e}");
        }
        if let Sink::Process { mut child, stdin } = session.sink {
            drop(stdin);
            if tokio::time::timeout(Duration::from_secs(1), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(samples: Vec<i16>) -> (Clip, oneshot::Receiver<Result<()>>) {
        let (done, rx) = oneshot::channel();
        (
            Clip {
                samples,
                pos: 0,
                done,
            },
            rx,
        )
    }

    fn decode_chunk(b: &[u8]) -> Vec<i16> {
        b.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn feeder_silence_when_empty() {
        let mut f = Feeder::default();
        let mut out = Vec::new();
        let ended = f.next_chunk(10, 1.0, &mut out);
        assert!(ended.is_empty());
        assert_eq!(out, vec![0u8; 40]);
    }

    #[test]
    fn feeder_scales_and_pads_and_reports_end() {
        let mut f = Feeder {
            gain: 0.5,
            ..Default::default()
        };
        let (c, _rx) = clip(vec![1000, -1000, 2000, -2000]); // 2 frames
        f.queue.push_back(c);
        let mut out = Vec::new();
        let ended = f.next_chunk(4, 0.5, &mut out);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].frame, 2);
        assert_eq!(decode_chunk(&out), [500, -500, 1000, -1000, 0, 0, 0, 0]);
        assert!(!f.has_audio());
    }

    #[test]
    fn feeder_spans_chunks_and_clips() {
        let mut f = Feeder {
            gain: 1.0,
            ..Default::default()
        };
        let (a, _ra) = clip(vec![1; 6]); // 3 frames
        let (b, _rb) = clip(vec![2; 6]);
        f.queue.push_back(a);
        f.queue.push_back(b);
        let mut out = Vec::new();
        let e1 = f.next_chunk(4, 1.0, &mut out);
        assert_eq!(e1.len(), 1);
        assert_eq!(e1[0].frame, 3);
        assert_eq!(decode_chunk(&out), [1, 1, 1, 1, 1, 1, 2, 2]);
        let e2 = f.next_chunk(4, 1.0, &mut out);
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].frame, 2);
    }

    #[test]
    fn feeder_ramps_gain_and_saturates() {
        let mut f = Feeder {
            gain: 0.0,
            ..Default::default()
        };
        let (c, _rx) = clip(vec![10000; 8]); // 4 frames
        f.queue.push_back(c);
        let mut out = Vec::new();
        f.next_chunk(4, 1.0, &mut out);
        let s = decode_chunk(&out);
        assert_eq!(s[0], 0);
        assert!(s[2] < s[4] && s[4] < s[6]);
        assert!(s[6] < 10000);
        // Gain above unity saturates.
        let mut f = Feeder {
            gain: 4.0,
            ..Default::default()
        };
        let (c, _rx) = clip(vec![30000, -30000]);
        f.queue.push_back(c);
        f.next_chunk(1, 4.0, &mut out);
        assert_eq!(decode_chunk(&out), [i16::MAX, i16::MIN]);
    }

    #[test]
    fn feeder_cancel_completes_ok() {
        let mut f = Feeder::default();
        let (c, mut rx) = clip(vec![1; 100]);
        f.queue.push_back(c);
        f.cancel_all();
        assert!(matches!(rx.try_recv(), Ok(Ok(()))));
    }

    #[test]
    fn commands() {
        let (bin, args) = playback_command(&SpeakerConfig::new(Backend::EchoDot)).unwrap();
        assert_eq!(bin, "aplay");
        assert_eq!(
            args.join(" "),
            "-D hw:0,23 -f S16_LE -c 2 -r 48000 -t raw -q"
        );
        let (bin, args) = playback_command(&SpeakerConfig::new(Backend::PipeWire)).unwrap();
        assert_eq!(bin, "pw-play");
        assert_eq!(
            args.join(" "),
            "--raw --rate 48000 --channels 2 --format s16 -"
        );
        assert!(playback_command(&SpeakerConfig::new(Backend::WavFile("x".into()))).is_none());
    }

    fn null_speaker(idle: Duration) -> Speaker {
        let mut cfg = SpeakerConfig::new(Backend::WavFile("unused".into()));
        cfg.idle_timeout = idle;
        cfg.initial_volume = 100;
        Speaker::new(cfg)
    }

    #[tokio::test]
    async fn plays_in_real_time_and_idles_out() {
        let sp = null_speaker(Duration::from_millis(300));
        let mut st = sp.state();
        assert!(!sp.is_playing());
        let pcm = crate::tone::beep(440.0, 300, 0.5);
        let t0 = Instant::now();
        let play = {
            let sp = sp.clone();
            tokio::spawn(async move { sp.play_pcm(pcm).await })
        };
        // State goes to Playing.
        tokio::time::timeout(Duration::from_secs(2), async {
            while *st.borrow() != PlaybackState::Playing {
                st.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(sp.is_playing());
        play.await.unwrap().unwrap();
        let took = t0.elapsed();
        assert!(
            took >= Duration::from_millis(280) && took < Duration::from_millis(900),
            "{took:?}"
        );
        // Back to Idle but session stays open, then closes after the idle timeout.
        tokio::time::timeout(Duration::from_secs(2), async {
            while *st.borrow() != PlaybackState::Idle {
                st.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(sp.is_session_open());
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(!sp.is_session_open());
    }

    #[tokio::test]
    async fn back_to_back_clips_reuse_one_session() {
        let sp = null_speaker(Duration::from_secs(5));
        sp.play_pcm(crate::tone::beep(440.0, 100, 0.5))
            .await
            .unwrap();
        assert!(sp.is_session_open());
        sp.play_pcm(crate::tone::beep(880.0, 100, 0.5))
            .await
            .unwrap();
        assert!(sp.is_session_open());
    }

    #[tokio::test]
    async fn stop_cancels_playback() {
        let sp = null_speaker(Duration::from_secs(5));
        let long = crate::tone::beep(440.0, 5000, 0.5);
        let h = {
            let sp = sp.clone();
            tokio::spawn(async move { sp.play_pcm(long).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        let t0 = Instant::now();
        sp.stop();
        h.await.unwrap().unwrap();
        assert!(t0.elapsed() < Duration::from_millis(500));
        let mut st = sp.state();
        tokio::time::timeout(Duration::from_secs(1), async {
            while *st.borrow() != PlaybackState::Idle {
                st.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn mixer_sequence_runs_in_order_on_echodot() {
        // Fake amixer logs its args; fake aplay swallows stdin.
        let dir = std::env::temp_dir().join(format!("pinna-fakespk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        let amixer = dir.join("amixer");
        std::fs::write(
            &amixer,
            format!("#!/bin/sh\necho \"$5 $6\" >> {}\n", log.display()),
        )
        .unwrap();
        let aplay = dir.join("aplay");
        std::fs::write(&aplay, "#!/bin/sh\ncat > /dev/null\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        for p in [&amixer, &aplay] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut cfg = SpeakerConfig::new(Backend::EchoDot);
        cfg.mixer.bin = amixer.to_string_lossy().into();
        cfg.aplay_bin = aplay.to_string_lossy().into();
        cfg.idle_timeout = Duration::from_millis(200);
        let sp = Speaker::new(cfg);
        sp.play_pcm(crate::tone::beep(440.0, 60, 0.5))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(900)).await; // idle close
        let log = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        let mut want: Vec<String> = mixer::playback_arm(&SpeakerProfile::default())
            .iter()
            .map(|c| format!("name={} {}", c.control, c.values.join(",")))
            .collect();
        want.extend(
            [
                "name=MFP Gpio Mute On",
                "name=Ext_Speaker_Amp_Switch On",
                "name=MFP Gpio Mute Off",
            ]
            .map(String::from),
        );
        want.extend(["name=MFP Gpio Mute On", "name=Ext_Speaker_Amp_Switch Off"].map(String::from));
        assert_eq!(lines, want);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
