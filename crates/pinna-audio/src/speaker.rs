//! Speaker output as a long-lived session.
//!
//! Opening the speaker from idle takes ~1.4 s on the Dot (mixer arm, PCM open,
//! amp power-up), so one `aplay` stays open while a turn is active. Silence is
//! written while idle (no underrun pops) and the session is closed after
//! [`SpeakerConfig::idle_timeout`] without audio. On the Echo Dot it is never
//! closed: every amp power-up cracks, whatever the codec and mute state.
//! Volume is applied digitally in this pipeline using LibreEcho's
//! curve; the codec stays at its 0 dB reference.
//!
//! The session mixes two lanes: the *voice* lane (queued in order: clips such
//! as chimes and the alarm, and streamed URLs such as replies and
//! announcements, which sound as they download) and the *media* lane (a
//! streamed URL, see [`crate::media`]). Media is ducked under the voice lane
//! and while [`Speaker::duck`] is held.

use crate::chain::LOOKAHEAD;
use crate::clock::{PointerAnchor, monotonic_now, parse_avail_min, parse_boundary, parse_pointer};
use crate::decode::{OUT_RATE, Pcm};
use crate::media::{
    Decoder, MediaHandle, MediaLane, MediaPhase, MediaState, Pull, VoiceSource, open_voice,
    open_voice_stream,
};
use crate::mix::{Ducker, FADE, Ramp};
use crate::mixer::{self, AMP_SETTLE, MixerRunner, SpeakerProfile};
use crate::spectrum::{Analyzer, Spectrum};
use crate::timed::Timing;
use crate::track::Track;
use crate::upsample::{GROUP_DELAY, Upsampler2x};
use crate::volume::{VolumeCurve, curve_gain, curve_stream_gain, top_boost_db};
use crate::{Backend, Error, Result, decode};
use bytes::Bytes;
use futures_util::stream::BoxStream;
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch};

/// Frames per write (20 ms at 48 kHz).
const CHUNK_FRAMES: usize = 960;

/// Gets each written chunk's mean square (full scale is 1) and the time it is
/// heard (ADR 0036).
type OutputLevelSink = Arc<dyn Fn(Instant, f32) + Send + Sync>;

static OUTPUT_LEVEL_SINK: OnceLock<OutputLevelSink> = OnceLock::new();
static OUTPUT_LEVEL_ON: AtomicBool = AtomicBool::new(false);

/// Install the process-wide sink for the speaker's output level. Only the
/// first call counts. Nothing is computed until [`report_output_level`] turns
/// it on.
pub fn install_output_level(sink: Arc<dyn Fn(Instant, f32) + Send + Sync>) {
    let _ = OUTPUT_LEVEL_SINK.set(sink);
}

/// Turn the output level on or off. While it is off the write loop pays one
/// relaxed atomic load per chunk and computes nothing.
pub fn report_output_level(on: bool) {
    OUTPUT_LEVEL_ON.store(on, Ordering::Relaxed);
}

/// The mean square over all samples of an interleaved S16_LE chunk, full
/// scale being 1.
fn chunk_mean_square(bytes: &[u8]) -> f32 {
    let samples = bytes.len() / 2;
    if samples == 0 {
        return 0.0;
    }
    let sum: f64 = bytes
        .chunks_exact(2)
        .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])).powi(2))
        .sum();
    (sum / samples as f64 / (32768.0 * 32768.0)) as f32
}

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
    /// Close the session after this long without audio. `None` opens it at
    /// startup and keeps it open (the Echo Dot: its amp cracks at every
    /// power-up).
    pub idle_timeout: Option<Duration>,
    /// Logical volume 0..=100 at startup.
    pub initial_volume: u8,
    /// How far ahead of real time audio is handed to the player.
    pub lead: Duration,
    /// Media level while ducked, in dB (<= 0).
    pub duck_db: f32,
    /// Fixed gain on top of the volume curve, in dB (the speaker's
    /// loudness calibration).
    pub pre_gain_db: f32,
    /// Extra gain at the top of the volume range ([`top_boost_db`]), applied
    /// in the Echo Dot speaker chain, whose limiter keeps it under full scale.
    pub volume_boost: bool,
    /// How the volume becomes a gain (the board's).
    pub volume_curve: VolumeCurve,
    /// The playback PCM's `/proc/asound` status file (Echo Dot). The amp is
    /// powered only once it reads `RUNNING`.
    pub pcm_status: Option<PathBuf>,
    /// Play a mono mix: the board's speaker plays one channel only. Applied in
    /// the Echo Dot speaker chain.
    pub mono: bool,
    /// The playback PCM's frame rate: [`OUT_RATE`], or twice it, which is
    /// upsampled to at the end of the chain (the Echo Dot (5th gen)'s PCM is
    /// fixed at 96 kHz). The player's `-r`; the PipeWire player stays at
    /// [`OUT_RATE`].
    pub device_rate: u32,
}

/// `/proc/asound/cardC/pcmD{p|c}/sub0/status` of the ALSA device `hw:C,D`.
pub fn pcm_status_path(device: &str, capture: bool) -> Option<PathBuf> {
    let (card, dev) = device.strip_prefix("hw:")?.split_once(',')?;
    let (card, dev) = (card.parse::<u32>().ok()?, dev.parse::<u32>().ok()?);
    let dir = if capture { 'c' } else { 'p' };
    Some(format!("/proc/asound/card{card}/pcm{dev}{dir}/sub0/status").into())
}

/// Frames the player's PCM takes for each frame of the session, if the rate
/// is one the speaker chain can reach.
fn rate_factor(device_rate: u32) -> Option<u64> {
    match device_rate {
        r if r == OUT_RATE => Some(1),
        r if r == 2 * OUT_RATE => Some(2),
        _ => None,
    }
}

impl SpeakerConfig {
    pub fn new(backend: Backend) -> Self {
        let idle_timeout = (!backend.echo_dot_mixer()).then_some(Duration::from_secs(10));
        SpeakerConfig {
            backend,
            profile: SpeakerProfile::default(),
            mixer: MixerRunner::default(),
            aplay_bin: "aplay".into(),
            alsa_device: "hw:0,23".into(),
            pw_play_bin: "pw-play".into(),
            pw_target: None,
            idle_timeout,
            initial_volume: 50,
            lead: Duration::from_millis(100),
            duck_db: -20.0,
            pre_gain_db: 0.0,
            volume_boost: false,
            volume_curve: VolumeCurve::LibreEcho,
            pcm_status: Some("/proc/asound/card0/pcm23p/sub0/status".into()),
            mono: false,
            device_rate: OUT_RATE,
        }
    }
}

/// How long the player may take to start the PCM before the amp is powered
/// anyway (it takes ~1.2 s on the Dot).
const PCM_START_TIMEOUT: Duration = Duration::from_secs(3);

/// The player's buffer where the PCM doesn't bound it to this itself: the
/// MT8163's, so the session's prime of twice the lead starts it.
const PLAYER_BUFFER_MS: u64 = 128;

/// How long a session that is kept open waits after a failed open before it
/// tries again.
const OPEN_RETRY: Duration = Duration::from_secs(5);

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

/// A URL streaming on the voice lane.
pub(crate) struct StreamItem {
    src: VoiceSource,
    cur: Vec<i16>,
    pos: usize,
    /// Taken when its first frame is mixed.
    started: Option<oneshot::Sender<()>>,
    done: oneshot::Sender<Result<()>>,
    /// [`Inner::voice_epoch`] when it was requested.
    epoch: u64,
    /// Frames waited since it last sounded (once started).
    waiting: u64,
    /// Frames of silence in stalls it came back from.
    stalled: u64,
}

pub(crate) enum VoiceItem {
    Clip(Clip),
    Stream(StreamItem),
}

impl VoiceItem {
    fn complete(self, r: Result<()>) {
        let done = match self {
            VoiceItem::Clip(c) => c.done,
            VoiceItem::Stream(s) => s.done,
        };
        let _ = done.send(r);
    }
}

/// One frame of the voice lane's front item.
enum Step {
    /// A frame; `last` if it was the item's final one.
    Sound { l: f32, r: f32, last: bool },
    /// A stream with nothing to sound yet.
    Wait,
    /// A stream is over (before this frame).
    End(Result<()>),
}

impl Clip {
    fn next_frame(&mut self) -> Step {
        let (l, r) = (
            f32::from(self.samples[self.pos]),
            f32::from(self.samples[self.pos + 1]),
        );
        self.pos += 2;
        Step::Sound {
            l,
            r,
            last: self.pos + 1 >= self.samples.len(),
        }
    }
}

impl StreamItem {
    fn next_frame(&mut self) -> Step {
        if self.pos + 1 >= self.cur.len() {
            match self.src.pull() {
                Pull::Chunk(c) => {
                    self.cur = c;
                    self.pos = 0;
                    self.stalled += std::mem::take(&mut self.waiting);
                }
                Pull::Wait => {
                    if self.started.is_none() {
                        self.waiting += 1;
                    }
                    return Step::Wait;
                }
                Pull::End(None) => return Step::End(Ok(())),
                Pull::End(Some(e)) => return Step::End(Err(Error::Other(e))),
            }
        }
        let (l, r) = (
            f32::from(self.cur[self.pos]),
            f32::from(self.cur[self.pos + 1]),
        );
        self.pos += 2;
        Step::Sound { l, r, last: false }
    }
}

/// Something that happens when the frame it is marked at has sounded.
enum Event {
    /// A stream's first frame.
    Started(oneshot::Sender<()>),
    /// An item's end, with how it went.
    Done(oneshot::Sender<Result<()>>, Result<()>),
}

impl Event {
    fn fire(self) {
        match self {
            Event::Started(tx) => {
                let _ = tx.send(());
            }
            Event::Done(tx, r) => {
                let _ = tx.send(r);
            }
        }
    }

    /// Stopped before it sounded: an end completes `Ok` (as a stopped clip
    /// does), a start never happened.
    fn cancel(self) {
        if let Event::Done(tx, _) = self {
            let _ = tx.send(Ok(()));
        }
    }

    fn fail(self, msg: &str) {
        if let Event::Done(tx, _) = self {
            let _ = tx.send(Err(Error::Other(msg.to_string())));
        }
    }
}

/// An [`Event`] inside a chunk, at `frame` (frames into the chunk).
struct Mark {
    frame: usize,
    ev: Event,
}

/// An encoded stream playing on the voice lane ([`Speaker::play_url`] or [`Speaker::play_stream`]).
pub struct VoicePlayback {
    /// Resolves when its first audio sounds; errs if it never does (stopped
    /// first, or the speaker failed).
    pub started: oneshot::Receiver<()>,
    /// Resolves when its last frame has sounded (`Ok` too when
    /// [`Speaker::stop`] cancelled it), or with why it ended early.
    pub done: oneshot::Receiver<Result<()>>,
}

pub(crate) enum Cmd {
    Play(Clip),
    PlayStream(StreamItem),
    /// Open the session ahead of audio that is about to come.
    Warm,
    Stop,
    MediaBegin {
        gen_: u64,
        url: String,
    },
    /// A live stream from the source its header names begins.
    StreamBegin {
        gen_: u64,
        source: String,
    },
    /// A timed source begins (ADR 0027).
    TimedBegin {
        gen_: u64,
        source: String,
        decoder: Decoder,
    },
    MediaAttach {
        gen_: u64,
        decoder: Decoder,
    },
    MediaFail {
        gen_: u64,
        msg: String,
    },
    MediaStop {
        gen_: u64,
    },
    MediaPause,
    MediaResume,
    MediaMute(bool),
    /// What the play `gen_` names (its tags or ICY metadata).
    MediaTrack {
        gen_: u64,
        track: Track,
    },
    /// What a stream bridge names.
    StreamTrack {
        source: String,
        track: Track,
    },
}

struct Inner {
    tx: mpsc::UnboundedSender<Cmd>,
    volume: Arc<AtomicU8>,
    voice_gain: Arc<AtomicU8>,
    stream_volume: Arc<AtomicU8>,
    held_duck: Arc<AtomicBool>,
    media: MediaHandle,
    spectrum: Spectrum,
    state: watch::Receiver<PlaybackState>,
    session_open: Arc<AtomicBool>,
    level: VoiceLevel,
    /// Bumped by every [`Speaker::stop`]: a stream requested before it is
    /// dropped even if it reaches the actor after the stop.
    voice_epoch: Arc<AtomicU64>,
    volume_curve: VolumeCurve,
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
        let stream_volume = Arc::new(AtomicU8::new(100));
        let held_duck = Arc::new(AtomicBool::new(false));
        let (media_tx, media_rx) = watch::channel(MediaState::default());
        let media = MediaHandle::new(tx.clone(), media_rx);
        let spectrum = Spectrum::default();
        let level = VoiceLevel::default();
        let voice_epoch = Arc::new(AtomicU64::new(0));
        let volume_curve = cfg.volume_curve;
        let actor = Actor {
            feeder: Feeder::new(cfg.duck_db),
            // Only the Echo Dot's own sound card has a speaker clock to track.
            clocks: cfg.backend.echo_dot_mixer().then(|| Clocks::new(&cfg)),
            cfg,
            rx,
            state: state_tx,
            media_state: media_tx,
            volume: volume.clone(),
            voice_gain: voice_gain.clone(),
            stream_volume: stream_volume.clone(),
            held_duck: held_duck.clone(),
            session_open: session_open.clone(),
            finishing: VecDeque::new(),
            session: None,
            idle_since: Instant::now(),
            analyzer: Analyzer::new(OUT_RATE),
            spectrum: spectrum.clone(),
            level: level.clone(),
            warm: false,
            open_at: Instant::now(),
            voice_epoch: voice_epoch.clone(),
        };
        tokio::spawn(actor.run());
        Speaker {
            inner: Arc::new(Inner {
                tx,
                volume,
                voice_gain,
                stream_volume,
                held_duck,
                media,
                spectrum,
                state: state_rx,
                session_open,
                level,
                voice_epoch,
                volume_curve,
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

    /// Stream `url` (http, https, `file://` or a plain path; wav, mp3, flac or
    /// ogg) on the voice lane, behind anything queued there. It sounds as it
    /// downloads, and waits in silence if the download falls behind (nothing
    /// is skipped). Resolves once the first audio is decoded; errs if the URL
    /// cannot be opened or holds no audio, and then nothing is queued.
    pub async fn play_url(&self, url: &str) -> Result<VoicePlayback> {
        let epoch = self.inner.voice_epoch.load(Ordering::SeqCst);
        let src = open_voice(self.inner.media.client(), url).await?;
        self.queue_voice(epoch, src)
    }

    /// Stream encoded audio on the voice lane, behind anything queued there.
    /// `mime` is an optional content-type hint. Resolves once the first audio
    /// is decoded; an empty or invalid stream errs and queues nothing.
    pub async fn play_stream(
        &self,
        mime: Option<&str>,
        body: BoxStream<'static, std::io::Result<Bytes>>,
    ) -> Result<VoicePlayback> {
        let epoch = self.inner.voice_epoch.load(Ordering::SeqCst);
        let src = open_voice_stream(mime, body).await?;
        self.queue_voice(epoch, src)
    }

    /// Queue an opened source on the voice lane. `epoch` is the lane's when
    /// opening began: a stop meanwhile drops it.
    fn queue_voice(&self, epoch: u64, src: VoiceSource) -> Result<VoicePlayback> {
        let (started_tx, started) = oneshot::channel();
        let (done_tx, done) = oneshot::channel();
        self.inner
            .tx
            .send(Cmd::PlayStream(StreamItem {
                src,
                cur: Vec::new(),
                pos: 0,
                started: Some(started_tx),
                done: done_tx,
                epoch,
                waiting: 0,
                stalled: 0,
            }))
            .map_err(|_| Error::Other("speaker task ended".into()))?;
        Ok(VoicePlayback { started, done })
    }

    /// Open the session now, if it is closed, for audio that is about to come
    /// (opening takes ~1.4 s on the Dot). Without any, it closes again after
    /// [`SpeakerConfig::idle_timeout`]. A no-op when the session is kept open.
    pub fn warm(&self) {
        let _ = self.inner.tx.send(Cmd::Warm);
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

    /// The linear gain of a volume 0..=100 on this speaker's curve.
    pub fn volume_gain(&self, percent: u8) -> f32 {
        curve_gain(self.inner.volume_curve, percent)
    }

    /// Reply volume 0..=100: scales only the voice lane (default 100), on top
    /// of the master volume.
    pub fn set_voice_gain(&self, percent: u8) {
        self.inner
            .voice_gain
            .store(percent.min(100), Ordering::Relaxed);
    }

    /// Live stream volume 0..=100 (the sender's volume, ADR 0011): scales
    /// the media lane while it plays a stream, on top of the master volume.
    /// Other media plays at full level. Kept across sessions; each bridge
    /// sets its own before it connects.
    pub fn set_stream_volume(&self, percent: u8) {
        self.inner
            .stream_volume
            .store(percent.min(100), Ordering::Relaxed);
    }

    /// The track the stream bridge `source` plays, as it names it (ADR
    /// 0011). Kept across sessions, and shown in the media state only while
    /// a stream from `source` plays.
    pub fn set_stream_track(&self, source: &str, track: Track) {
        let _ = self.inner.tx.send(Cmd::StreamTrack {
            source: source.to_string(),
            track,
        });
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
        self.inner.voice_epoch.fetch_add(1, Ordering::SeqCst);
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
// Feeder: queued voice items -> volume-scaled s16le stereo chunks (pure, testable)

struct Feeder {
    queue: VecDeque<VoiceItem>,
    /// Master gain as of the last chunk.
    gain: f32,
    voice_gain: Ramp,
    held_duck: bool,
    ducker: Ducker,
    media: MediaLane,
    /// The last chunk's media lane as mono (before ducking and gain).
    media_mono: Vec<f32>,
    /// RMS of the voice samples in the last chunk (before any gain); `None`
    /// when the chunk had no voice, `Some(0.0)` when a started stream was
    /// waiting for data (a pause in speech, not the end of it).
    voice_rms: Option<f32>,
}

impl Default for Feeder {
    fn default() -> Self {
        Feeder::new(-20.0)
    }
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
    ///
    /// A stream at the front with nothing to sound yet holds the lane: its
    /// frames are silent (the items behind it wait), and media stays ducked.
    fn next_chunk(&mut self, frames: usize, target_gain: f32, out: &mut Vec<u8>) -> Vec<Mark> {
        out.clear();
        self.media_mono.clear();
        let mut marks = Vec::new();
        let start_gain = self.gain;
        let (mut voice_power, mut voice_samples) = (0.0f64, 0u32);
        let mut stalled = false;
        for f in 0..frames {
            let mut voice = None;
            let mut waiting = false;
            loop {
                let step = match self.queue.front_mut() {
                    None => break,
                    Some(VoiceItem::Clip(c)) => c.next_frame(),
                    Some(VoiceItem::Stream(s)) => {
                        let step = s.next_frame();
                        match step {
                            Step::Sound { .. } => {
                                if let Some(tx) = s.started.take() {
                                    marks.push(Mark {
                                        frame: f,
                                        ev: Event::Started(tx),
                                    });
                                }
                            }
                            // A pause in speech once it has started.
                            Step::Wait => stalled |= s.started.is_none(),
                            Step::End(_) => {}
                        }
                        step
                    }
                };
                match step {
                    Step::Sound { l, r, last } => {
                        voice = Some((l, r));
                        if last {
                            let VoiceItem::Clip(c) = self.queue.pop_front().unwrap() else {
                                unreachable!("only clips know their last frame")
                            };
                            marks.push(Mark {
                                frame: f + 1,
                                ev: Event::Done(c.done, Ok(())),
                            });
                        }
                        break;
                    }
                    Step::Wait => {
                        waiting = true;
                        break;
                    }
                    Step::End(r) => {
                        let VoiceItem::Stream(s) = self.queue.pop_front().unwrap() else {
                            unreachable!("only streams end on their own")
                        };
                        if s.src.stalls() > 0 {
                            tracing::info!(
                                "voice stream: fell behind {} time(s), {:.1} s of silence",
                                s.src.stalls(),
                                s.stalled as f64 / f64::from(OUT_RATE)
                            );
                        }
                        marks.push(Mark {
                            frame: f,
                            ev: Event::Done(s.done, r),
                        });
                        // The next item may sound in this same frame.
                    }
                }
            }
            if let Some((l, r)) = voice {
                voice_power += f64::from(l * l) + f64::from(r * r);
                voice_samples += 2;
            }
            let duck = self.ducker.tick(voice.is_some() || waiting, self.held_duck);
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
        self.voice_rms = if voice_samples > 0 {
            Some((voice_power / f64::from(voice_samples)).sqrt() as f32)
        } else {
            stalled.then_some(0.0)
        };
        marks
    }

    /// Drop everything queued; dropping a stream closes its download.
    fn cancel_all(&mut self) {
        for item in self.queue.drain(..) {
            item.complete(Ok(()));
        }
    }

    fn fail_all(&mut self, msg: &str) {
        for item in self.queue.drain(..) {
            item.complete(Err(Error::Other(msg.to_string())));
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
    /// Echo Dot speaker chain (the device's own speaker only).
    chain: Option<crate::chain::SpeakerChain>,
    /// Doubles the rate at the very end, for a PCM at twice [`OUT_RATE`].
    /// Everything else here counts session frames, at [`OUT_RATE`].
    upsampler: Option<Upsampler2x>,
    /// Corrects `anchor_t` from the playback PCM's hardware pointer (the
    /// Echo backends, ADR 0027 §10).
    pointer: Option<PointerAnchor>,
    /// From the time a frame passes the hardware pointer to the time it is
    /// heard: the speaker chain's look-ahead and the model's output delay.
    heard_delay: Duration,
}

/// `a - b` in seconds, negative when `a` is earlier.
fn signed_secs(a: Instant, b: Instant) -> f64 {
    if a >= b {
        (a - b).as_secs_f64()
    } else {
        -(b - a).as_secs_f64()
    }
}

impl Session {
    /// When frame `frame` of what the session is given is heard (ADR 0027
    /// §10).
    fn heard_at(&self, frame: u64) -> Instant {
        self.time_of(frame) + self.heard_delay
    }

    /// Whether [`heard_at`](Self::heard_at) follows the hardware pointer.
    fn exact(&self) -> bool {
        self.pointer.as_ref().is_some_and(PointerAnchor::exact)
    }

    /// Frames the PCM counts for each session frame.
    fn factor(&self) -> u64 {
        if self.upsampler.is_some() { 2 } else { 1 }
    }

    /// The pipe into the player.
    fn player_fd(&self) -> Option<RawFd> {
        match &self.sink {
            Sink::Process { stdin, .. } => Some(stdin.as_raw_fd()),
            Sink::Null => None,
        }
    }

    /// Move the frame-to-time mapping `secs` later (earlier if negative).
    fn shift(&mut self, secs: f64) {
        let d = Duration::from_secs_f64(secs.abs());
        self.anchor_t = if secs >= 0.0 {
            self.anchor_t + d
        } else {
            self.anchor_t.checked_sub(d).unwrap_or(self.anchor_t)
        };
    }

    /// Correct the mapping from a speaker status sample. `mono_now` and
    /// `now` were read together: they convert the PCM's monotonic timestamps
    /// into [`Instant`]s. `unread`: the frames still in the player's pipe as
    /// the sample was read, if known.
    fn follow_pointer(
        &mut self,
        status: &str,
        sw_params: &Path,
        mono_now: f64,
        now: Instant,
        unread: Option<u64>,
    ) {
        let Some(mut sample) = parse_pointer(status) else {
            return;
        };
        // The PCM counts its own frames; the anchor counts the session's.
        let factor = self.factor();
        sample.hw_ptr /= factor;
        sample.appl_ptr = sample.appl_ptr.map(|a| a / factor);
        let written = self.frames_written;
        let Some(p) = self.pointer.as_mut() else {
            return;
        };
        if sample.running && p.needs_boundary() {
            // Read once: without it, a wrap counts as a restart, and a
            // restart is never identified exactly.
            let text = std::fs::read_to_string(sw_params).ok();
            p.set_boundary(text.as_deref().and_then(parse_boundary).map(|b| b / factor));
            p.set_player_chunk(
                text.as_deref()
                    .and_then(parse_avail_min)
                    .map(|c| c / factor),
            );
        }
        // `time_of`, in monotonic seconds.
        let anchor = mono_now + signed_secs(self.anchor_t, now);
        let (anchor_frame, rate) = (self.anchor_frame as f64, self.rate);
        let predicted = |f: u64| anchor + (f as f64 - anchor_frame) / rate;
        if let Some(secs) = p.update(&sample, mono_now, written, unread, predicted) {
            self.shift(secs);
        }
    }

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

    /// `boost` is the speaker chain's linear gain (see [`SpeakerConfig::volume_boost`]).
    async fn write(&mut self, bytes: &[u8], boost: f32) -> std::io::Result<()> {
        let chain_buf;
        let bytes = match &mut self.chain {
            Some(chain) => {
                let mut b = bytes.to_vec();
                chain.process(&mut b, boost);
                chain_buf = b;
                &chain_buf[..]
            }
            None => bytes,
        };
        let frames = (bytes.len() / 4) as u64;
        if let Sink::Process { stdin, .. } = &mut self.sink {
            match &mut self.upsampler {
                Some(up) => {
                    let mut doubled = Vec::new();
                    up.process(bytes, &mut doubled);
                    stdin.write_all(&doubled).await?;
                }
                None => stdin.write_all(bytes).await?,
            }
        } else if let Some(up) = &mut self.upsampler {
            // Keep the filter's state moving as it would with a player.
            up.process(bytes, &mut Vec::new());
        }
        self.frames_written += frames;
        Ok(())
    }
}

/// Session frames written into the pipe `fd` and not yet read from it, by
/// `FIONREAD`; `None` if it can't be asked. The pipe holds the player's
/// 16-bit stereo frames, `factor` of them for each session frame (the
/// upsampler's).
fn pipe_unread_frames(fd: RawFd, factor: u64) -> Option<u64> {
    let mut bytes: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int through the pointer, which is valid
    // and writable for the duration of the call.
    let r = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut bytes) };
    (r == 0)
        .then(|| u64::try_from(bytes).ok())
        .flatten()
        .map(|b| b / 4 / factor)
}

/// Sleep until `t`, or forever without one.
async fn sleep_until(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => std::future::pending().await,
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

static UNDERRUNS: AtomicU64 = AtomicU64::new(0);

/// Underruns the player reported since the process started.
pub fn underruns() -> u64 {
    UNDERRUNS.load(Ordering::Relaxed)
}

/// Log the player's messages. aplay reports an underrun as `underrun!!! (at
/// least N ms long)`, even with `-q`, and restarts the PCM, which on the Echo
/// Dot switches the amp off and on again (an audible crack).
async fn log_player(stderr: ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.contains("underrun") {
            UNDERRUNS.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("speaker {line}");
        } else {
            tracing::debug!("speaker player: {line}");
        }
    }
}

/// The speaker's hardware clock (Echo Dot), measured. The microphones' clock
/// needs no fitting: the echo canceller's reference comes in their own frames
/// (ADR 0004).
struct Clocks {
    /// Frames the speaker PCM counts for each session frame.
    factor: u64,
    spk: crate::clock::PcmClock,
    last: Instant,
    /// Deviation from nominal, e.g. 3.2e-4 = +320 ppm (kept across sessions).
    d_spk: f64,
    logged: Instant,
    /// The speaker's latest status, with `CLOCK_MONOTONIC` and the
    /// [`Instant`] read together after it, and the frames in the player's
    /// pipe as it was read (for [`Session::follow_pointer`]).
    spk_status: Option<(String, f64, Instant, Option<u64>)>,
}

impl Clocks {
    fn new(cfg: &SpeakerConfig) -> Self {
        let spk_status = cfg
            .pcm_status
            .clone()
            .unwrap_or_else(|| "/proc/asound/card0/pcm23p/sub0/status".into());
        Self {
            factor: rate_factor(cfg.device_rate).unwrap_or(1),
            spk: crate::clock::PcmClock::new(spk_status, f64::from(cfg.device_rate)),
            last: Instant::now(),
            d_spk: 0.0,
            logged: Instant::now(),
            spk_status: None,
        }
    }

    /// The speaker PCM's `sw_params` (its pointer boundary), next to its status.
    fn spk_sw_params(&self) -> PathBuf {
        self.spk.path().with_file_name("sw_params")
    }

    /// Sample the clock (every 200 ms) and update the estimate. `player` is
    /// the pipe into the speaker's player.
    fn tick(&mut self, now: Instant, player: Option<RawFd>) -> bool {
        if now.saturating_duration_since(self.last) < Duration::from_millis(200) {
            return false;
        }
        self.last = now;
        // The pipe is read on both sides of the status: if the player took
        // from it in between, the two can't be matched.
        let factor = self.factor;
        let before = player.and_then(|fd| pipe_unread_frames(fd, factor));
        let status = self.spk.sample_text(now);
        let after = player.and_then(|fd| pipe_unread_frames(fd, factor));
        let unread = before.filter(|&b| after == Some(b));
        self.spk_status = status.map(|text| (text, monotonic_now(), Instant::now(), unread));
        let Some(d) = self.spk.deviation() else {
            return false;
        };
        self.d_spk = d;
        if now.saturating_duration_since(self.logged) > Duration::from_secs(60) {
            self.logged = now;
            tracing::info!("speaker clock: {:+.0} ppm", self.d_spk * 1e6);
        }
        true
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
    stream_volume: Arc<AtomicU8>,
    held_duck: Arc<AtomicBool>,
    session_open: Arc<AtomicBool>,
    feeder: Feeder,
    /// Events handed to the player, firing once the given frame sounds.
    finishing: VecDeque<(u64, Event)>,
    session: Option<Session>,
    idle_since: Instant,
    analyzer: Analyzer,
    spectrum: Spectrum,
    level: VoiceLevel,
    /// Open the session even without audio ([`Speaker::warm`]).
    warm: bool,
    /// When a session that is kept open ([`SpeakerConfig::idle_timeout`] is
    /// `None`) is (re)opened: at startup, and [`OPEN_RETRY`] after a failure.
    open_at: Instant,
    voice_epoch: Arc<AtomicU64>,
}

/// Player command line.
pub fn playback_command(cfg: &SpeakerConfig) -> Option<(String, Vec<String>)> {
    let rate = cfg.device_rate.to_string();
    match cfg.backend {
        Backend::EchoDot | Backend::EchoDotPipeWire => {
            let mut a: Vec<String> = [
                "-D",
                &cfg.alsa_device,
                "-f",
                "S16_LE",
                "-c",
                "2",
                "-r",
                &rate,
                "-t",
                "raw",
                "-q",
            ]
            .map(String::from)
            .to_vec();
            // aplay starts the PCM once its buffer is full, and the session
            // primes it with twice the lead of silence before it waits for
            // that. The MT8163's PCM caps the buffer at 128 ms itself. The
            // MT8519's DL2 lets aplay take its 500 ms default (buffer_size
            // 48000 at 96 kHz, read on a C2N6L4, 2026-11-26): the prime
            // never filled it, every session waited out PCM_START_TIMEOUT,
            // and the speaker played half a second behind. A rate above
            // OUT_RATE gets the MT8163's 128 ms, in four periods.
            if cfg.device_rate != OUT_RATE {
                let buffer = u64::from(cfg.device_rate) * PLAYER_BUFFER_MS / 1000;
                a.push(format!("--buffer-size={buffer}"));
                a.push(format!("--period-size={}", buffer / 4));
            }
            Some((cfg.aplay_bin.clone(), a))
        }
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
            Cmd::StreamBegin { gen_, source } => media.begin_stream(gen_, source),
            Cmd::TimedBegin {
                gen_,
                source,
                decoder,
            } => media.begin_timed(gen_, source, decoder),
            Cmd::MediaAttach { gen_, decoder } => media.attach(gen_, decoder),
            Cmd::MediaFail { gen_, msg } => media.open_failed(gen_, msg),
            Cmd::MediaStop { gen_ } => media.stop(gen_),
            Cmd::MediaPause => media.pause(),
            Cmd::MediaResume => media.resume(),
            Cmd::MediaMute(m) => media.set_muted(m),
            Cmd::MediaTrack { gen_, track } => media.set_track(gen_, track),
            Cmd::StreamTrack { source, track } => media.set_stream_track(source, track),
            Cmd::Play(clip) => {
                if clip.samples.len() < 2 {
                    let _ = clip.done.send(Ok(()));
                } else {
                    self.feeder.queue.push_back(VoiceItem::Clip(clip));
                }
            }
            Cmd::PlayStream(item) => {
                if item.epoch == self.voice_epoch.load(Ordering::SeqCst) {
                    self.feeder.queue.push_back(VoiceItem::Stream(item));
                } else {
                    // Requested before a stop that overtook it.
                    let _ = item.done.send(Ok(()));
                }
            }
            Cmd::Warm => {
                self.warm = self.session.is_none();
                self.idle_since = Instant::now();
            }
            Cmd::Stop => {
                self.feeder.cancel_all();
                for (_, ev) in self.finishing.drain(..) {
                    ev.cancel();
                }
            }
        }
    }

    async fn run(mut self) {
        let mut chunk = Vec::with_capacity(CHUNK_FRAMES * 4);
        loop {
            self.publish_media();
            if self.session.is_none() {
                let keep_open = self.cfg.idle_timeout.is_none().then_some(self.open_at);
                let due = keep_open.is_some_and(|t| Instant::now() >= t);
                if !self.feeder.has_audio()
                    && !self.feeder.media.wants_session()
                    && !self.warm
                    && !due
                {
                    self.set_state(PlaybackState::Idle);
                    tokio::select! {
                        cmd = self.rx.recv() => match cmd {
                            Some(c) => self.handle(c),
                            None => return,
                        },
                        _ = sleep_until(keep_open) => {}
                    }
                    continue;
                }
                self.set_state(if self.feeder.has_audio() {
                    PlaybackState::Opening
                } else {
                    PlaybackState::Idle
                });
                self.warm = false;
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
                        self.open_at = Instant::now() + OPEN_RETRY;
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
            if let Some(idle) = self.cfg.idle_timeout
                && !busy
            {
                wake = wake.min(self.idle_since + idle);
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
            let player = self.session.as_ref().and_then(Session::player_fd);
            if let Some(c) = self.clocks.as_mut()
                && c.tick(now, player)
                && let Some(s) = self.session.as_mut()
            {
                s.set_rate(f64::from(OUT_RATE) * (1.0 + c.d_spk));
            }
            if let Some(c) = self.clocks.as_mut()
                && let Some((status, mono, at, unread)) = c.spk_status.take()
                && let Some(s) = self.session.as_mut()
            {
                s.follow_pointer(&status, &c.spk_sw_params(), mono, at, unread);
            }
            // Complete clips whose last frame has sounded.
            while let Some((end, _)) = self.finishing.front() {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.time_of(*end) <= now)
                {
                    let (_, ev) = self.finishing.pop_front().unwrap();
                    ev.fire();
                } else {
                    break;
                }
            }
            if self.feeder.has_audio()
                || !self.finishing.is_empty()
                || self.feeder.media.wants_session()
            {
                self.idle_since = now;
            } else if self
                .cfg
                .idle_timeout
                .is_some_and(|idle| now >= self.idle_since + idle)
            {
                self.close_session().await;
                continue;
            }

            let s = self.session.as_mut().unwrap();
            if s.time_of(s.frames_written).saturating_duration_since(now) > self.cfg.lead {
                continue; // not due yet
            }
            let volume = self.volume.load(Ordering::Relaxed);
            let gain =
                curve_gain(self.cfg.volume_curve, volume) * 10f32.powf(self.cfg.pre_gain_db / 20.0);
            let boost = if self.cfg.volume_boost {
                10f32.powf(top_boost_db(volume) / 20.0)
            } else {
                1.0
            };
            self.feeder
                .voice_gain
                .set(f32::from(self.voice_gain.load(Ordering::Relaxed)) / 100.0);
            self.feeder.media.set_stream_gain(curve_stream_gain(
                self.cfg.volume_curve,
                self.stream_volume.load(Ordering::Relaxed),
            ));
            self.feeder.held_duck = self.held_duck.load(Ordering::Relaxed);
            // A timed source reads the audio due when this chunk is heard.
            self.feeder.media.set_timing(
                Timing {
                    first: s.heard_at(s.frames_written),
                    rate: s.rate,
                    exact: s.exact(),
                },
                CHUNK_FRAMES,
            );
            let marks = self.feeder.next_chunk(CHUNK_FRAMES, gain, &mut chunk);
            let base = s.frames_written;
            if OUTPUT_LEVEL_ON.load(Ordering::Relaxed)
                && let Some(sink) = OUTPUT_LEVEL_SINK.get()
            {
                sink(s.heard_at(base), chunk_mean_square(&chunk));
            }
            if self.feeder.media.phase() == MediaPhase::Playing {
                if gain <= 0.0 {
                    self.feeder.media_mono.fill(0.0); // volume 0: nothing sounds
                }
                let bands = self.analyzer.process(&self.feeder.media_mono);
                self.spectrum.push(s.time_of(base), bands);
            }
            if let Err(e) = s.write(&chunk, boost).await {
                tracing::error!("speaker write failed: {e}");
                self.feeder.fail_all("speaker process died");
                self.feeder.media.fail("speaker process died");
                for m in marks {
                    m.ev.fail("speaker process died");
                }
                for (_, ev) in self.finishing.drain(..) {
                    ev.fail("speaker process died");
                }
                self.close_session().await;
                continue;
            }
            if let Some(rms) = self.feeder.voice_rms {
                self.level.push(s.time_of(base), rms);
            }
            for m in marks {
                self.finishing.push_back((base + m.frame as u64, m.ev));
            }
        }
    }

    /// Mixer arm -> player process -> prime silence -> PCM running -> amp on
    /// -> unmute.
    async fn open_session(&mut self) -> Result<Session> {
        let echo = self.cfg.backend.echo_dot_mixer();
        // Only the ALSA player runs at the device's rate.
        let factor = if echo {
            rate_factor(self.cfg.device_rate).ok_or_else(|| {
                Error::Other(format!(
                    "the speaker's rate {} Hz must be {OUT_RATE} Hz or {} Hz",
                    self.cfg.device_rate,
                    2 * OUT_RATE
                ))
            })?
        } else {
            1
        };
        if echo
            && let Err(e) = self
                .cfg
                .mixer
                .apply(&mixer::playback_arm(&self.cfg.profile))
                .await
        {
            let _ = self
                .cfg
                .mixer
                .apply(&mixer::playback_disable(self.cfg.profile.codec))
                .await;
            return Err(e);
        }
        let sink = match playback_command(&self.cfg) {
            None => Sink::Null,
            Some((bin, args)) => {
                let mut child = Command::new(&bin)
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| Error::Other(format!("spawn {bin}: {e}")))?;
                let stdin = child.stdin.take().expect("piped");
                tokio::spawn(log_player(child.stderr.take().expect("piped")));
                Sink::Process {
                    child: Box::new(child),
                    stdin,
                }
            }
        };
        if let Some(c) = self.clocks.as_mut() {
            c.spk.reset();
        }
        let rate = f64::from(OUT_RATE) * (1.0 + self.clocks.as_ref().map_or(0.0, |c| c.d_spk));
        let mut session = Session {
            sink,
            anchor_t: Instant::now(),
            anchor_frame: 0,
            rate,
            frames_written: 0,
            chain: echo.then(|| crate::chain::SpeakerChain::new(self.cfg.mono)),
            upsampler: (factor == 2).then(Upsampler2x::new),
            // The PCM just opened: its pointer counts this session's frames.
            pointer: self
                .clocks
                .is_some()
                .then(|| PointerAnchor::new(f64::from(OUT_RATE))),
            heard_delay: self.cfg.profile.output_delay
                + if echo {
                    Duration::from_secs_f64(LOOKAHEAD as f64 / f64::from(OUT_RATE))
                } else {
                    Duration::ZERO
                }
                // The upsampler's filter delays the signal after the
                // pointer's frame counting.
                + if factor == 2 {
                    Duration::from_secs_f64(GROUP_DELAY as f64 / f64::from(self.cfg.device_rate))
                } else {
                    Duration::ZERO
                },
        };
        let result: Result<()> = async {
            // Pre-roll: twice the lead of silence. The player starts the PCM
            // once its buffer (128 ms on the Dot) is full, and the rest plays
            // while the amp powers up.
            session.start_clock(Instant::now());
            let silence = vec![0u8; CHUNK_FRAMES * 4];
            let prime_chunks = (self.cfg.lead.as_millis() as usize * 2).div_ceil(20).max(1);
            for _ in 0..prime_chunks {
                session.write(&silence, 1.0).await?;
            }
            if echo {
                // The codec powers up while the player prepares the PCM. An
                // amp that is already on plays that as a crack.
                let started =
                    wait_pcm_running(self.cfg.pcm_status.as_deref(), &mut session).await?;
                session.start_clock(started);
                self.cfg
                    .mixer
                    .apply(&mixer::playback_power(self.cfg.profile.codec))
                    .await?;
                tokio::time::sleep(AMP_SETTLE).await;
                self.cfg
                    .mixer
                    .apply(&mixer::playback_unmute(self.cfg.profile.codec))
                    .await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            Self::close(
                session,
                echo.then_some((&self.cfg.mixer, self.cfg.profile.codec)),
            )
            .await;
            return Err(e);
        }
        self.idle_since = Instant::now();
        Ok(session)
    }

    async fn close_session(&mut self) {
        if let Some(s) = self.session.take() {
            let echo = self.cfg.backend.echo_dot_mixer();
            Self::close(s, echo.then_some((&self.cfg.mixer, self.cfg.profile.codec))).await;
        }
        self.session_open.store(false, Ordering::Relaxed);
    }

    /// Mute and cut the amp first (no shutdown pop), then end the player.
    async fn close(session: Session, mixer: Option<(&MixerRunner, mixer::Codec)>) {
        if let Some((m, codec)) = mixer
            && let Err(e) = m.apply(&mixer::playback_disable(codec)).await
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
    use futures_util::{StreamExt, stream};

    #[test]
    fn chunk_mean_square_is_full_scale_one() {
        let bytes =
            |samples: &[i16]| -> Vec<u8> { samples.iter().flat_map(|s| s.to_le_bytes()).collect() };
        assert_eq!(chunk_mean_square(&[]), 0.0);
        assert_eq!(chunk_mean_square(&bytes(&[0, 0, 0, 0])), 0.0);
        // Full scale in every sample is 1; half the samples silent, a half.
        assert_eq!(chunk_mean_square(&bytes(&[i16::MIN, i16::MIN])), 1.0);
        assert_eq!(chunk_mean_square(&bytes(&[i16::MIN, 0, i16::MIN, 0])), 0.5);
    }

    /// The Echo output plays S16_LE stereo at [`OUT_RATE`] or twice it (the
    /// upsampler's), and its clock reads the board's playback PCM's status
    /// file. A board with another playback PCM (ADR 0032) fails here until
    /// the speaker chain reaches its rate.
    #[test]
    fn every_boards_playback_pcm_is_one_the_speaker_can_play() {
        for m in pinna_board::Model::ALL {
            let play = m.board().audio.playback;
            assert_eq!(play.format, pinna_board::SampleFormat::S16Le, "{m:?}");
            assert_eq!(play.channels, 2, "{m:?}");
            assert!(
                play.rate == OUT_RATE || play.rate == 2 * OUT_RATE,
                "{m:?}: {} Hz",
                play.rate
            );
            assert!(rate_factor(play.rate).is_some(), "{m:?}");
            assert!(pcm_status_path(play.device, false).is_some(), "{m:?}");
        }
    }

    #[test]
    fn a_pcm_device_names_its_status_file() {
        let path = |d, c| pcm_status_path(d, c).unwrap();
        assert_eq!(
            path("hw:0,23", false),
            Path::new("/proc/asound/card0/pcm23p/sub0/status")
        );
        assert_eq!(
            path("hw:0,1", false),
            Path::new("/proc/asound/card0/pcm1p/sub0/status")
        );
        assert_eq!(
            path("hw:0,9", true),
            Path::new("/proc/asound/card0/pcm9c/sub0/status")
        );
        assert_eq!(pcm_status_path("default", false), None);
        assert_eq!(pcm_status_path("hw:0", false), None);
        let cfg = SpeakerConfig::new(Backend::EchoDot);
        assert_eq!(cfg.pcm_status, Some(path("hw:0,23", false)));
    }

    #[test]
    fn only_the_session_rate_and_twice_it_are_reachable() {
        assert_eq!(rate_factor(48_000), Some(1));
        assert_eq!(rate_factor(96_000), Some(2));
        assert_eq!(rate_factor(44_100), None);
        assert_eq!(rate_factor(192_000), None);
    }

    fn clip(samples: Vec<i16>) -> (VoiceItem, oneshot::Receiver<Result<()>>) {
        let (done, rx) = oneshot::channel();
        (
            VoiceItem::Clip(Clip {
                samples,
                pos: 0,
                done,
            }),
            rx,
        )
    }

    /// A voice stream fed by hand: 20 ms chunks of `level` (see `send`).
    struct TestStream {
        tx: SyncSender<Item>,
        started: oneshot::Receiver<()>,
        done: oneshot::Receiver<Result<()>>,
    }

    impl TestStream {
        fn send(&self, level: i16, chunks: usize) {
            for _ in 0..chunks {
                self.tx
                    .send(Item::Data(vec![level; CHUNK_FRAMES * 2]))
                    .unwrap();
            }
        }

        fn end(&self, err: Option<&str>) {
            self.tx.send(Item::End(err.map(String::from))).unwrap();
        }
    }

    fn stream() -> (VoiceItem, TestStream) {
        let (tx, rx) = sync_channel(100);
        let (started_tx, started) = oneshot::channel();
        let (done_tx, done) = oneshot::channel();
        let item = VoiceItem::Stream(StreamItem {
            src: VoiceSource::new(Decoder::detached(rx)),
            cur: Vec::new(),
            pos: 0,
            started: Some(started_tx),
            done: done_tx,
            epoch: 0,
            waiting: 0,
            stalled: 0,
        });
        (item, TestStream { tx, started, done })
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
    fn stream_marks_its_first_and_last_frame_and_holds_the_queue() {
        let mut f = Feeder {
            gain: 1.0,
            ..Default::default()
        };
        let (item, mut ts) = stream();
        f.queue.push_back(item);
        let (c, _rx) = clip(vec![7; 4]); // behind the stream
        f.queue.push_back(c);
        let mut out = Vec::new();
        // Gathering: silent, nothing marked, the lane stays busy.
        ts.send(1000, 4);
        assert!(f.next_chunk(CHUNK_FRAMES, 1.0, &mut out).is_empty());
        assert!(decode_chunk(&out).iter().all(|&s| s == 0));
        assert!(f.has_audio());
        assert_eq!(f.voice_rms, None, "nothing has sounded yet");
        // The target is there: it starts at frame 0.
        ts.send(1000, 1);
        let m = f.next_chunk(CHUNK_FRAMES, 1.0, &mut out);
        assert!(matches!(
            m[..],
            [Mark {
                frame: 0,
                ev: Event::Started(_)
            }]
        ));
        assert_eq!(decode_chunk(&out)[0], 1000);
        for _ in 0..4 {
            assert!(f.next_chunk(CHUNK_FRAMES, 1.0, &mut out).is_empty());
        }
        // Ran dry (the last chunk faded): a stall, not the end; the clip waits.
        f.next_chunk(CHUNK_FRAMES, 1.0, &mut out);
        assert!(decode_chunk(&out).iter().all(|&s| s == 0));
        assert_eq!(f.voice_rms, Some(0.0), "a pause in speech");
        // It ends: done right after its last frame, then the clip plays.
        ts.end(None);
        let m = f.next_chunk(4, 1.0, &mut out);
        assert_eq!(m.len(), 2);
        assert!(matches!(
            m[0],
            Mark {
                frame: 0,
                ev: Event::Done(_, Ok(()))
            }
        ));
        assert!(matches!(
            m[1],
            Mark {
                frame: 2,
                ev: Event::Done(_, Ok(()))
            }
        ));
        assert_eq!(decode_chunk(&out), [7, 7, 7, 7, 0, 0, 0, 0]);
        assert!(!f.has_audio());
        assert!(ts.started.try_recv().is_err(), "the actor fires events");
        assert!(ts.done.try_recv().is_err());
    }

    #[test]
    fn stream_failing_mid_reply_reports_the_error() {
        let mut f = Feeder::default();
        let (item, ts) = stream();
        f.queue.push_back(item);
        ts.send(1000, 5);
        ts.end(Some("cut off"));
        let mut out = Vec::new();
        let mut marks = Vec::new();
        for _ in 0..6 {
            marks.extend(f.next_chunk(CHUNK_FRAMES, 1.0, &mut out));
        }
        let Some(Mark {
            ev: Event::Done(_, Err(e)),
            ..
        }) = marks.pop()
        else {
            panic!("expected a failed end");
        };
        assert!(e.to_string().contains("cut off"));
    }

    #[test]
    fn media_stays_ducked_while_a_stream_waits() {
        // 3 s of media at 10000; the stream is silent so the media level
        // reads directly.
        let (mut f, _tx) = media_feeder(-20.0, 10000, 150);
        let (item, ts) = stream();
        f.queue.push_back(item);
        // Ducked before the first audio...
        let l = left(&mut f, ms(200));
        assert!((f64::from(l[ms(190)]) - 1000.0).abs() < 50.0);
        // ...through 100 ms of speech and a 1 s stall...
        ts.send(0, 5);
        let l = left(&mut f, ms(1100));
        assert!(l.iter().skip(ms(10)).all(|&s| (900..=1100).contains(&s)));
        // ...and released after the end (hold 300 ms, release 500 ms).
        ts.end(None);
        let l = left(&mut f, ms(1000));
        assert!((f64::from(l[ms(200)]) - 1000.0).abs() < 50.0);
        assert!(l[ms(900)] >= 9990, "{}", l[ms(900)]);
    }

    #[test]
    fn cancel_drops_a_stream_and_its_download() {
        let mut f = Feeder::default();
        let (item, mut ts) = stream();
        f.queue.push_back(item);
        f.cancel_all();
        assert!(matches!(ts.done.try_recv(), Ok(Ok(()))));
        assert!(ts.tx.send(Item::End(None)).is_err(), "decoder dropped");
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

    /// A session without a player, whose frame 0 is mapped to `anchor`.
    fn null_session(anchor: Instant, written: u64, factor: u64) -> Session {
        Session {
            sink: Sink::Null,
            anchor_t: anchor,
            anchor_frame: 0,
            rate: 48_000.0,
            frames_written: written,
            chain: None,
            upsampler: (factor == 2).then(Upsampler2x::new),
            pointer: Some(PointerAnchor::new(48_000.0)),
            heard_delay: Duration::ZERO,
        }
    }

    #[test]
    fn the_session_follows_the_hardware_pointer() {
        // At the session's rate, and at twice it: the PCM counts device
        // frames, the session its own.
        for factor in [1u64, 2] {
            let now = Instant::now();
            let mono = 1000.0;
            let mut s = null_session(now - Duration::from_millis(300), 20_000, factor);
            // The pointer says session frame 14400 played 8 ms ago, on the
            // monotonic clock; the mapping put it at now.
            let status = |hw: u64, tstamp: f64| {
                format!(
                    "state: RUNNING\ntstamp      : {tstamp:.9}\nhw_ptr      : {}\nappl_ptr    : {}\n",
                    hw * factor,
                    (hw + 4800) * factor
                )
            };
            let none = Path::new("/nonexistent/sw_params");
            s.follow_pointer(&status(14_400, mono - 0.008), none, mono, now, Some(0));
            let err = signed_secs(s.time_of(14_400), now) + 0.008;
            assert!(err.abs() < 1e-6, "{factor}: {err}");
            // A stopped PCM or a closed one changes nothing.
            let before = s.anchor_t;
            s.follow_pointer(
                &status(14_400, mono).replace("RUNNING", "XRUN"),
                none,
                mono,
                now,
                Some(0),
            );
            s.follow_pointer("closed\n", none, mono, now, Some(0));
            assert_eq!(s.anchor_t, before);
            // Without a pointer (PipeWire), nothing either.
            s.pointer = None;
            s.follow_pointer(&status(14_400, mono - 0.5), none, mono, now, Some(0));
            assert_eq!(s.anchor_t, before);
        }
    }

    /// The boundary and the player's chunk are read from the PCM's own
    /// `sw_params`, in its frames, and the pointer wraps at the boundary.
    #[test]
    fn the_pointer_boundary_and_chunk_are_counted_in_session_frames() {
        let dir = std::env::temp_dir().join(format!("pinna-swparams-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sw = dir.join("sw_params");
        // 2^20 device frames: 2^19 session frames at twice the rate.
        std::fs::write(&sw, "avail_min: 1920\nboundary: 1048576\n").unwrap();
        let now = Instant::now();
        let mono = 1000.0;
        let mut s = null_session(now, 100_000, 2);
        let status = |hw: u64, tstamp: f64| {
            format!(
                "state: RUNNING\ntstamp      : {tstamp:.9}\nhw_ptr      : {hw}\nappl_ptr    : {hw}\n"
            )
        };
        s.follow_pointer(&status(2 * 50_000, mono), &sw, mono, now, Some(0));
        assert!(!s.pointer.as_ref().unwrap().needs_boundary());
        // Just short of the wrap, then past it: still one running count.
        s.follow_pointer(
            &status(1_048_000, mono + 0.1),
            &sw,
            mono + 0.1,
            now,
            Some(0),
        );
        s.follow_pointer(&status(2_000, mono + 0.2), &sw, mono + 0.2, now, Some(0));
        assert!(s.pointer.as_ref().unwrap().exact());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_frames_waiting_in_the_players_pipe_are_counted() {
        use std::io::{Read, Write};
        for factor in [1u64, 2] {
            let (mut rx, mut tx) = std::io::pipe().unwrap();
            let fd = tx.as_raw_fd();
            assert_eq!(pipe_unread_frames(fd, factor), Some(0));
            // The pipe holds the player's frames: `factor` for each of the
            // session's 960.
            tx.write_all(&vec![0; 4 * 960 * factor as usize]).unwrap();
            assert_eq!(pipe_unread_frames(fd, factor), Some(960));
            rx.read_exact(&mut vec![0; 4 * 100 * factor as usize])
                .unwrap();
            assert_eq!(pipe_unread_frames(fd, factor), Some(860));
            assert_eq!(pipe_unread_frames(-1, factor), None);
        }
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
        // The player runs at the device's rate; the PipeWire player doesn't.
        let mut cfg = SpeakerConfig::new(Backend::EchoDot);
        cfg.device_rate = 96_000;
        cfg.alsa_device = "hw:0,1".into();
        let (_, args) = playback_command(&cfg).unwrap();
        assert_eq!(
            args.join(" "),
            "-D hw:0,1 -f S16_LE -c 2 -r 96000 -t raw -q --buffer-size=12288 --period-size=3072"
        );
        // The session's prime (twice the lead) fills that buffer, so aplay
        // starts the PCM without waiting out PCM_START_TIMEOUT.
        assert!(Duration::from_millis(PLAYER_BUFFER_MS) < cfg.lead * 2);
        cfg.backend = Backend::PipeWire;
        let (_, args) = playback_command(&cfg).unwrap();
        assert_eq!(
            args.join(" "),
            "--raw --rate 48000 --channels 2 --format s16 -"
        );
    }

    fn null_speaker(idle: Duration) -> Speaker {
        let mut cfg = SpeakerConfig::new(Backend::WavFile("unused".into()));
        cfg.idle_timeout = Some(idle);
        cfg.initial_volume = 100;
        Speaker::new(cfg)
    }

    fn encoded_wav() -> Vec<u8> {
        let pcm = crate::tone::beep(440.0, 100, 0.5);
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(
                &mut bytes,
                hound::WavSpec {
                    channels: 2,
                    sample_rate: 48_000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for sample in pcm.samples {
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        bytes.into_inner()
    }

    #[tokio::test]
    async fn play_stream_wav_chunks_start_and_finish() {
        let sp = null_speaker(Duration::from_secs(5));
        let wav = encoded_wav();
        let first = wav.len() / 3;
        let second = first * 2;
        let chunks = [
            Bytes::from(wav[..first].to_vec()),
            Bytes::from(wav[first..second].to_vec()),
            Bytes::from(wav[second..].to_vec()),
        ];
        let body = stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)).boxed();
        let playback = sp
            .play_stream(Some("audio/x-wav; charset=binary"), body)
            .await
            .unwrap();
        let VoicePlayback { started, done } = playback;
        tokio::time::timeout(Duration::from_secs(3), started)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), done)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn play_stream_stop_while_opening_drops_the_item() {
        let sp = null_speaker(Duration::from_secs(5));
        let (tx, rx) = mpsc::unbounded_channel::<std::io::Result<Bytes>>();
        let body = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .boxed();
        let opening = {
            let sp = sp.clone();
            tokio::spawn(async move { sp.play_stream(None, body).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        sp.stop();
        tx.send(Ok(Bytes::from(encoded_wav()))).unwrap();
        drop(tx);
        let playback = tokio::time::timeout(Duration::from_secs(3), opening)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let VoicePlayback { started, done } = playback;
        assert!(
            tokio::time::timeout(Duration::from_secs(1), started)
                .await
                .unwrap()
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(1), done)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
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

    #[test]
    fn echo_dot_keeps_the_session_open() {
        for b in [Backend::EchoDot, Backend::EchoDotPipeWire] {
            assert_eq!(SpeakerConfig::new(b).idle_timeout, None);
        }
        assert_eq!(
            SpeakerConfig::new(Backend::PipeWire).idle_timeout,
            Some(Duration::from_secs(10))
        );
    }

    #[tokio::test]
    async fn kept_open_session_opens_at_startup_and_never_idles_out() {
        let mut cfg = SpeakerConfig::new(Backend::WavFile("unused".into()));
        cfg.idle_timeout = None;
        cfg.initial_volume = 100;
        let sp = Speaker::new(cfg);
        let opened = async {
            while !sp.is_session_open() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(2), opened)
            .await
            .expect("opens without audio");
        sp.play_pcm(crate::tone::beep(440.0, 60, 0.5))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(sp.is_session_open());
        assert!(!sp.is_playing());
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
        cfg.idle_timeout = Some(Duration::from_millis(200));
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

    /// The echo canceller reads what the speaker plays from the microphones'
    /// loopback lanes (ADR 0004): the Echo Dot's speaker starts no reference
    /// player of its own.
    #[tokio::test]
    async fn the_echo_dot_speaker_sends_no_echo_reference() {
        let (dir, mut cfg) = fake_echodot("noref", "RUNNING");
        let started = dir.join("pw-play-args");
        let pw_play = dir.join("pw-play");
        std::fs::write(
            &pw_play,
            format!(
                "#!/bin/sh\necho \"$@\" >> {}\ncat > /dev/null\n",
                started.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pw_play, std::fs::Permissions::from_mode(0o755)).unwrap();
        cfg.backend = Backend::EchoDotPipeWire;
        cfg.pw_play_bin = pw_play.to_string_lossy().into();
        let sp = Speaker::new(cfg);
        sp.play_pcm(crate::tone::beep(440.0, 200, 0.5))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !started.exists(),
            "pw-play started: {}",
            std::fs::read_to_string(&started).unwrap_or_default()
        );
        drop(sp);
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

    #[tokio::test]
    async fn a_rate_the_speaker_cannot_reach_fails_the_open() {
        let (dir, mut cfg) = fake_echodot("rate", "RUNNING");
        cfg.device_rate = 44_100;
        let sp = Speaker::new(cfg);
        let err = sp
            .play_pcm(crate::tone::beep(440.0, 60, 0.5))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("44100"), "{err}");
        drop(sp);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
