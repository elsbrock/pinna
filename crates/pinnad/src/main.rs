//! pinnad — the Pinna device daemon.
//!
//! Wires the hardware (LED ring, buttons, mic privacy latch, microphone array,
//! speaker), the on-device wake word detector and the ESPHome native API, so
//! Home Assistant adopts the device as a voice satellite.

mod airplay;
mod api;
mod bluetooth;
mod boot;
mod calibration;
mod checks;
mod clock;
mod config;
mod identity;
mod leds;
mod logs;
mod metrics;
mod mic;
mod network;
mod satellites;
mod secrets;
mod settings;
mod standalone;
mod store;
mod timezone;
mod updates;
mod virtual_device;
mod voice;
mod wakewords;
mod web;
mod wifi;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use pinna_audio::{Agc, Backend, CaptureConfig, Speaker, SpeakerConfig, start_capture};
use pinna_esphome::{DeviceConfig, DeviceEvent, EsphomeHandle, WakeWordInfo};
use pinna_hal::anim::Effect;
use pinna_hal::led::{Is31Sysfs, LedHandle, MockLeds, RING_PIXELS, Rgb};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::leds::LedState;
use crate::mic::MicRouter;
use crate::voice::{Brain, Input, LocalOptions, NoopSpawner, Options, Shared, Snapshot};
use crate::wakewords::{Catalogue, KeywordSet};

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

fn audio_backend(spec: &str) -> Option<Backend> {
    match spec {
        "none" => None,
        "echodot" => Some(Backend::EchoDot),
        "echodot-pipewire" => Some(Backend::EchoDotPipeWire),
        "pipewire" => Some(Backend::PipeWire),
        s if s.starts_with("wav:") => Some(Backend::WavFile(PathBuf::from(&s[4..]))),
        _ => {
            let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
            if cards.contains("mt-snd-card") {
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

fn led_ring(cfg: &config::Leds) -> Result<Option<LedHandle>> {
    match cfg.backend.as_str() {
        "none" => Ok(None),
        "mock" => Ok(Some(LedHandle::spawn(MockLeds::new()))),
        backend => match Is31Sysfs::discover() {
            Ok(mut ring) => {
                ring.set_brightness(cfg.brightness);
                tracing::info!("LED ring: IS31FL3236 via sysfs");
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

async fn capture_loop(
    capture: CaptureConfig,
    mut gain: CaptureGain,
    mut detector: Option<pinna_wakeword::Detector>,
    mut keywords: mpsc::UnboundedReceiver<KeywordSet>,
    shared: Arc<Shared>,
    esp: EsphomeHandle,
    inputs: mpsc::Sender<Input>,
) {
    let mut stream = match start_capture(capture).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("microphone unavailable: {e}");
            return;
        }
    };
    tracing::info!("microphone running");
    let mut router = MicRouter::default();
    // Peak detector score of the current utterance, for near-miss logging.
    let mut peak = 0.0f32;
    // The peak reached the threshold (and something else rejected it).
    let mut peak_reached = false;
    // Which word scored the peak, for the near-miss log.
    let mut peak_word = String::new();
    let mut peak_at = std::time::Instant::now();
    // The newest set of wake words applied (ADR 0013).
    let mut generation = 0u64;
    let mut detected_at: Option<std::time::Instant> = None;
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
            continue;
        }
        gain.apply(&mut frame);
        let streaming = shared.streaming.load(Ordering::SeqCst);
        // The wake detector's latest noise floor seeds the shadow endpointer.
        let noise_seed = match shared.noise_energy.load(Ordering::Relaxed) {
            0 => None,
            energy => Some(energy),
        };
        let routed = router.route(frame_bytes(&frame), streaming, &shared.mic, noise_seed);
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
        }
        if routed.detect_wake {
            // Always feed the detector so it keeps its audio context (it needs
            // ~2 s of history); only act on detections while wake is enabled.
            if let Some(d) = detector.as_mut() {
                let threshold = d.threshold();
                let mut new_peak = None;
                let detections = tokio::task::block_in_place(|| {
                    d.push_with(&frame, |b| {
                        if b.score > peak {
                            peak = b.score;
                            peak_reached = b.score >= threshold;
                            peak_at = std::time::Instant::now();
                            new_peak = b.keyword;
                        }
                    })
                });
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
                if shared.wake_enabled.load(Ordering::Relaxed) {
                    for det in detections {
                        let id = d.keyword(det.keyword).map(|k| k.name().to_owned());
                        let _ = inputs
                            .send(Input::Wake {
                                id: id.unwrap_or_default(),
                                score: det.score,
                            })
                            .await;
                    }
                }
            }
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

    // Which hardware this is; the virtual Dot stands in for the Echo Dot.
    let model = if virt.is_some() {
        "echo-dot-2"
    } else {
        pinna_hal::ident::board_model()
    };

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
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    let updater = Arc::new(updates::Updater::new(updates::UpdaterPaths::default()));
    let default_channel = updates::default_channel(updater.development_build(), &booted_version);
    updater.init_source(match &cfg.update.manifest {
        Some(url) => updates::Source::Manifest(url.clone()),
        None => updates::Source::Channel(initial.update_channel.unwrap_or(default_channel)),
    });
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
    // Wake words (ADR 0013): those installed, and the ones chosen last.
    let catalogue = Arc::new(Catalogue::new(&cfg.wake));
    let active_wake = catalogue.sanitize(
        initial
            .wake_words
            .as_deref()
            .unwrap_or(std::slice::from_ref(&cfg.wake.id)),
    );
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
    dc.wake_words = catalogue
        .words()
        .iter()
        .map(|w| WakeWordInfo {
            id: w.id.clone(),
            wake_word: w.phrase.clone(),
            trained_languages: vec!["en".into()],
        })
        .collect();
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
        env!("CARGO_PKG_VERSION"),
        dc.name,
        dc.mac_string()
    );
    if cfg.esphome.mdns {
        if let Err(e) = identity::publish_mdns(&dc) {
            tracing::warn!("mDNS: {e:#}");
        }
        tokio::spawn(identity::watch_name());
    }
    let dc_mac = dc.mac_string();
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
        None => led_ring(&cfg.leds)?,
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
    let backend = audio_backend(&cfg.audio.backend);
    tracing::info!("audio backend: {backend:?}");
    let audio_backend = match &backend {
        None => "none",
        Some(Backend::EchoDot) => "echodot",
        Some(Backend::EchoDotPipeWire) => "echodot-pipewire",
        Some(Backend::PipeWire) => "pipewire",
        Some(Backend::WavFile(_)) => "wav",
    };
    let speaker = backend.clone().map(|b| {
        let echo_dot = b.echo_dot_mixer();
        let mut sc = SpeakerConfig::new(b);
        sc.initial_volume = initial.volume;
        sc.profile.codec_flat = identity::speaker_flat(cfg.audio.speaker_profile.as_deref(), model);
        if echo_dot && sc.profile.codec_flat {
            // The Echo Dot (biscuit) speaker: +6 dB on top of the volume
            // curve (which tops out at -12 dB), plus the stock firmware's
            // extra gain above 70 %, which the speaker chain's limiter
            // keeps under full scale.
            sc.pre_gain_db = 6.0;
            sc.volume_boost = true;
        }
        Arc::new(Speaker::new(sc))
    });
    // The ring shows music as an equalizer and follows the voice while
    // speaking. It is spawned once the control API answers, below: until then
    // the stage helper keeps drawing the boot stages (ADR 0012).

    let shared = Arc::new(Shared::default());
    let (_live_tx, live) = watch::channel(Arc::new(standalone::AssistLive::empty()));
    let (in_tx, in_rx) = mpsc::channel::<Input>(64);

    // Microphone + wake word. Words chosen later reach the detector here.
    let mut keyword_tx = None;
    if let Some(b) = backend {
        let detector = if cfg.wake.enabled {
            let wc = pinna_wakeword::WakeConfig {
                threshold: initial.wake_threshold(),
                ..Default::default()
            };
            let keywords = catalogue.load(&active_wake);
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
        let gain = match cfg.audio.capture_gain_db {
            Some(db) => {
                tracing::info!("capture gain: fixed, {db:+.1} dB");
                CaptureGain::Fixed(10f32.powf(db / 20.0))
            }
            None => {
                // The echo canceller's output is far below what the wake
                // word model expects; start where the fixed gain was.
                let start = match b {
                    Backend::EchoDotPipeWire => 12.0,
                    _ => 0.0,
                };
                tracing::info!("capture gain: automatic, from {start:+.1} dB");
                CaptureGain::Auto {
                    agc: Agc::new(start),
                    logged: start,
                }
            }
        };
        let mut capture = CaptureConfig::new(b);
        capture.pw_target = cfg.audio.capture_target.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        if detector.is_some() {
            keyword_tx = Some(tx);
        }
        tokio::spawn(capture_loop(
            capture,
            gain,
            detector,
            rx,
            shared.clone(),
            esp.clone(),
            in_tx.clone(),
        ));
    } else {
        tracing::warn!("no audio backend: wake word and speech disabled");
    }

    // Buttons.
    let mut buttons = if cli.stdin_buttons {
        pinna_hal::buttons::MockButtons::from_stdin()
    } else {
        pinna_hal::buttons::button_stream()
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
        None => vec![
            pinna_hal::privacy::PRIVACY_STATE_PATH.into(),
            pinna_hal::privacy::PRIVACY_STATE_FALLBACK_PATH.into(),
        ],
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

    // Local control API.
    let (status_tx, status_rx) = watch::channel(Snapshot::default());
    let device_api = api::DeviceApi::with_updater(
        api::Info {
            version: booted_version,
            mac: dc_mac,
            time_zones: Arc::new(timezone::zones(Path::new(timezone::ZONEINFO))),
            wake_words: catalogue.clone(),
            audio_backend: audio_backend.into(),
            led_backend: led_backend.into(),
            model: model.into(),
            virtual_device: virt.is_some(),
            last_boot,
            // tmpfiles seeds the key file from the image again after a reset.
            key_from_image: !cfg.esphome.encryption_key.trim().is_empty()
                || std::path::Path::new("/usr/share/pinna/device/esphome-key").exists(),
        },
        status_rx,
        in_tx.clone(),
        updater.clone(),
    );
    tokio::spawn(updater.clone().run_checks());
    let device_api = Arc::new(
        device_api
            .with_ring(shown_rx)
            .with_satellites(satellites_rx),
    );
    // A Wi-Fi trial that a stopped pinnad left behind goes back (ADR 0008).
    let wifi_api = device_api.clone();
    tokio::spawn(async move { wifi_api.recover_wifi().await });
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
    match web::start(device_api, &cfg.web, virt, &shared.counters).await {
        Ok(Some(addr)) => tracing::info!("dashboard listening on http://{addr}"),
        Ok(None) => {}
        Err(e) => tracing::error!("dashboard: {e:#}"),
    }

    // AirPlay plays through the stream socket (ADR 0011).
    let airplay = speaker.is_some() && !cfg.audio.stream_socket.as_os_str().is_empty();
    // Bluetooth needs the same, and a controller.
    let bluetooth = airplay && bluetooth::present();
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
            settings_file: cli.settings.clone(),
            airplay,
            bluetooth,
            settings: initial,
            updates: Some(updater),
        },
        LocalOptions {
            live,
            spawner: Arc::new(NoopSpawner),
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
}
