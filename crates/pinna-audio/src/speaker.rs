//! Speaker output as a long-lived session.
//!
//! Opening the speaker from idle takes ~1.4 s on the Dot (mixer arm, PCM open,
//! amp power-up), so one `aplay` stays open while a turn is active. Silence is
//! written while idle (no underrun pops) and the session is closed after
//! [`SpeakerConfig::idle_timeout`] without audio. Volume is applied digitally
//! in this pipeline using LibreEcho's curve; the codec stays at its 0 dB
//! reference.
//!
//! The session mixes two lanes: the *voice* lane (queued clips: replies,
//! announcements, chimes) and the *media* lane (a streamed URL, see
//! [`crate::media`]). Media is ducked under the voice lane and while
//! [`Speaker::duck`] is held.

use crate::decode::{OUT_RATE, Pcm};
use crate::media::{Decoder, MediaHandle, MediaLane, MediaPhase, MediaState};
use crate::mix::{Ducker, FADE, Ramp};
use crate::mixer::{self, AMP_SETTLE, MixerRunner, SpeakerProfile};
use crate::spectrum::{Analyzer, Spectrum};
use crate::volume::volume_gain;
use crate::{Backend, Error, Result, decode};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Media level while ducked, in dB (<= 0).
    pub duck_db: f32,
    /// Fixed gain on top of the volume curve, in dB (the speaker's
    /// loudness calibration).
    pub pre_gain_db: f32,
    /// The playback PCM's `/proc/asound` status file (Echo Dot). The amp is
    /// powered only once it reads `RUNNING`.
    pub pcm_status: Option<PathBuf>,
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
            duck_db: -20.0,
            pre_gain_db: 0.0,
            pcm_status: Some("/proc/asound/card0/pcm23p/sub0/status".into()),
        }
    }
}

/// How long the player may take to start the PCM before the amp is powered
/// anyway (it takes ~1.2 s on the Dot).
const PCM_START_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing queued or sounding (the session may still be open, on silence).
    Idle,
    /// Audio is waiting for the speaker session to open.
    Opening,
    /// Audio is queued or still sounding.
    Playing,
}

/// Chunks of voice level kept: 640 ms, well past the lead.
const LEVEL_HISTORY: usize = 32;
/// A level older than this is not current any more (a chunk lasts 20 ms).
const LEVEL_STALE: Duration = Duration::from_millis(60);

/// Loudness of the voice lane (replies, announcements, chimes) as it is
/// heard: the RMS of each 20 ms chunk in sample units, before any volume, at
/// the moment the chunk starts to sound. Media is not included.
#[derive(Clone, Default)]
pub struct VoiceLevel(Arc<Mutex<VecDeque<(Instant, f32)>>>);

impl VoiceLevel {
    fn push(&self, at: Instant, rms: f32) {
        let mut q = self.0.lock().unwrap();
        if q.len() == LEVEL_HISTORY {
            q.pop_front();
        }
        q.push_back((at, rms));
    }

    /// The level sounding at `now`, or `None` when no voice is playing.
    pub fn at(&self, now: Instant) -> Option<f32> {
        let q = self.0.lock().unwrap();
        q.iter()
            .rev()
            .find(|(t, _)| *t <= now)
            .filter(|(t, _)| now.duration_since(*t) < LEVEL_STALE)
            .map(|(_, rms)| *rms)
    }
}

pub(crate) struct Clip {
    samples: Vec<i16>,
    pos: usize,
    done: oneshot::Sender<Result<()>>,
}

pub(crate) enum Cmd {
    Play(Clip),
    Stop,
    MediaBegin { gen_: u64, url: String },
    MediaAttach { gen_: u64, decoder: Decoder },
    MediaFail { gen_: u64, msg: String },
    MediaStop { gen_: u64 },
    MediaPause,
    MediaResume,
    MediaMute(bool),
}

struct Inner {
    tx: mpsc::UnboundedSender<Cmd>,
    volume: Arc<AtomicU8>,
    voice_gain: Arc<AtomicU8>,
    held_duck: Arc<AtomicBool>,
    media: MediaHandle,
    spectrum: Spectrum,
    state: watch::Receiver<PlaybackState>,
    session_open: Arc<AtomicBool>,
    level: VoiceLevel,
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
        let voice_gain = Arc::new(AtomicU8::new(100));
        let held_duck = Arc::new(AtomicBool::new(false));
        let (media_tx, media_rx) = watch::channel(MediaState::default());
        let media = MediaHandle::new(tx.clone(), media_rx);
        let spectrum = Spectrum::default();
        let level = VoiceLevel::default();
        let actor = Actor {
            feeder: Feeder::new(cfg.duck_db),
            // Only the Echo Dot's own sound card has the two clocks to track.
            clocks: cfg.backend.echo_dot_mixer().then(Clocks::new),
            cfg,
            rx,
            state: state_tx,
            media_state: media_tx,
            volume: volume.clone(),
            voice_gain: voice_gain.clone(),
            held_duck: held_duck.clone(),
            session_open: session_open.clone(),
            finishing: VecDeque::new(),
            session: None,
            idle_since: Instant::now(),
            analyzer: Analyzer::new(OUT_RATE),
            spectrum: spectrum.clone(),
            level: level.clone(),
        };
        tokio::spawn(actor.run());
        Speaker {
            inner: Arc::new(Inner {
                tx,
                volume,
                voice_gain,
                held_duck,
                media,
                spectrum,
                state: state_rx,
                session_open,
                level,
            }),
        }
    }

    /// Loudness of the voice lane as it is heard (for the LED ring).
    pub fn voice_level(&self) -> VoiceLevel {
        self.inner.level.clone()
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

    /// Reply volume 0..=100: scales only the voice lane (default 100), on top
    /// of the master volume.
    pub fn set_voice_gain(&self, percent: u8) {
        self.inner
            .voice_gain
            .store(percent.min(100), Ordering::Relaxed);
    }

    /// Hold the media lane ducked (e.g. while the mic is listening), on top of
    /// the automatic duck under the voice lane.
    pub fn duck(&self, on: bool) {
        self.inner.held_duck.store(on, Ordering::Relaxed);
    }

    /// The media lane (stream a URL, pause/resume/stop, state).
    pub fn media(&self) -> MediaHandle {
        self.inner.media.clone()
    }

    /// Band levels of the media lane as it sounds (the ring's equalizer).
    pub fn spectrum(&self) -> Spectrum {
        self.inner.spectrum.clone()
    }

    /// Drop everything queued and playing in the voice lane; the session stays
    /// open until idle. Media is stopped with [`MediaHandle::stop`].
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

struct Feeder {
    queue: VecDeque<Clip>,
    /// Master gain as of the last chunk.
    gain: f32,
    voice_gain: Ramp,
    held_duck: bool,
    ducker: Ducker,
    media: MediaLane,
    /// The last chunk's media lane as mono (before ducking and gain).
    media_mono: Vec<f32>,
    /// RMS of the voice samples in the last chunk (before any gain); `None`
    /// when the chunk had no voice.
    voice_rms: Option<f32>,
}

impl Default for Feeder {
    fn default() -> Self {
        Feeder::new(-20.0)
    }
}

/// A clip that ended inside a chunk, at `frame` (frames into the chunk).
struct Ended {
    done: oneshot::Sender<Result<()>>,
    frame: usize,
}

impl Feeder {
    fn new(duck_db: f32) -> Feeder {
        Feeder {
            queue: VecDeque::new(),
            gain: 0.0,
            voice_gain: Ramp::new(1.0, FADE),
            held_duck: false,
            ducker: Ducker::new(duck_db),
            media: MediaLane::default(),
            media_mono: Vec::new(),
            voice_rms: None,
        }
    }

    fn has_audio(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Fill `out` with `frames` frames of s16le stereo (silence when nothing is
    /// queued): `voice * voice_gain + media * duck`, saturated, then the master
    /// gain ramped linearly to `target_gain` across the chunk.
    fn next_chunk(&mut self, frames: usize, target_gain: f32, out: &mut Vec<u8>) -> Vec<Ended> {
        out.clear();
        self.media_mono.clear();
        let mut ended = Vec::new();
        let start_gain = self.gain;
        let (mut voice_power, mut voice_samples) = (0.0f64, 0u32);
        for f in 0..frames {
            let mut voice = None;
            if let Some(clip) = self.queue.front_mut() {
                let (l, r) = (
                    f32::from(clip.samples[clip.pos]),
                    f32::from(clip.samples[clip.pos + 1]),
                );
                voice_power += f64::from(l * l) + f64::from(r * r);
                voice_samples += 2;
                voice = Some((l, r));
                clip.pos += 2;
                if clip.pos + 1 >= clip.samples.len() {
                    let clip = self.queue.pop_front().unwrap();
                    ended.push(Ended {
                        done: clip.done,
                        frame: f + 1,
                    });
                }
            }
            let duck = self.ducker.tick(voice.is_some(), self.held_duck);
            let vg = self.voice_gain.tick();
            let (vl, vr) = voice.unwrap_or((0.0, 0.0));
            let (ml, mr) = self.media.pull();
            self.media_mono.push((ml + mr) * 0.5);
            let t = f as f32 / frames as f32;
            let g = start_gain + (target_gain - start_gain) * t;
            for (v, m) in [(vl, ml), (vr, mr)] {
                let mix = (v * vg + m * duck).round().clamp(-32768.0, 32767.0);
                let s = (mix * g).round().clamp(-32768.0, 32767.0) as i16;
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        self.gain = target_gain;
        self.voice_rms =
            (voice_samples > 0).then(|| (voice_power / f64::from(voice_samples)).sqrt() as f32);
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
    /// Pacing: frame `anchor_frame` is due at `anchor_t`, later ones at `rate` per second.
    anchor_t: Instant,
    anchor_frame: u64,
    /// The speaker's real frame rate (measured; nominal until known).
    rate: f64,
    frames_written: u64,
    /// Echo Dot speaker EQ (the device's own speaker only).
    eq: Option<crate::refsink::SpeakerEq>,
    /// Echo-cancellation reference into PipeWire (EchoDotPipeWire).
    reference: Option<crate::refsink::RefTap>,
}

impl Session {
    fn time_of(&self, frame: u64) -> Instant {
        let df = frame as f64 - self.anchor_frame as f64;
        if df >= 0.0 {
            self.anchor_t + Duration::from_secs_f64(df / self.rate)
        } else {
            self.anchor_t
                .checked_sub(Duration::from_secs_f64(-df / self.rate))
                .unwrap_or(self.anchor_t)
        }
    }

    /// Start the pacing clock: the session's first frame sounds at `at`.
    fn start_clock(&mut self, at: Instant) {
        self.anchor_t = at;
        self.anchor_frame = 0;
    }

    /// Pace by a new rate from the next frame on, without a jump.
    fn set_rate(&mut self, rate: f64) {
        if (rate - self.rate).abs() > 0.005 {
            self.anchor_t = self.time_of(self.frames_written);
            self.anchor_frame = self.frames_written;
            self.rate = rate;
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let eq_buf;
        let bytes = match &mut self.eq {
            Some(eq) => {
                let mut b = bytes.to_vec();
                eq.process(&mut b);
                eq_buf = b;
                &eq_buf[..]
            }
            None => bytes,
        };
        if let Some(r) = &mut self.reference {
            r.push(bytes);
        }
        if let Sink::Process { stdin, .. } = &mut self.sink {
            stdin.write_all(bytes).await?;
        }
        self.frames_written += (bytes.len() / 4) as u64;
        Ok(())
    }
}

/// Wait until the player has started the PCM (its status file reads
/// `state: RUNNING`) and return when. Gives up, returning now, when the file
/// cannot be read or after [`PCM_START_TIMEOUT`]; fails if the player exits.
async fn wait_pcm_running(status: Option<&Path>, session: &mut Session) -> Result<Instant> {
    let Some(path) = status else {
        return Ok(Instant::now());
    };
    let deadline = Instant::now() + PCM_START_TIMEOUT;
    loop {
        match tokio::fs::read_to_string(path).await {
            Ok(s) if s.starts_with("state: RUNNING") => return Ok(Instant::now()),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("speaker status {}: {e}", path.display());
                return Ok(Instant::now());
            }
        }
        if let Sink::Process { child, .. } = &mut session.sink
            && let Some(st) = child.try_wait()?
        {
            return Err(Error::Other(format!(
                "player exited before the speaker started: {st}"
            )));
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                "speaker not running after {PCM_START_TIMEOUT:?}, powering the amp anyway"
            );
            return Ok(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The speaker's and the microphones' hardware clocks (Echo Dot), measured.
struct Clocks {
    spk: crate::clock::PcmClock,
    mic: crate::clock::PcmClock,
    last: Instant,
    /// Deviation from nominal, e.g. 3.2e-4 = +320 ppm (kept across sessions).
    d_spk: f64,
    d_mic: f64,
    logged: Instant,
}

impl Clocks {
    fn new() -> Self {
        Self {
            spk: crate::clock::PcmClock::new("/proc/asound/card0/pcm23p/sub0/status", 48_000.0),
            mic: crate::clock::PcmClock::new("/proc/asound/card0/pcm24c/sub0/status", 16_000.0),
            last: Instant::now(),
            d_spk: 0.0,
            d_mic: 0.0,
            logged: Instant::now(),
        }
    }

    /// Sample both clocks (every 200 ms) and update the estimates.
    fn tick(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.last) < Duration::from_millis(200) {
            return false;
        }
        self.last = now;
        self.spk.sample(now);
        self.mic.sample(now);
        let mut changed = false;
        if let Some(d) = self.spk.deviation() {
            self.d_spk = d;
            changed = true;
        }
        if let Some(d) = self.mic.deviation() {
            self.d_mic = d;
            changed = true;
        }
        if changed && now.saturating_duration_since(self.logged) > Duration::from_secs(60) {
            self.logged = now;
            tracing::info!(
                "audio clocks: speaker {:+.0} ppm, microphones {:+.0} ppm",
                self.d_spk * 1e6,
                self.d_mic * 1e6
            );
        }
        changed
    }

    /// Reference decimation step: speaker-clock input per microphone-clock output.
    fn ref_step(&self) -> f64 {
        3.0 * (1.0 + self.d_spk) / (1.0 + self.d_mic)
    }
}

struct Actor {
    cfg: SpeakerConfig,
    clocks: Option<Clocks>,
    rx: mpsc::UnboundedReceiver<Cmd>,
    state: watch::Sender<PlaybackState>,
    media_state: watch::Sender<MediaState>,
    volume: Arc<AtomicU8>,
    voice_gain: Arc<AtomicU8>,
    held_duck: Arc<AtomicBool>,
    session_open: Arc<AtomicBool>,
    feeder: Feeder,
    /// Clips fully handed to the player, completing at the given frame.
    finishing: VecDeque<(u64, oneshot::Sender<Result<()>>)>,
    session: Option<Session>,
    idle_since: Instant,
    analyzer: Analyzer,
    spectrum: Spectrum,
    level: VoiceLevel,
}

/// Player command line.
pub fn playback_command(cfg: &SpeakerConfig) -> Option<(String, Vec<String>)> {
    match cfg.backend {
        Backend::EchoDot | Backend::EchoDotPipeWire => Some((
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
            let target = cfg
                .pw_target
                .clone()
                .or_else(|| cfg.backend.pw_playback_target().map(String::from));
            if let Some(t) = target {
                a.push("--target".into());
                a.push(t);
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

    fn publish_media(&self) {
        let lane = &self.feeder.media;
        self.media_state.send_if_modified(|cur| lane.publish(cur));
    }

    fn handle(&mut self, cmd: Cmd) {
        let media = &mut self.feeder.media;
        match cmd {
            Cmd::MediaBegin { gen_, url } => media.begin(gen_, url),
            Cmd::MediaAttach { gen_, decoder } => media.attach(gen_, decoder),
            Cmd::MediaFail { gen_, msg } => media.open_failed(gen_, msg),
            Cmd::MediaStop { gen_ } => media.stop(gen_),
            Cmd::MediaPause => media.pause(),
            Cmd::MediaResume => media.resume(),
            Cmd::MediaMute(m) => media.set_muted(m),
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
            self.publish_media();
            if self.session.is_none() {
                if !self.feeder.has_audio() && !self.feeder.media.wants_session() {
                    self.set_state(PlaybackState::Idle);
                    match self.rx.recv().await {
                        Some(c) => self.handle(c),
                        None => return,
                    }
                    continue;
                }
                self.set_state(if self.feeder.has_audio() {
                    PlaybackState::Opening
                } else {
                    PlaybackState::Idle
                });
                match self.open_session().await {
                    Ok(s) => {
                        self.session = Some(s);
                        self.session_open.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::error!("speaker open failed: {e}");
                        self.feeder.fail_all(&format!("speaker open failed: {e}"));
                        self.feeder.media.fail(&format!("speaker open failed: {e}"));
                        self.set_state(PlaybackState::Idle);
                        continue;
                    }
                }
            }

            // Session open: pace writes, complete clips, idle out.
            let s = self.session.as_ref().unwrap();
            let now = Instant::now();
            let voice_busy = self.feeder.has_audio() || !self.finishing.is_empty();
            let busy = voice_busy || self.feeder.media.wants_session();
            if voice_busy {
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
            if let Some(c) = self.clocks.as_mut()
                && c.tick(now)
                && let Some(s) = self.session.as_mut()
            {
                s.set_rate(f64::from(OUT_RATE) * (1.0 + c.d_spk));
                if let Some(r) = s.reference.as_mut() {
                    r.set_step(c.ref_step());
                }
            }
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
            if self.feeder.has_audio()
                || !self.finishing.is_empty()
                || self.feeder.media.wants_session()
            {
                self.idle_since = now;
            } else if now >= self.idle_since + self.cfg.idle_timeout {
                self.close_session().await;
                continue;
            }

            let s = self.session.as_mut().unwrap();
            if s.time_of(s.frames_written).saturating_duration_since(now) > self.cfg.lead {
                continue; // not due yet
            }
            let gain = volume_gain(self.volume.load(Ordering::Relaxed))
                * 10f32.powf(self.cfg.pre_gain_db / 20.0);
            self.feeder
                .voice_gain
                .set(f32::from(self.voice_gain.load(Ordering::Relaxed)) / 100.0);
            self.feeder.held_duck = self.held_duck.load(Ordering::Relaxed);
            let ended = self.feeder.next_chunk(CHUNK_FRAMES, gain, &mut chunk);
            let base = s.frames_written;
            if self.feeder.media.phase() == MediaPhase::Playing {
                if gain <= 0.0 {
                    self.feeder.media_mono.fill(0.0); // volume 0: nothing sounds
                }
                let bands = self.analyzer.process(&self.feeder.media_mono);
                self.spectrum.push(s.time_of(base), bands);
            }
            if let Err(e) = s.write(&chunk).await {
                tracing::error!("speaker write failed: {e}");
                self.feeder.fail_all("speaker process died");
                self.feeder.media.fail("speaker process died");
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
            if let Some(rms) = self.feeder.voice_rms {
                self.level.push(s.time_of(base), rms);
            }
            for e in ended {
                self.finishing.push_back((base + e.frame as u64, e.done));
            }
        }
    }

    /// Mixer arm -> player process -> prime silence -> PCM running -> amp on
    /// -> unmute.
    async fn open_session(&mut self) -> Result<Session> {
        let echo = self.cfg.backend.echo_dot_mixer();
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
        let mut reference = self.cfg.backend.pw_reference_target().and_then(|t| {
            crate::refsink::RefTap::spawn(&self.cfg.pw_play_bin, t)
                .map_err(|e| tracing::warn!("echo reference to {t}: {e}"))
                .ok()
        });
        if let Some(c) = self.clocks.as_mut() {
            c.spk.reset();
            if let Some(r) = reference.as_mut() {
                r.set_step(c.ref_step());
            }
        }
        let rate = f64::from(OUT_RATE) * (1.0 + self.clocks.as_ref().map_or(0.0, |c| c.d_spk));
        let mut session = Session {
            sink,
            anchor_t: Instant::now(),
            anchor_frame: 0,
            rate,
            frames_written: 0,
            eq: echo.then(crate::refsink::SpeakerEq::new),
            reference,
        };
        let result: Result<()> = async {
            // Pre-roll: twice the lead of silence. The player starts the PCM
            // once its buffer (128 ms on the Dot) is full, and the rest plays
            // while the amp powers up.
            session.start_clock(Instant::now());
            let silence = vec![0u8; CHUNK_FRAMES * 4];
            let prime_chunks = (self.cfg.lead.as_millis() as usize * 2).div_ceil(20).max(1);
            for _ in 0..prime_chunks {
                session.write(&silence).await?;
            }
            if echo {
                // The codec powers up while the player prepares the PCM. An
                // amp that is already on plays that as a crack.
                let started =
                    wait_pcm_running(self.cfg.pcm_status.as_deref(), &mut session).await?;
                session.start_clock(started);
                self.cfg.mixer.apply(&mixer::playback_power()).await?;
                tokio::time::sleep(AMP_SETTLE).await;
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
            let echo = self.cfg.backend.echo_dot_mixer();
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
    fn feeder_measures_voice_before_gain() {
        let mut f = Feeder::default();
        let mut out = Vec::new();
        f.next_chunk(4, 1.0, &mut out);
        assert_eq!(f.voice_rms, None, "no voice queued");
        // A square wave of +-3000 has an RMS of 3000, whatever the volume.
        let (c, _rx) = clip(vec![3000, -3000, -3000, 3000, 3000, -3000]);
        f.queue.push_back(c);
        f.next_chunk(8, 0.1, &mut out);
        assert_eq!(f.voice_rms, Some(3000.0), "only the 3 voice frames count");
        f.next_chunk(4, 0.1, &mut out);
        assert_eq!(f.voice_rms, None, "the clip ended");
    }

    #[test]
    fn voice_level_is_what_sounds_now() {
        let level = VoiceLevel::default();
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        level.push(ms(0), 100.0);
        level.push(ms(20), 200.0);
        level.push(ms(40), 300.0);
        assert_eq!(
            level.at(t0.checked_sub(Duration::from_millis(1)).unwrap()),
            None
        );
        assert_eq!(level.at(ms(5)), Some(100.0));
        assert_eq!(
            level.at(ms(25)),
            Some(200.0),
            "a later chunk is not heard yet"
        );
        assert_eq!(level.at(ms(90)), Some(300.0));
        assert_eq!(level.at(ms(100)), None, "stale after the last chunk");
        for i in 0..100 {
            level.push(ms(100 + i * 20), 1.0);
        }
        assert_eq!(level.0.lock().unwrap().len(), LEVEL_HISTORY);
    }

    // -- two-lane mixing ---------------------------------------------------

    use crate::media::{Item, MediaPhase};
    use std::sync::mpsc::{SyncSender, sync_channel};

    /// A feeder whose media lane plays `chunks` of constant `level` stereo
    /// (960 frames each); the sender stays open (no end of stream).
    fn media_feeder(duck_db: f32, level: i16, chunks: usize) -> (Feeder, SyncSender<Item>) {
        let mut f = Feeder::new(duck_db);
        f.gain = 1.0;
        f.media.begin(1, "test".into());
        let (tx, rx) = sync_channel(chunks + 4);
        for _ in 0..chunks {
            tx.send(Item::Data(vec![level; 1920])).unwrap();
        }
        f.media.attach(1, Decoder::detached(rx));
        (f, tx)
    }

    /// Left-channel samples of a chunk.
    fn left(f: &mut Feeder, frames: usize) -> Vec<i16> {
        let mut out = Vec::new();
        f.next_chunk(frames, 1.0, &mut out);
        decode_chunk(&out).chunks_exact(2).map(|s| s[0]).collect()
    }

    fn ms(n: usize) -> usize {
        n * 48
    }

    #[test]
    fn mixes_both_lanes_and_saturates() {
        let (mut f, _tx) = media_feeder(0.0, 500, 2);
        let (c, _rx) = clip(vec![1000; 8]); // 4 frames
        f.queue.push_back(c);
        let l = left(&mut f, 6);
        assert_eq!(l, [1500, 1500, 1500, 1500, 500, 500]);

        let (mut f, _tx) = media_feeder(0.0, 20000, 2);
        let (c, _rx) = clip(vec![20000; 8]);
        f.queue.push_back(c);
        assert_eq!(
            left(&mut f, 5),
            [i16::MAX, i16::MAX, i16::MAX, i16::MAX, 20000]
        );
        let (c, _rx) = clip(vec![-30000; 4]);
        f.queue.push_back(c);
        f.media.set_muted(true);
        f.media.set_muted(false);
        // Negative side saturates too: -30000 + 20000*mute-ramp stays in range.
        assert!(left(&mut f, 2).iter().all(|&s| s > i16::MIN));
    }

    #[test]
    fn voice_gain_scales_only_the_voice_lane() {
        let (mut f, _tx) = media_feeder(0.0, 1000, 1);
        f.voice_gain.jump(0.5);
        let (c, _rx) = clip(vec![2000; 4]);
        f.queue.push_back(c);
        assert_eq!(left(&mut f, 3), [2000, 2000, 1000]);
    }

    #[test]
    fn master_gain_applies_after_the_mix() {
        let (mut f, _tx) = media_feeder(0.0, 1000, 1);
        let (c, _rx) = clip(vec![1000; 2]);
        f.queue.push_back(c);
        let mut out = Vec::new();
        f.gain = 0.5;
        f.next_chunk(1, 0.5, &mut out);
        assert_eq!(decode_chunk(&out)[0], 1000);
    }

    #[test]
    fn media_ducks_under_voice_holds_then_releases() {
        // 1.4 s of media at 10000; a silent 400 ms voice clip makes the
        // media level directly readable (voice contributes zero).
        let (mut f, _tx) = media_feeder(-20.0, 10000, 70);
        let (c, _rx) = clip(vec![0; ms(400) * 2]);
        f.queue.push_back(c);
        let l = left(&mut f, ms(1400));
        let at = |t_ms: usize| f64::from(l[ms(t_ms)]);
        // Attack: ~linear 1.0 -> 0.1 over 150 ms.
        assert!((at(75) - 5500.0).abs() < 150.0, "{}", at(75));
        assert!((at(150) - 1000.0).abs() < 50.0, "{}", at(150));
        assert!((at(390) - 1000.0).abs() < 50.0);
        // Voice ends at 400 ms; held through 700 ms.
        assert!((at(690) - 1000.0).abs() < 50.0, "{}", at(690));
        // Release 500 ms from 700 ms: halfway at 950 ms, clear at 1200 ms.
        assert!((at(950) - 5500.0).abs() < 200.0, "{}", at(950));
        assert!(at(1220) >= 9990.0, "{}", at(1220));
        // Monotonic: no clicks.
        assert!(
            l.windows(2)
                .all(|w| (i32::from(w[1]) - i32::from(w[0])).abs() < 30)
        );
    }

    #[test]
    fn held_duck_is_independent_of_voice() {
        let (mut f, _tx) = media_feeder(-20.0, 10000, 80);
        f.held_duck = true;
        let l = left(&mut f, ms(500));
        assert!((f64::from(l[ms(200)]) - 1000.0).abs() < 50.0);
        // Voice comes and goes while held: still ducked afterwards.
        let (c, _rx) = clip(vec![0; ms(100) * 2]);
        f.queue.push_back(c);
        let l = left(&mut f, ms(600));
        assert!((f64::from(l[ms(550)]) - 1000.0).abs() < 50.0);
        // Released: back to full after 500 ms.
        f.held_duck = false;
        let l = left(&mut f, ms(100));
        assert!(l[ms(99)] > 1000);
        let l = left(&mut f, ms(100));
        assert!(l[ms(99)] > 3000);
    }

    #[test]
    fn pause_fades_stops_draining_and_resumes() {
        let (mut f, _tx) = media_feeder(0.0, 10000, 10);
        assert_eq!(left(&mut f, 4), [10000; 4]);
        f.media.pause();
        let l = left(&mut f, ms(40));
        assert!(l[0] < 10000 && l[ms(30)] == 0, "fades to silence");
        assert!(l.windows(2).all(|w| w[1] <= w[0]));
        assert_eq!(f.media.phase(), MediaPhase::Paused);
        let drained = |f: &Feeder| f.media.position_for_test();
        let before = drained(&f);
        assert!(left(&mut f, ms(200)).iter().all(|&s| s == 0));
        assert_eq!(drained(&f), before, "paused lane must not drain");
        f.media.resume();
        let l = left(&mut f, ms(40));
        assert!(l.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(l[ms(30)], 10000);
        assert_eq!(f.media.phase(), MediaPhase::Playing);
    }

    #[test]
    fn mute_ramps_and_natural_end_goes_idle() {
        let (mut f, tx) = media_feeder(0.0, 10000, 1);
        f.media.set_muted(true);
        let l = left(&mut f, 100);
        assert!(l[0] < 10000 && l[0] > l[99] && l[99] > 0, "ramped: {l:?}");
        tx.send(Item::End(None)).unwrap();
        // The queued chunk plays out, then the lane goes idle with no error.
        left(&mut f, 960);
        assert_eq!(f.media.phase(), MediaPhase::Idle);
        let mut st = crate::media::MediaState::default();
        f.media.publish(&mut st);
        assert_eq!((st.phase, st.error.clone()), (MediaPhase::Idle, None));
        // Failure carries the reason.
        let (mut f, tx) = media_feeder(0.0, 10000, 0);
        tx.send(Item::End(Some("boom".into()))).unwrap();
        left(&mut f, 4);
        let mut st = crate::media::MediaState::default();
        assert!(f.media.publish(&mut st));
        assert_eq!(st.error.as_deref(), Some("boom"));
        assert_eq!(st.phase, MediaPhase::Idle);
    }

    #[test]
    fn starved_lane_reports_buffering_and_stays_silent() {
        let (mut f, tx) = media_feeder(0.0, 10000, 1);
        left(&mut f, 960);
        assert!(left(&mut f, 10).iter().all(|&s| s == 0));
        assert_eq!(f.media.phase(), MediaPhase::Buffering);
        tx.send(Item::Data(vec![7; 1920])).unwrap();
        assert_eq!(left(&mut f, 2), [7, 7]);
        assert_eq!(f.media.phase(), MediaPhase::Playing);
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

    /// An Echo Dot speaker on a fake amixer (logs its args to `log`), a fake
    /// aplay (swallows stdin) and a PCM status file that reads `state`.
    fn fake_echodot(tag: &str, state: &str) -> (std::path::PathBuf, SpeakerConfig) {
        let dir = std::env::temp_dir().join(format!("pinna-fakespk-{tag}-{}", std::process::id()));
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
        std::fs::write(dir.join("status"), format!("state: {state}\n")).unwrap();
        let mut cfg = SpeakerConfig::new(Backend::EchoDot);
        cfg.mixer.bin = amixer.to_string_lossy().into();
        cfg.aplay_bin = aplay.to_string_lossy().into();
        cfg.pcm_status = Some(dir.join("status"));
        cfg.idle_timeout = Duration::from_millis(200);
        (dir, cfg)
    }

    #[tokio::test]
    async fn mixer_sequence_runs_in_order_on_echodot() {
        let (dir, cfg) = fake_echodot("seq", "RUNNING");
        let log = dir.join("log");
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

    #[tokio::test]
    async fn amp_waits_for_the_running_pcm() {
        let (dir, cfg) = fake_echodot("wait", "SETUP");
        let (log, status) = (dir.join("log"), dir.join("status"));
        let sp = Speaker::new(cfg);
        let prepare = async {
            // The codec is still powering up: the amp must stay off.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let before = std::fs::read_to_string(&log).unwrap_or_default();
            std::fs::write(&status, "state: RUNNING\n").unwrap();
            before
        };
        let (played, before) =
            tokio::join!(sp.play_pcm(crate::tone::beep(440.0, 60, 0.5)), prepare);
        played.unwrap();
        assert!(
            before.contains("name=Ext_Speaker_Amp_Switch Off"),
            "{before}"
        );
        assert!(
            !before.contains("name=Ext_Speaker_Amp_Switch On"),
            "{before}"
        );
        assert!(!before.contains("name=MFP Gpio Mute Off"), "{before}");
        let after = std::fs::read_to_string(&log).unwrap();
        assert!(after.contains("name=Ext_Speaker_Amp_Switch On"), "{after}");
        assert!(after.contains("name=MFP Gpio Mute Off"), "{after}");
        drop(sp);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
