//! pinnad — the Pinna device daemon.
//!
//! Wires the hardware (LED ring, buttons, mic privacy latch, microphone array,
//! speaker), the on-device wake word detector and the ESPHome native API, so
//! Home Assistant adopts the device as a voice satellite.

mod airplay;
mod api;
mod assistant_api;
mod auth;
mod bluetooth;
mod boot;
mod calibration;
mod checks;
mod clock;
mod config;
mod group;
mod hotspot;
mod identity;
mod leds;
mod logs;
mod mcp;
mod metrics;
mod mic;
mod network;
mod people;
mod people_api;
mod recording;
mod satellites;
mod secrets;
mod sendspin;
mod settings;
mod standalone;
mod store;
mod timeline;
mod timezone;
mod tools;
mod updates;
mod virtual_device;
mod voice;
mod wake_level;
mod wakewords;
mod web;
mod wifi;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use pinna_audio::dsp::{CaptureFormat, SampleFormat};
use pinna_audio::mixer::Codec;
use pinna_audio::speaker::pcm_status_path;
use pinna_audio::volume::VolumeCurve;
use pinna_audio::{
    Agc, Backend, CaptureConfig, CaptureStatus, Speaker, SpeakerConfig, start_capture,
};
use pinna_esphome::{DeviceConfig, DeviceEvent, EsphomeHandle};
use pinna_hal::anim::Effect;
use pinna_hal::board::{Board, Model, Soc, SpeakerTuning};
use pinna_hal::led::{LedHandle, MockLeds, RING_PIXELS, Rgb};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::leds::LedState;
use crate::mic::MicRouter;
use crate::standalone::service::{Paths, Service};
use crate::standalone::spawn::ProdSpawner;
use crate::voice::{Brain, Input, LocalOptions, Options, Shared, Snapshot};
use crate::wake_level::WakeLevel;
use crate::wakewords::{Catalogue, KeywordSet};

/// musl's own malloc held on to the memory pinnad freed, so pinnad grew
/// without bound while the microphones were on. Measured on
/// the Echo (2nd gen) running 0.6.0-beta.2: with musl's malloc, RSS rose
/// 1.7 MB a minute while the live heap stayed at 6.7 MB, until the satellite
/// ran out of its 485 MB and hung. With mimalloc, RSS stayed between 46 and
/// 58 MB for an hour.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Repeatable: each file layers over the one before.
    #[arg(long, default_value = "/etc/pinna/pinnad.toml")]
    config: Vec<PathBuf>,
    /// Device-local configuration layered over `--config` (identity, overrides).
    #[arg(long, default_value = "/var/lib/pinna/pinnad.toml")]
    local_config: PathBuf,
    /// Runtime settings changed from Home Assistant (volume, sensitivity, ...).
    #[arg(long, default_value = "/var/lib/pinna/settings.toml")]
    settings: PathBuf,
    /// Read button presses from stdin (up/down/action/mute) — development.
    #[arg(long)]
    stdin_buttons: bool,
    /// Make the node name the hostname and exit (pinna-hostname.service, early
    /// in the boot; ADR 0010).
    #[arg(long)]
    set_hostname: bool,
    /// Test the wake word model in FILE and exit: print `ok` and exit 0, or
    /// the reason it is refused and exit 2. pinnad runs itself with this to
    /// test an uploaded model in a process of its own (ADR 0013).
    #[arg(long, value_name = "FILE", requires = "check_threshold")]
    check_wake_word: Option<PathBuf>,
    /// The wake threshold the test holds the model to.
    #[arg(long, value_name = "SCORE", requires = "check_wake_word")]
    check_threshold: Option<f32>,
}

/// The group that may write to the stream socket: the AirPlay and Bluetooth
/// bridges run with it (ADR 0011). Created in the image.
const STREAM_GROUP: &str = "pinna-stream";

/// The id of group `name` in an `/etc/group` text.
fn gid_in(groups: &str, name: &str) -> Option<u32> {
    groups.lines().find_map(|line| {
        let mut f = line.split(':');
        if f.next()? != name {
            return None;
        }
        f.nth(1)?.parse().ok()
    })
}

/// Hand the stream socket to [`STREAM_GROUP`] (it is 0660). Without the group
/// (a laptop, a test) it stays root's.
fn share_stream_socket(path: &std::path::Path) {
    let gid = std::fs::read_to_string("/etc/group")
        .ok()
        .and_then(|g| gid_in(&g, STREAM_GROUP));
    match gid {
        Some(gid) => {
            if let Err(e) = std::os::unix::fs::chown(path, None, Some(gid)) {
                tracing::warn!("stream socket to group {STREAM_GROUP}: {e}");
            }
        }
        None => tracing::debug!("no group {STREAM_GROUP}: the stream socket stays root's"),
    }
}

/// `text` as a sentence: a capital first, a full stop last.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    let first: String = chars
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();
    let rest = chars.as_str();
    if rest.ends_with('.') {
        format!("{first}{rest}")
    } else {
        format!("{first}{rest}.")
    }
}

/// Test an uploaded model for [`Cli::check_wake_word`]: print one line, and
/// return the exit code.
fn check_wake_word(cfg: &Config, file: &Path, threshold: f32) -> i32 {
    let result = std::fs::read(file)
        .map_err(|e| format!("The satellite couldn't read the model to test it: {e}."))
        .and_then(|bytes| {
            pinna_wakeword::test_keyword(&cfg.wake.models, &bytes, threshold)
                .map(|_| ())
                .map_err(|e| {
                    // The owner reads the reason; the inference engine's
                    // detail below it goes to the log (check.rs logs stderr).
                    eprintln!("{e:#}");
                    sentence(&e.to_string())
                })
        });
    match result {
        Ok(()) => {
            println!("ok");
            0
        }
        Err(reason) => {
            println!("{}", reason.replace('\n', " "));
            2
        }
    }
}

/// pinna-audio's mixer sequences for the board's codec (pinna-audio doesn't
/// depend on the board crate, ADR 0032).
fn audio_codec(codec: pinna_hal::board::Codec) -> Codec {
    match codec {
        pinna_hal::board::Codec::Mt8163Aic => Codec::Mt8163Aic,
        pinna_hal::board::Codec::Mt8519Max98396 => Codec::Mt8519Max98396,
    }
}

/// pinna-audio's volume curve for the board's (ADR 0032, as [`audio_codec`]).
fn volume_curve(curve: pinna_hal::board::VolumeCurve) -> VolumeCurve {
    match curve {
        pinna_hal::board::VolumeCurve::LibreEcho => VolumeCurve::LibreEcho,
        pinna_hal::board::VolumeCurve::Steps(steps) => VolumeCurve::Steps(steps),
    }
}

/// What the board's capture PCM delivers, for the microphone front end.
fn capture_format(board: &Board) -> Result<CaptureFormat> {
    let pcm = board.audio.capture;
    Ok(CaptureFormat {
        rate: pcm.rate,
        channels: pcm.channels,
        sample: match pcm.format {
            pinna_hal::board::SampleFormat::S24_3Le => SampleFormat::S24_3Le,
            pinna_hal::board::SampleFormat::S32Le => SampleFormat::S32Le,
            // The front end decodes only what a board's microphones deliver.
            pinna_hal::board::SampleFormat::S16Le => anyhow::bail!(
                "the {} captures S16_LE, which the microphone front end doesn't decode",
                board.model.name()
            ),
        },
        mic_lanes: (0..board.mics.count).collect(),
    })
}

/// The board whose drivers to use. Without a board description the Echo Dot
/// (2nd gen)'s stand in, which is right on an MT8163 (an idme id Pinna doesn't
/// know) and on hardware with no known SoC (a computer, the VM), where they
/// find nothing. On any other known SoC they would run the MT8163's mixer
/// sequences on the wrong card, so `Err` names the SoC and pinnad starts
/// without audio and ring drivers.
fn driver_board(board: Option<&'static Board>, soc: Option<Soc>) -> Result<&'static Board, Soc> {
    match (board, soc) {
        (Some(b), _) => Ok(b),
        (None, Some(soc)) if soc != Soc::Mt8163 => Err(soc),
        (None, _) => Ok(&pinna_hal::board::ECHO_DOT_2),
    }
}

/// The raw lanes as PipeWire's `dot_mics` hands them to `pw-record`: every
/// lane of the board's capture PCM as 24-bit samples at 16 kHz, the
/// microphones first. The graph's clock sets the rate, not the sound card's
/// (the Echo Dot (5th gen)'s PCM is 96 kHz, S32_LE).
fn raw_lanes_format(board: &Board) -> CaptureFormat {
    CaptureFormat {
        rate: pinna_audio::dsp::SAMPLE_RATE,
        channels: board.audio.capture.channels,
        sample: SampleFormat::S24_3Le,
        mic_lanes: (0..board.mics.count).collect(),
    }
}

/// `card` is the board's sound card id ([`pinna_hal::board::Audio`]).
fn audio_backend(spec: &str, card: &str) -> Option<Backend> {
    match spec {
        "none" => None,
        "echodot" => Some(Backend::EchoDot),
        "echodot-pipewire" => Some(Backend::EchoDotPipeWire),
        "pipewire" => Some(Backend::PipeWire),
        s if s.starts_with("wav:") => Some(Backend::WavFile(PathBuf::from(&s[4..]))),
        _ => {
            let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
            if cards.contains(card) {
                // The device's PipeWire graph (system service) when it runs,
                // otherwise the sound card directly.
                let socket = std::env::var_os("PIPEWIRE_RUNTIME_DIR")
                    .map_or_else(|| PathBuf::from("/run/pipewire"), PathBuf::from)
                    .join("pipewire-0");
                if socket.exists() {
                    Some(Backend::EchoDotPipeWire)
                } else {
                    Some(Backend::EchoDot)
                }
            } else if std::env::var_os("XDG_RUNTIME_DIR")
                .map(|d| PathBuf::from(d).join("pipewire-0").exists())
                .unwrap_or(false)
            {
                Some(Backend::PipeWire)
            } else {
                None
            }
        }
    }
}

fn led_ring(cfg: &config::Leds, board: &Board) -> Result<Option<LedHandle>> {
    match cfg.backend.as_str() {
        "none" => Ok(None),
        "mock" => Ok(Some(LedHandle::spawn(MockLeds::new()))),
        backend => match pinna_hal::led::open(board, cfg.brightness) {
            Ok(ring) => {
                tracing::info!("LED ring: {}", board.ring.driver.name());
                Ok(Some(LedHandle::spawn(ring)))
            }
            Err(e) if backend == "is31" => Err(e).context("LED ring"),
            Err(e) => {
                tracing::info!("no LED ring ({e})");
                Ok(None)
            }
        },
    }
}

/// Gain on the captured stream before the wake word and Home Assistant:
/// automatic (ADR 0004), or fixed when `capture_gain_db` is set.
enum CaptureGain {
    Auto {
        agc: Agc,
        /// Gain last logged, so that only changes of 3 dB or more are.
        logged: f32,
    },
    Fixed(f32),
}

impl CaptureGain {
    fn apply(&mut self, frame: &mut [i16]) {
        match self {
            CaptureGain::Auto { agc, logged } => {
                agc.process(frame);
                let g = agc.gain_db();
                if (g - *logged).abs() >= 3.0 {
                    tracing::debug!("capture gain {g:+.1} dB");
                    *logged = g;
                }
            }
            CaptureGain::Fixed(g) => apply_gain(frame, *g),
        }
    }
}

/// The gain in each wake word's measurement line (ADR 0026): the automatic
/// gain as it stands after the latest frame, or the fixed one.
impl CaptureGain {
    fn db(&self) -> f32 {
        match self {
            CaptureGain::Auto { agc, .. } => agc.gain_db(),
            CaptureGain::Fixed(g) => 20.0 * g.log10(),
        }
    }
}

impl std::fmt::Display for CaptureGain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureGain::Auto { .. } => write!(f, "automatic gain {:+.1} dB", self.db()),
            CaptureGain::Fixed(_) => write!(f, "fixed gain {:+.1} dB", self.db()),
        }
    }
}

fn frame_bytes(frame: &[i16]) -> Bytes {
    let mut out = Vec::with_capacity(frame.len() * 2);
    for s in frame {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Bytes::from(out)
}

/// Microphone → (wake word | Home Assistant). Keeps 200 ms of pre-roll so the
/// start of the utterance right after the wake word is not lost.
/// Saturating gain in place.
fn apply_gain(frame: &mut [i16], gain: f32) {
    if (gain - 1.0).abs() < f32::EPSILON {
        return;
    }
    for s in frame.iter_mut() {
        *s = (f32::from(*s) * gain).round().clamp(-32768.0, 32767.0) as i16;
    }
}

/// The word as the timeline names it, and whether the input ignores it.
fn wake_detail(word: &str, shared: &Shared) -> String {
    if shared.wake_enabled.load(Ordering::Relaxed) {
        word.to_owned()
    } else {
        format!("{word} (wake word off)")
    }
}

/// One frame into the wake word detector, which is told first whether the
/// satellite plays anything. After echo cancellation, a wake word said over
/// playback can clear the threshold in one block with no block around it
/// above the support line (LibreEcho's `wake_worker.c`), so while anything
/// plays the detector takes one supporting score instead of two.
fn feed_detector(
    d: &mut pinna_wakeword::Detector,
    speaker: Option<&Speaker>,
    frame: &[i16],
    on_block: impl FnMut(&pinna_wakeword::BlockScore),
) -> Vec<pinna_wakeword::Detection> {
    d.set_playback(speaker.is_some_and(Speaker::plays_anything));
    d.push_with(frame, on_block)
}

#[expect(
    clippy::too_many_arguments,
    reason = "each is one input of the capture task"
)]
async fn capture_loop(
    (capture, mut gain): (CaptureConfig, CaptureGain),
    people: Arc<people::People>,
    mut detector: Option<pinna_wakeword::Detector>,
    speaker: Option<Arc<Speaker>>,
    mut keywords: mpsc::UnboundedReceiver<KeywordSet>,
    shared: Arc<Shared>,
    esp: EsphomeHandle,
    inputs: mpsc::Sender<Input>,
    // The words' phrases, for the debugging timeline: the detector knows ids.
    catalogue: Arc<Catalogue>,
) {
    let mut stream = match start_capture(capture).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("microphone unavailable: {e}");
            return;
        }
    };
    tracing::info!("microphone running");
    // The stream changing explains a gap in the debugging timeline.
    let mut status = stream.status();
    tokio::spawn(async move {
        let mut last = "";
        loop {
            let now = match &*status.borrow_and_update() {
                CaptureStatus::Running => "processed stream",
                CaptureStatus::RunningRaw => "raw lanes, without echo cancellation",
                CaptureStatus::Starting | CaptureStatus::Restarting { .. } => "starting",
                CaptureStatus::Finished | CaptureStatus::Stopped => "stopped",
            };
            if now != last {
                last = now;
                crate::timeline::event(crate::timeline::EventKind::Microphone, now);
            }
            if status.changed().await.is_err() {
                break;
            }
        }
    });
    let mut router = MicRouter::default().with_speaker_tap(people::tap::Tap::new(
        people,
        shared.speaker.clone(),
        inputs.clone(),
        shared.counters.clone(),
    ));
    // Peak detector score of the current utterance, for near-miss logging.
    let mut peak = 0.0f32;
    // The peak reached the threshold (and something else rejected it).
    let mut peak_reached = false;
    // Which word scored the peak, for the near-miss log.
    let mut peak_word = String::new();
    // The peak's level and gain, for the debugging timeline: the level
    // window is gone by the time a near miss is decided.
    let mut peak_level = 0.0f32;
    let mut peak_gain = 0.0f32;
    let mut peak_at = std::time::Instant::now();
    // The newest set of wake words applied (ADR 0013).
    let mut generation = 0u64;
    let mut detected_at: Option<std::time::Instant> = None;
    // Each wake word's level before the gain (ADR 0026).
    let mut levels = WakeLevel::default();
    while let Some(mut frame) = stream.recv().await {
        // Newly chosen wake words, loaded outside this task.
        while let Ok(set) = keywords.try_recv() {
            if let Some(d) = detector.as_mut()
                && set.generation > generation
            {
                generation = set.generation;
                d.set_keywords(set.keywords);
            }
        }
        if let Some(d) = detector.as_mut() {
            let t = f32::from_bits(shared.threshold.load(Ordering::Relaxed));
            if t > 0.0 && t != d.threshold() {
                d.set_threshold(t);
            }
        }
        if shared.muted.load(Ordering::Relaxed) {
            router.clear_preroll();
            if let Some(d) = detector.as_mut() {
                d.reset();
            }
            levels.reset();
            continue;
        }
        let pre_gain = pinna_audio::agc::mean_square(&frame);
        // The recording hears the frame before and after the gain (ADR 0030).
        let before_gain = shared.recording.active().then_some(frame);
        gain.apply(&mut frame);
        // The timeline's audio lanes (ADR 0036): one check while they are off.
        let mut lane = timeline::levels_on()
            .then(|| (pre_gain, pinna_audio::agc::mean_square(&frame), gain.db()));
        if let Some(before) = &before_gain {
            shared.recording.frame(before, &frame);
        }
        let streaming = shared.streaming.load(Ordering::SeqCst);
        // The wake detector's latest noise floor seeds the shadow endpointer.
        let noise_seed = match shared.noise_energy.load(Ordering::Relaxed) {
            0 => None,
            energy => Some(energy),
        };
        let routed = router.route(frame_bytes(&frame), streaming, &shared.mic, noise_seed);
        shared.counters.mic_dropped(router.dropped());
        if routed.end_home_assistant {
            let _ = esp.send(DeviceEvent::VoiceAudioEnd).await;
        }
        for frame in routed.home_assistant {
            let _ = esp.try_send(DeviceEvent::VoiceAudio(frame));
        }
        if routed.reset_detector
            && let Some(d) = detector.as_mut()
        {
            // The detector skipped routed audio; drop its stale history.
            d.reset();
            levels.reset();
        }
        if routed.detect_wake {
            // Always feed the detector so it keeps its audio context (it needs
            // ~2 s of history); only act on detections while wake is enabled.
            if let Some(d) = detector.as_mut() {
                let threshold = d.threshold();
                let mut new_peak = None;
                let mut speech = false;
                levels.frame(pre_gain);
                let detections = tokio::task::block_in_place(|| {
                    feed_detector(d, speaker.as_deref(), &frame, |b| {
                        levels.block(b);
                        speech |= b.vad_active;
                        if b.score > peak {
                            peak = b.score;
                            peak_reached = b.score >= threshold;
                            peak_at = std::time::Instant::now();
                            new_peak = b.keyword;
                            if crate::timeline::get().is_some_and(|t| t.on()) {
                                peak_level = levels.word(b.sample_index).map_or(0.0, |l| l.dbfs);
                                peak_gain = gain.db();
                            }
                        }
                    })
                });
                if let Some((before, after, gain_db)) = lane.take() {
                    let scores: Vec<_> = d
                        .scores()
                        .map(|(id, s)| (catalogue.phrase(id), s))
                        .collect();
                    timeline::levels_frame(
                        tokio::time::Instant::now(),
                        before,
                        after,
                        gain_db,
                        Some(timeline::Detected {
                            threshold,
                            scores: &scores,
                            speech,
                        }),
                    );
                }
                shared
                    .noise_energy
                    .store(d.noise_energy().unwrap_or(0), Ordering::Relaxed);
                if let Some(k) = new_peak.and_then(|i| d.keyword(i)) {
                    peak_word = k.name().to_owned();
                }
                // A near miss (scored, but not detected) once the utterance is over:
                // the score only, never audio.
                if !detections.is_empty() {
                    detected_at = Some(std::time::Instant::now());
                    peak = 0.0;
                } else if peak_at.elapsed() > std::time::Duration::from_secs(2) {
                    // The rest of a detected utterance is not a near miss.
                    let after_detection = detected_at
                        .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(4));
                    if peak >= 0.2 && !after_detection {
                        tracing::info!("wake word near miss ({peak_word}): peak score {peak:.2}");
                        crate::timeline::with(|t| {
                            let mut event = crate::timeline::Event::new(
                                crate::timeline::EventKind::NearMiss,
                                &wake_detail(catalogue.phrase(&peak_word), &shared),
                            );
                            event.score = peak;
                            event.threshold = threshold;
                            event.level_dbfs = peak_level;
                            event.gain_db = peak_gain;
                            t.push(event);
                        });
                        if shared.wake_enabled.load(Ordering::Relaxed) {
                            let _ = inputs
                                .send(Input::NearMiss {
                                    score: peak,
                                    reached_threshold: peak_reached,
                                })
                                .await;
                        }
                    }
                    peak = 0.0;
                }
                crate::timeline::with(|t| {
                    for det in &detections {
                        let word = d.keyword(det.keyword).map_or("", |k| k.name());
                        let mut event = crate::timeline::Event::new(
                            crate::timeline::EventKind::Wake,
                            &wake_detail(catalogue.phrase(word), &shared),
                        );
                        event.score = det.score;
                        event.threshold = threshold;
                        event.level_dbfs = levels.word(det.sample_index).map_or(0.0, |l| l.dbfs);
                        event.gain_db = gain.db();
                        t.push(event);
                    }
                });
                if shared.wake_enabled.load(Ordering::Relaxed) && !detections.is_empty() {
                    // The time the detector decided, taken here because
                    // journald stamps a line only when it arrives.
                    let at = jiff::Timestamp::now();
                    for det in detections {
                        let id = d
                            .keyword(det.keyword)
                            .map(|k| k.name().to_owned())
                            .unwrap_or_default();
                        // ADR 0026 measures from these lines whether the
                        // level or the score picks the nearer satellite, before
                        // the group's wake word arbitration is built.
                        let level = levels.word(det.sample_index).map_or_else(
                            || "level before the gain unknown".to_owned(),
                            |l| l.to_string(),
                        );
                        let gain = gain.to_string();
                        tracing::info!(
                            "wake word {id:?}: {level}, {gain}, score {:.2}, at {at:.3}",
                            det.score
                        );
                        let _ = inputs
                            .send(Input::Wake {
                                id,
                                score: det.score,
                            })
                            .await;
                    }
                }
            }
        }
        // A frame the detector didn't hear has no words.
        if let Some((before, after, gain_db)) = lane {
            timeline::levels_frame(tokio::time::Instant::now(), before, after, gain_db, None);
        }
    }
    shared.mic.close();
    tracing::warn!("microphone stream ended");
}

#[tokio::main(flavor = "multi_thread", worker_threads = 3)]
async fn main() -> Result<()> {
    let fmt = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false);
    // journald timestamps lines itself.
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        fmt.without_time().with_ansi(false).init();
    } else {
        fmt.with_timer(tracing_subscriber::fmt::time::uptime())
            .init();
    }
    let cli = Cli::parse();
    let cfg = Config::load_layered(&cli.config, &cli.local_config)?;
    if let (Some(file), Some(threshold)) = (&cli.check_wake_word, cli.check_threshold) {
        // Nothing else runs: no sockets, no writes (ADR 0013).
        std::process::exit(check_wake_word(&cfg, file, threshold));
    }
    if cli.set_hostname {
        let settings = store::Store::load(
            &cli.settings,
            settings::FILE_MODE,
            settings::Settings::default(),
        )
        .1;
        let name = identity::node_name(
            settings.hostname.as_deref(),
            &cfg.device.name,
            &identity::device_mac(),
        );
        if identity::set_hostname(&name)? {
            tracing::info!("hostname: {name}");
        }
        return Ok(());
    }
    let defaults = settings::Settings {
        volume: cfg.audio.volume,
        wake_sensitivity: settings::sensitivity_for_threshold(cfg.wake.threshold),
        ..Default::default()
    };
    let (_, initial, _) = store::Store::load(&cli.settings, settings::FILE_MODE, defaults);

    // The time zone (ADR 0007): UTC until one is chosen.
    let zone = initial.time_zone.as_deref().unwrap_or("UTC");
    if let Err(e) = timezone::apply(
        zone,
        Path::new(timezone::ETC_LOCALTIME),
        Path::new(timezone::LINK),
        Path::new(timezone::ZONEINFO),
    ) {
        tracing::warn!("time zone: {e:#}");
    }

    // The development VM's virtual Echo Dot (ADR 0014): before the buttons and
    // the privacy latch are looked for, so they are found like the Dot's.
    let virt = if cfg.dev.virtual_device {
        match virtual_device::VirtualDevice::start() {
            Ok(v) => {
                tracing::info!("virtual Echo Dot: buttons, microphone switch, browser audio");
                Some(v)
            }
            Err(e) => {
                tracing::error!("virtual Echo Dot disabled: {e:#}");
                None
            }
        }
    } else {
        None
    };

    // Which hardware this is (ADR 0032). Hardware Pinna doesn't know (a
    // computer, the VM) has no board; its drivers look where the Echo Dot
    // (2nd gen)'s are and find nothing there. The virtual Dot stands in for
    // the Echo Dot's model.
    let board = pinna_hal::board::detect();
    let soc = Soc::in_compatible(Path::new(pinna_hal::board::DT_COMPATIBLE));
    let (hw, drivers) = match driver_board(board, soc) {
        Ok(hw) => (hw, true),
        Err(soc) => {
            tracing::warn!(
                "this {} board is not one Pinna knows: starting without audio and the light \
                 ring, since the Echo Dot (2nd gen)'s drivers don't fit it",
                soc.compatible()
            );
            (&pinna_hal::board::ECHO_DOT_2, false)
        }
    };
    let model = if virt.is_some() {
        Some(Model::EchoDot2)
    } else {
        board.map(|b| b.model)
    };
    tracing::info!("board: {}", board.map_or("none", |b| b.model.name()));

    // Identity + ESPHome API.
    let mac = identity::device_mac();
    let name = identity::node_name(initial.hostname.as_deref(), &cfg.device.name, &mac);
    // Again here: a board without the MAC in its idme record only knows it
    // once wlan0 exists, after pinna-hostname.service ran (ADR 0010).
    match identity::set_hostname(&name) {
        Ok(true) => tracing::info!("hostname: {name}"),
        Ok(false) => {}
        Err(e) => tracing::warn!("hostname: {e:#}"),
    }
    // The release that booted, and the update channel's source (ADR 0022).
    let booted_version =
        pinna_bootctl::slot::booted_version(std::path::Path::new(pinna_bootctl::slot::RUN_DIR))
            .ok()
            .flatten()
            .unwrap_or_else(|| pinna_version::VERSION.into());
    let updater = Arc::new(updates::Updater::new(updates::UpdaterPaths::default()));
    let default_channel = updates::default_channel(updater.development_build(), &booted_version);
    updater.init_source(match &cfg.update.manifest {
        Some(url) => updates::Source::Manifest(url.clone()),
        None => updates::Source::Channel(initial.update_channel.unwrap_or(default_channel)),
    });
    updater.init_automatic(updates::Policy {
        mode: initial.install_updates(),
        window: initial.install_window(),
        zone: initial.time_zone.clone(),
    });
    updater.set_counting(initial.include_in_statistics());
    // The dashboard's name wins over pinnad.toml's, which wins over the
    // model and MAC suffix (ADR 0007).
    let defaults = voice::Defaults {
        node_name: identity::node_name(None, &cfg.device.name, &mac),
        default_name: cfg.device.friendly_name.is_none(),
        friendly_name: cfg
            .device
            .friendly_name
            .clone()
            .unwrap_or_else(|| identity::default_friendly_name(model, &mac)),
        wake_sound: cfg.audio.wake_sound,
        update_channel: default_channel,
    };
    let mut dc = DeviceConfig::new(name, mac);
    dc.friendly_name = initial
        .friendly_name
        .clone()
        .unwrap_or_else(|| defaults.friendly_name.clone());
    dc.model = cfg
        .device
        .model
        .clone()
        .unwrap_or_else(|| identity::model_name(model).into());
    dc.manufacturer = cfg.device.manufacturer.clone();
    // The release that booted, as pinna-init read it from the root image's
    // signed footer; the crate's version says nothing about the release.
    if let Ok(Some(v)) =
        pinna_bootctl::slot::booted_version(std::path::Path::new(pinna_bootctl::slot::RUN_DIR))
    {
        dc.project_version = v;
    }
    dc.suggested_area = cfg.device.suggested_area.clone();
    dc.port = cfg.esphome.port;
    // Home Assistant's "Visit" link on the device page opens the dashboard.
    dc.webserver_port = cfg
        .web
        .listen
        .parse::<std::net::SocketAddr>()
        .map_or(0, |a| a.port());
    dc.dashboard_url.clone_from(&initial.url);
    // Standalone mode offers Home Assistant no voice assistant (ADR 0017).
    dc.voice_feature_flags = initial.mode.voice_feature_flags();
    // Wake words (ADR 0013): those installed, and the ones chosen last.
    let wake_dir = cli.settings.with_file_name("wakewords");
    let mut catalogue = Catalogue::open(&cfg.wake, &wake_dir);
    let mut active_wake = catalogue.sanitize(
        initial
            .wake_words
            .as_deref()
            .unwrap_or(std::slice::from_ref(&cfg.wake.id)),
    );
    // The classifiers load before Home Assistant is told the words on offer,
    // so that a word that fails isn't offered. A custom word that crashes
    // pinnad here is failed at the next start (the loading mark), never
    // again: there is no crash loop.
    let backend = if drivers {
        audio_backend(&cfg.audio.backend, hw.audio.card)
    } else {
        None
    };
    let mut keywords = Vec::new();
    if backend.is_some() && cfg.wake.enabled {
        let loaded = catalogue.load(&active_wake);
        keywords = loaded.keywords;
        catalogue = catalogue.with_failed(&loaded.failed);
        active_wake = catalogue.sanitize(&active_wake);
    }
    let catalogue = Arc::new(catalogue);
    tracing::info!(
        "wake words on offer: {:?}; listening for {:?}",
        catalogue.phrases(
            &catalogue
                .words()
                .iter()
                .map(|w| w.id.clone())
                .collect::<Vec<_>>()
        ),
        catalogue.phrases(&active_wake)
    );
    dc.wake_words = catalogue.for_home_assistant();
    dc.active_wake_words = active_wake.clone();
    dc.max_active_wake_words = wakewords::MAX_ACTIVE as u32;
    pinna_esphome::echo_dot_entities(&mut dc);
    if let Some(url) = &cfg.update.manifest {
        // The URL replaces the channel: it is the "Updates" select's only option.
        for s in dc
            .selects
            .iter_mut()
            .filter(|s| s.object_id == "update_channel")
        {
            s.options = vec![url.clone()];
        }
    }
    dc.encryption_key = identity::load_encryption_key(&cfg.esphome)?;
    // How the previous boot ended (ADR 0012), for the dashboard and the log.
    let last_boot = boot::LastBoot::read(std::path::Path::new(pinna_bootctl::slot::RUN_DIR));
    tracing::info!("last boot: {}", last_boot.summary());
    tracing::info!(
        "ESPHome API encryption: {}",
        if dc.encryption_key.is_some() {
            "Noise"
        } else {
            "none (plaintext)"
        }
    );
    tracing::info!(
        "pinnad {} as {} ({})",
        pinna_version::VERSION,
        dc.name,
        dc.mac_string()
    );
    // The mode is known before the Brain's first snapshot: the setup API and
    // /mcp read it from the start.
    let (status_tx, status_rx) = watch::channel(Snapshot {
        mode: initial.mode,
        ..Snapshot::default()
    });
    // The standalone configuration (ADR 0017): its files sit beside settings.toml.
    let assistant = Service::load(Paths::beside(&cli.settings), status_rx.clone());
    // The record names the satellite group from the start, so a satellite in
    // a group doesn't rewrite it, and restart resolved, on every boot
    // (ADR 0026). The Brain republishes it when the group changes.
    dc.group = assistant.live().borrow().group.record();
    if cfg.esphome.mdns {
        if let Err(e) = identity::publish_mdns(&dc) {
            tracing::warn!("mDNS: {e:#}");
        }
        tokio::spawn(identity::watch_name());
    }
    let dc_mac = dc.mac_string();
    let web_port = dc.webserver_port;
    let mdns = cfg.esphome.mdns.then(|| dc.clone());
    let encrypted = dc.encryption_key.is_some();
    let (esp, mut ha) = pinna_esphome::serve(dc).await?;
    tracing::info!("ESPHome API listening on {}", esp.local_addr());

    // LEDs. With the stage helper in charge (its handover exists), the boot
    // stages show instead of the startup effect, so the base starts idle.
    let staged = PathBuf::from(pinna_bootctl::slot::RUN_DIR)
        .join("boot-stages")
        .exists();
    let (led_tx, led_rx) = watch::channel(LedState {
        base: if staged { Effect::Idle } else { Effect::Boot },
        brightness: initial.led_brightness,
        ..Default::default()
    });
    // The virtual Echo Dot's ring is the dashboard's drawing of WatchRing.
    let ring = match &virt {
        Some(_) => None,
        None if !drivers => None,
        None => led_ring(&cfg.leds, hw)?,
    };
    let led_backend = match (&ring, cfg.leds.backend.as_str()) {
        (None, _) if virt.is_some() => "virtual",
        (None, _) => "none",
        (Some(_), "mock") => "mock",
        (Some(_), _) => "is31",
    };
    // What the ring shows, for WatchRing.
    let (shown_tx, shown_rx) = watch::channel([Rgb::OFF; RING_PIXELS]);

    // Audio.
    tracing::info!("audio backend: {backend:?}");
    let audio_backend = match &backend {
        None => "none",
        Some(Backend::EchoDot) => "echodot",
        Some(Backend::EchoDotPipeWire) => "echodot-pipewire",
        Some(Backend::PipeWire) => "pipewire",
        Some(Backend::WavFile(_)) => "wav",
    };
    // Only the Echo Dot's own output reads a hardware pointer: Sendspin plays
    // in exact step there, and elsewhere at the pacing clock (ADR 0027 §12).
    let exact_backend = backend.as_ref().is_some_and(Backend::echo_dot_mixer);
    let speaker = backend.clone().map(|b| {
        let echo_dot = b.echo_dot_mixer();
        let mut sc = SpeakerConfig::new(b);
        sc.initial_volume = initial.volume;
        sc.alsa_device = hw.audio.playback.device.into();
        sc.device_rate = hw.audio.playback.rate;
        sc.profile.codec = audio_codec(hw.audio.codec);
        sc.pcm_status = pcm_status_path(hw.audio.playback.device, false).or(sc.pcm_status.take());
        // The board's tuning, unless a developer named another.
        let tuning = match cfg.audio.speaker_profile.as_deref() {
            None => hw.audio.speaker,
            Some(name) => SpeakerTuning::named(name).unwrap_or_else(|| {
                tracing::warn!(
                    "speaker_profile {name:?} is neither Flat nor Radar: using the board's tuning"
                );
                hw.audio.speaker
            }),
        };
        sc.profile.codec_flat = tuning.codec_profile == Some(pinna_hal::board::CodecProfile::Flat);
        if echo_dot {
            sc.pre_gain_db = tuning.pre_gain_db;
            sc.volume_boost = tuning.volume_boost;
            // A speaker that plays one channel gets a mono mix (ADR 0004).
            sc.mono = matches!(
                hw.audio.speaker_channels,
                pinna_hal::board::SpeakerChannels::Only(_)
            );
            // The corner protects the driver, so it follows the board.
            sc.highpass_hz = hw.audio.speaker_highpass_hz;
            sc.volume_curve = volume_curve(tuning.volume_curve);
        }
        Arc::new(Speaker::new(sc))
    });
    // The ring shows music as an equalizer and follows the voice while
    // speaking. It is spawned once the control API answers, below: until then
    // the stage helper keeps drawing the boot stages (ADR 0012).

    let shared = Arc::new(Shared::default());
    // The speaker model loads in the background when someone is enrolled
    // (ADR 0038).
    let people = people::People::open(
        &people::Paths::beside(&cli.settings),
        cfg.wake.speaker_model.clone(),
    );
    let (in_tx, in_rx) = mpsc::channel::<Input>(64);
    // The debugging timeline (ADR 0036): off until the dashboard turns it on.
    let timeline = timeline::init(in_tx.clone());
    pinna_http::trace::install(timeline.clone());
    pinna_audio::speaker::install_output_level(Arc::new(timeline::levels_speaker));

    // Microphone + wake word. Words chosen later reach the detector here.
    let mut keyword_tx = None;
    if let Some(b) = backend {
        let detector = if cfg.wake.enabled {
            let wc = pinna_wakeword::WakeConfig {
                threshold: initial.wake_threshold(),
                ..Default::default()
            };
            match pinna_wakeword::Detector::new(&cfg.wake.models, keywords, wc) {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::error!("wake word disabled: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        // The echo canceller's output is far below what the wake word model
        // expects; start where the fixed gain was.
        let start = match b {
            Backend::EchoDotPipeWire => 12.0,
            _ => 0.0,
        };
        let make_gain = || match cfg.audio.capture_gain_db {
            Some(db) => CaptureGain::Fixed(10f32.powf(db / 20.0)),
            None => CaptureGain::Auto {
                agc: Agc::new(start),
                logged: start,
            },
        };
        let gain = make_gain();
        match cfg.audio.capture_gain_db {
            Some(db) => tracing::info!("capture gain: fixed, {db:+.1} dB"),
            None => tracing::info!("capture gain: automatic, from {start:+.1} dB"),
        }
        let mut capture = CaptureConfig::new(b.clone());
        capture.alsa_device = hw.audio.capture.device.into();
        capture.codec = audio_codec(hw.audio.codec);
        capture.format = capture_format(hw)?;
        capture.pw_target = cfg.audio.capture_target.clone();
        // Where PipeWire's stream is missing, and without PipeWire, pinnad
        // mixes the board's microphones itself (ADR 0004).
        capture.front_end.mic_mask = hw.mics.all();
        capture.front_end.calibration =
            pinna_audio::dsp::Calibration::new(pinna_hal::ident::read_miccal());
        // The raw lanes stand in for dot_wake, unless a developer pointed the
        // capture somewhere else.
        capture.raw_fallback = b
            .pw_raw_target()
            .filter(|_| cfg.audio.capture_target.is_none())
            .map(|target| pinna_audio::capture::RawFallback {
                target: target.to_owned(),
                format: raw_lanes_format(hw),
                after: std::time::Duration::from_secs(1),
                retry_after: std::time::Duration::from_secs(60),
            });
        let (tx, rx) = mpsc::unbounded_channel();
        if detector.is_some() {
            keyword_tx = Some(tx);
        }
        tokio::spawn(capture_loop(
            (capture, gain),
            people.clone(),
            detector,
            speaker.clone(),
            rx,
            shared.clone(),
            esp.clone(),
            in_tx.clone(),
            catalogue.clone(),
        ));
    } else {
        tracing::warn!("no audio backend: wake word and speech disabled");
    }

    // Buttons.
    let mut buttons = if cli.stdin_buttons {
        pinna_hal::buttons::MockButtons::from_stdin()
    } else {
        pinna_hal::buttons::button_stream(hw.keys)
    };
    let tx = in_tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = buttons.recv().await {
            if tx.send(Input::Button(ev)).await.is_err() {
                break;
            }
        }
    });

    // Hardware mic privacy latch (the virtual Dot's is a file).
    let privacy_paths: Vec<PathBuf> = match &virt {
        Some(v) => vec![v.latch_path().into()],
        None => pinna_hal::privacy::state_paths(&hw.privacy),
    };
    let privacy_trigger: Vec<PathBuf> = match &virt {
        Some(v) => vec![v.latch_path().into()],
        None => pinna_hal::privacy::trigger_paths(&hw.privacy),
    };
    let has_hw_privacy = pinna_hal::privacy::read_privacy_state_from(
        &privacy_paths
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>(),
    )
    .is_some();
    if has_hw_privacy {
        let mut privacy = pinna_hal::privacy::privacy_stream_from(privacy_paths);
        let tx = in_tx.clone();
        tokio::spawn(async move {
            while let Some(muted) = privacy.recv().await {
                if tx.send(Input::Privacy(muted)).await.is_err() {
                    break;
                }
            }
        });
    }

    // Live streams into the media lane (AirPlay/Bluetooth bridges). Not
    // essential to the satellite: a failed bind is logged, not fatal.
    if let Some(s) = &speaker
        && !cfg.audio.stream_socket.as_os_str().is_empty()
    {
        let path = &cfg.audio.stream_socket;
        match s.media().serve_stream(path) {
            Ok(()) => {
                tracing::info!("stream ingest on {}", path.display());
                share_stream_socket(path);
            }
            Err(e) => tracing::error!("stream ingest on {}: {e}", path.display()),
        }
    }

    // Media lane state (playing, paused, ended) for the Home Assistant media player.
    if let Some(s) = &speaker {
        let mut state = s.media().state();
        let tx = in_tx.clone();
        tokio::spawn(async move {
            while state.changed().await.is_ok() {
                let s = state.borrow_and_update().clone();
                if tx.send(Input::Media(s)).await.is_err() {
                    break;
                }
            }
        });
    }

    // Home Assistant commands.
    let tx = in_tx.clone();
    tokio::spawn(async move {
        while let Some(cmd) = ha.recv().await {
            if tx.send(Input::Ha(cmd)).await.is_err() {
                break;
            }
        }
    });

    // The other satellites, for the dashboard's switcher (ADR 0021).
    let (satellites_tx, satellites_rx) = watch::channel(satellites::Seen::default());
    tokio::spawn(satellites::run(dc_mac.clone(), satellites_tx));

    // The setup hotspot (ADR 0028), and its name while it runs, for ListWifi.
    let hotspot_paths = hotspot::HotspotPaths::default();
    let has_hotspot = hotspot::available(&hotspot_paths);
    let (hotspot_tx, hotspot_rx) = watch::channel(hotspot::State::default());
    // The ring shows the hotspot while it runs.
    if has_hotspot {
        let mut state = hotspot_rx.clone();
        let tx = in_tx.clone();
        tokio::spawn(async move {
            let mut was = false;
            while state.changed().await.is_ok() {
                let on = state.borrow_and_update().name.is_some();
                if on != std::mem::replace(&mut was, on)
                    && tx.send(Input::Hotspot(on)).await.is_err()
                {
                    break;
                }
            }
        });
    }

    // The dashboard password and who may reach what (ADR 0029). Only the web
    // listener checks it; the loopback API below stays open.
    let auth = auth::Auth::new(assistant.clone(), &dc_mac, in_tx.clone());
    // With a password and no key, the ESPHome API refuses connections until
    // the owner opens its window.
    auth.attach_esphome(esp.clone());
    // New sessions, a new password or Sign out everywhere end debugging.
    let mut sessions_ended = auth.subscribe_end();
    let ending = timeline.clone();
    tokio::spawn(async move {
        while sessions_ended.changed().await.is_ok() {
            ending.sessions_ended();
        }
    });

    // The satellite group (ADR 0026): its calls to the other members, the
    // calls they make here, and the exchange of lists after every search.
    let group = Arc::new(group::Group::new(
        assistant.clone(),
        &dc_mac,
        web_port,
        status_rx.clone(),
        satellites_rx.clone(),
        group::Limits::default(),
    ));
    tokio::spawn(group::exchange::run(group.clone()));

    // Local control API.
    let device_api = api::DeviceApi::with_updater(
        api::Info {
            version: booted_version.clone(),
            mac: dc_mac.clone(),
            time_zones: Arc::new(timezone::zones(Path::new(timezone::ZONEINFO))),
            audio_backend: audio_backend.into(),
            led_backend: led_backend.into(),
            model: model.map_or("", Model::id).into(),
            virtual_device: virt.is_some(),
            last_boot,
            // tmpfiles seeds the key file from the image again after a reset.
            key_from_image: !cfg.esphome.encryption_key.trim().is_empty()
                || std::path::Path::new("/usr/share/pinna/device/esphome-key").exists(),
        },
        status_rx.clone(),
        in_tx.clone(),
        updater.clone(),
    );
    tokio::spawn(updater.clone().run_checks());
    tokio::spawn(updater.clone().run_automatic());
    let recorder = recording::Recorder::new(
        shared.clone(),
        in_tx.clone(),
        status_rx.clone(),
        // The lanes exist on the Echo's PipeWire graph only. The reference is
        // the one the board names (ADR 0004).
        (audio_backend == "echodot-pipewire").then(|| {
            Arc::new(recording::PwRecord {
                bin: "pw-record".into(),
                reference_lane: hw.speaker_lane().unwrap_or(hw.mics.lanes() - 1),
                lanes: hw.audio.capture.channels as u32,
            }) as Arc<dyn recording::DebugCapture>
        }),
    );
    let device_api = Arc::new(
        device_api
            .with_auth(auth.clone())
            .with_recorder(recorder.clone())
            .with_timeline(timeline)
            .with_group(group.clone())
            .with_ring(shown_rx)
            .with_satellites(satellites_rx.clone())
            .with_hotspot(has_hotspot, hotspot_rx)
            .with_wake_checker(wakewords::Checker::process(
                wake_dir,
                // The test reads the same configuration this run did.
                cli.config
                    .iter()
                    .flat_map(|c| ["--config".into(), c.clone().into_os_string()])
                    .chain([
                        "--local-config".into(),
                        cli.local_config.clone().into_os_string(),
                    ])
                    .collect(),
            )),
    );
    // A Wi-Fi trial that a stopped pinnad left behind goes back (ADR 0008).
    let wifi_api = device_api.clone();
    tokio::spawn(async move { wifi_api.recover_wifi().await });
    // The setup hotspot while no network is saved (ADR 0028); a join stops it.
    if has_hotspot {
        let wifi = device_api.wifi();
        let hotspot = Arc::new(hotspot::Hotspot::new(
            hotspot_paths,
            wifi.clone(),
            hotspot_tx,
        ));
        let stopper = hotspot.clone();
        wifi.set_before_join(Arc::new(move || {
            let h = stopper.clone();
            Box::pin(async move { h.stop().await })
        }));
        tokio::spawn(hotspot.run(network::hostname));
    }
    let api_addr = api::start(device_api.clone(), &cfg.api.listen).await?;
    tracing::info!("control API listening on {api_addr}");

    // Pinna runs (ADR 0012, stage 6): take the boot-stage ring over from the
    // helper and let it end the arc with stage 7, the health verdict.
    let boot = boot::BootRing::claim();
    if boot.is_some() {
        tracing::info!("took the boot stages over from the stage helper");
    }
    tokio::spawn(leds::run(
        ring,
        shown_tx,
        led_rx,
        speaker.as_ref().map(|s| s.spectrum()),
        speaker.as_ref().map(|s| s.voice_level()),
        boot,
    ));

    // The dashboard is not essential to the satellite: a failed bind is logged, not fatal.
    let assistant_api = Arc::new(
        assistant_api::AssistantApi::new(assistant.clone(), in_tx.clone())
            .with_satellites(satellites_rx, dc_mac)
            .with_group(group.clone()),
    );
    let people_api = Arc::new(
        people_api::PeopleApi::new(people.clone()).with_enroller(Arc::new(
            people::enroll::Enroller::new(people, recorder, auth.clone()),
        )),
    );
    // The tools' HTTP clients, counters and updater, shared by every run and /mcp call.
    let tool_kit = tools::ToolKit::new(shared.counters.clone())
        .with_updater(updater.clone())
        .with_group(group.clone());
    let mcp_server = Arc::new(mcp::McpServer::new(
        assistant.clone(),
        in_tx.clone(),
        status_rx.clone(),
        tool_kit.clone(),
        booted_version.clone(),
    ));
    // The other members' tool calls run as /mcp's do, sharing its limit.
    let peer_api = Arc::new(
        group::peer::PeerApi::new(group.clone())
            .with_auth(auth.clone())
            .with_brain(in_tx.clone())
            .with_tools(mcp_server.clone()),
    );
    match web::start(
        device_api,
        assistant_api,
        people_api,
        peer_api,
        &cfg.web,
        virt,
        mcp_server,
        &shared.counters,
        auth,
    )
    .await
    {
        Ok(Some(addr)) => tracing::info!("dashboard listening on http://{addr}"),
        Ok(None) => {}
        Err(e) => tracing::error!("dashboard: {e:#}"),
    }

    // AirPlay plays through the stream socket (ADR 0011).
    let airplay = speaker.is_some() && !cfg.audio.stream_socket.as_os_str().is_empty();
    // Bluetooth needs the same, and a controller.
    let bluetooth = airplay && bluetooth::present();
    // Sendspin plays on the speaker's media lane itself (ADR 0027).
    let sendspin = sendspin::Setup::new(
        speaker.as_deref().cloned(),
        exact_backend,
        cli.settings.with_file_name("sendspin.toml"),
        sendspin::device_info(board.map(|b| b.model), Some(mac)),
    )
    .at(cfg.sendspin.port, cfg.sendspin.record.clone());
    let spawner = ProdSpawner::new(
        in_tx.clone(),
        status_rx,
        Arc::new(shared.counters.standalone.clone()),
        tool_kit,
    );
    let brain = Brain::new_with_local(
        esp,
        speaker,
        shared,
        led_tx,
        status_tx,
        in_tx,
        Options {
            key_file: cfg.esphome.key_file.clone(),
            wake_words: catalogue,
            active_wake_words: active_wake,
            keywords: keyword_tx,
            defaults,
            mdns,
            encrypted,
            has_hw_privacy,
            privacy_trigger,
            mute_lamp: hw.privacy.mute_lamp.map(Into::into),
            settings_file: cli.settings.clone(),
            airplay,
            bluetooth,
            sendspin: Some(sendspin),
            settings: initial,
            updates: Some(updater),
        },
        LocalOptions {
            live: assistant.live(),
            spawner: Arc::new(spawner),
            timers_file: cli.settings.with_file_name("timers.toml"),
        },
    );
    tokio::select! {
        _ = brain.run(in_rx) => {}
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_group_id_by_name() {
        let groups = "root:x:0:\naudio:x:29:pulse\npinna-stream:x:996:shairport-sync\n";
        assert_eq!(gid_in(groups, "pinna-stream"), Some(996));
        assert_eq!(gid_in(groups, "audio"), Some(29));
        assert_eq!(gid_in(groups, "pinna"), None);
        assert_eq!(gid_in("broken", "broken"), None);
    }

    #[test]
    fn an_unknown_board_on_another_known_soc_gets_no_drivers() {
        let dot2 = &pinna_hal::board::ECHO_DOT_2;
        let dot5 = &pinna_hal::board::ECHO_DOT_5;
        assert_eq!(driver_board(Some(dot5), Some(Soc::Mt8519)), Ok(dot5));
        assert_eq!(driver_board(None, Some(Soc::Mt8519)), Err(Soc::Mt8519));
        assert_eq!(driver_board(None, Some(Soc::Mt8163)), Ok(dot2));
        assert_eq!(driver_board(None, None), Ok(dot2));
    }

    /// The MT8163 boards hand pinna-audio what it assumed before the audio
    /// followed the board.
    #[test]
    fn the_mt8163_boards_keep_the_audio_defaults() {
        let speaker = SpeakerConfig::new(Backend::EchoDot);
        for model in [Model::EchoDot2, Model::Echo2] {
            let hw = model.board();
            assert_eq!(audio_codec(hw.audio.codec), Codec::Mt8163Aic);
            assert_eq!(capture_format(hw).unwrap(), CaptureFormat::default());
            assert_eq!(hw.audio.playback.device, speaker.alsa_device);
            assert_eq!(hw.audio.playback.rate, speaker.device_rate);
            assert_eq!(
                pcm_status_path(hw.audio.playback.device, false),
                speaker.pcm_status
            );
            assert_eq!(
                hw.mics.speech_mask,
                pinna_audio::dsp::FrontEndConfig::default().mic_mask
            );
        }
    }

    /// The raw fallback follows the board: its lanes, not the MT8163's nine.
    #[test]
    fn the_raw_fallback_follows_the_boards_lanes() {
        assert_eq!(
            raw_lanes_format(&pinna_hal::board::ECHO_DOT_2),
            CaptureFormat::default()
        );
        let dot5 = &pinna_hal::board::ECHO_DOT_5;
        let format = raw_lanes_format(dot5);
        assert_eq!(format.channels, 6);
        assert_eq!(format.rate, 16_000);
        assert_eq!(format.mic_lanes, (0..dot5.mics.count).collect::<Vec<_>>());
        assert_eq!(format.channels, dot5.mics.lanes());
        format.check().unwrap();
        let mut cfg = CaptureConfig::new(Backend::EchoDotPipeWire);
        cfg.raw_fallback = Some(pinna_audio::capture::RawFallback {
            target: "dot_mics".into(),
            format,
            after: std::time::Duration::from_secs(1),
            retry_after: std::time::Duration::from_secs(60),
        });
        let (_, args) = pinna_audio::capture::capture_command(&cfg, true).unwrap();
        assert_eq!(
            args.join(" "),
            "--raw --rate 16000 --channels 6 --format s24 --target dot_mics -"
        );
    }

    /// A board that captures S16_LE is an error, not a panic.
    #[test]
    fn a_board_capturing_s16_is_refused_with_a_reason() {
        use pinna_hal::board::{Audio, ECHO_DOT_2, Pcm, SampleFormat};
        let board = Board {
            audio: Audio {
                capture: Pcm {
                    format: SampleFormat::S16Le,
                    ..ECHO_DOT_2.audio.capture
                },
                ..ECHO_DOT_2.audio
            },
            ..ECHO_DOT_2
        };
        let err = capture_format(&board).unwrap_err();
        assert!(err.to_string().contains("S16_LE"), "{err}");
    }

    /// Every board's capture format is one the front end decodes.
    #[test]
    fn every_boards_capture_format_decodes() {
        for model in Model::ALL {
            capture_format(model.board())
                .unwrap()
                .check()
                .unwrap_or_else(|e| panic!("{model:?}: {e}"));
        }
    }

    #[tokio::test]
    async fn the_wake_word_detector_knows_when_the_satellite_plays() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../blobs/models/openwakeword-0.5.1");
        if !dir.join("melspectrogram.onnx").exists() {
            eprintln!(
                "SKIP: models missing in {}; run `just models`",
                dir.display()
            );
            return;
        }
        let mut d = pinna_wakeword::Detector::new(&dir, vec![], Default::default()).unwrap();
        let mut cfg = SpeakerConfig::new(Backend::WavFile("unused".into()));
        cfg.idle_timeout = Some(std::time::Duration::from_secs(5));
        let speaker = Speaker::new(cfg);
        // One block of silence per call.
        let block = [0i16; pinna_wakeword::BLOCK_SAMPLES];
        let mut seen = Vec::new();
        feed_detector(&mut d, Some(&speaker), &block, |b| seen.push(b.playback));
        let play = {
            let speaker = speaker.clone();
            tokio::spawn(async move {
                speaker
                    .play_pcm(pinna_audio::tone::beep(440.0, 1_000, 0.5))
                    .await
            })
        };
        let mut st = speaker.state();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while *st.borrow() != pinna_audio::PlaybackState::Playing {
                st.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        feed_detector(&mut d, Some(&speaker), &block, |b| seen.push(b.playback));
        speaker.stop();
        let _ = play.await;
        feed_detector(&mut d, None, &block, |b| seen.push(b.playback));
        assert_eq!(seen, [false, true, false]);
    }
}
