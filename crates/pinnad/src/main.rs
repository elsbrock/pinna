//! pinnad — the Pinna device daemon.
//!
//! Wires the hardware (LED ring, buttons, mic privacy latch, microphone array,
//! speaker), the on-device wake word detector and the ESPHome native API, so
//! Home Assistant adopts the device as a voice satellite.

mod api;
mod config;
mod identity;
mod leds;
mod settings;
mod voice;
mod web;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use pinna_audio::{Backend, CaptureConfig, Speaker, SpeakerConfig, start_capture};
use pinna_esphome::{DeviceConfig, DeviceEvent, EsphomeHandle, WakeWordInfo};
use pinna_hal::led::{Is31Sysfs, LedHandle, MockLeds};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::leds::LedState;
use crate::voice::{Brain, Input, Options, Shared, Snapshot};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, default_value = "/etc/pinna/pinnad.toml")]
    config: PathBuf,
    /// Device-local configuration layered over `--config` (identity, overrides).
    #[arg(long, default_value = "/var/lib/pinna/pinnad.toml")]
    local_config: PathBuf,
    /// Runtime settings changed from Home Assistant (volume, sensitivity, ...).
    #[arg(long, default_value = "/var/lib/pinna/settings.toml")]
    settings: PathBuf,
    /// Read button presses from stdin (up/down/action/mute) — development.
    #[arg(long)]
    stdin_buttons: bool,
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
    backend: Backend,
    gain: f32,
    mut detector: Option<pinna_wakeword::Detector>,
    shared: Arc<Shared>,
    esp: EsphomeHandle,
    inputs: mpsc::Sender<Input>,
) {
    let mut stream = match start_capture(CaptureConfig::new(backend)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("microphone unavailable: {e}");
            return;
        }
    };
    tracing::info!("microphone running");
    let mut preroll: VecDeque<Bytes> = VecDeque::with_capacity(10);
    let mut was_streaming = false;
    // Peak detector score of the current utterance, for near-miss logging.
    let mut peak = 0.0f32;
    let mut peak_at = std::time::Instant::now();
    let mut detected_at: Option<std::time::Instant> = None;
    while let Some(mut frame) = stream.recv().await {
        apply_gain(&mut frame, gain);
        if let Some(d) = detector.as_mut() {
            let t = f32::from_bits(shared.threshold.load(Ordering::Relaxed));
            if t > 0.0 && t != d.threshold() {
                d.set_threshold(t);
            }
        }
        if shared.muted.load(Ordering::Relaxed) {
            preroll.clear();
            if let Some(d) = detector.as_mut() {
                d.reset();
            }
            continue;
        }
        let streaming = shared.streaming.load(Ordering::SeqCst);
        if streaming {
            if !was_streaming {
                for f in preroll.drain(..) {
                    let _ = esp.try_send(DeviceEvent::VoiceAudio(f));
                }
            }
            let _ = esp.try_send(DeviceEvent::VoiceAudio(frame_bytes(&frame)));
        } else {
            if was_streaming {
                let _ = esp.send(DeviceEvent::VoiceAudioEnd).await;
                // The detector skipped the streamed audio; drop its stale history.
                if let Some(d) = detector.as_mut() {
                    d.reset();
                }
            }
            if preroll.len() == 10 {
                preroll.pop_front();
            }
            preroll.push_back(frame_bytes(&frame));
            // Always feed the detector so it keeps its audio context (it needs
            // ~2 s of history); only act on detections while wake is enabled.
            if let Some(d) = detector.as_mut() {
                let detections = tokio::task::block_in_place(|| {
                    d.push_with(&frame, |b| {
                        if b.score > peak {
                            peak = b.score;
                            peak_at = std::time::Instant::now();
                        }
                    })
                });
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
                        tracing::info!("wake word near miss: peak score {peak:.2}");
                    }
                    peak = 0.0;
                }
                if shared.wake_enabled.load(Ordering::Relaxed) {
                    for det in detections {
                        let _ = inputs.send(Input::Wake { score: det.score }).await;
                    }
                }
            }
        }
        was_streaming = streaming;
    }
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
        fmt.without_time().init();
    } else {
        fmt.with_timer(tracing_subscriber::fmt::time::uptime())
            .init();
    }
    let cli = Cli::parse();
    let cfg = Config::load_layered(&cli.config, &cli.local_config)?;
    let defaults = settings::Settings {
        volume: cfg.audio.volume,
        wake_sensitivity: settings::sensitivity_for_threshold(cfg.wake.threshold),
        ..Default::default()
    };
    let (_, initial) = settings::Store::load(&cli.settings, defaults);

    // Identity + ESPHome API.
    let mac = identity::device_mac();
    let name = if cfg.device.name.is_empty() {
        identity::default_name(&mac)
    } else {
        cfg.device.name.clone()
    };
    let mut dc = DeviceConfig::new(name, mac);
    dc.friendly_name = cfg.device.friendly_name.clone();
    dc.model = cfg.device.model.clone();
    dc.manufacturer = cfg.device.manufacturer.clone();
    dc.suggested_area = cfg.device.suggested_area.clone();
    dc.port = cfg.esphome.port;
    // Home Assistant's "Visit" link on the device page opens the dashboard.
    dc.webserver_port = cfg
        .web
        .listen
        .parse::<std::net::SocketAddr>()
        .map_or(0, |a| a.port());
    dc.wake_words = vec![WakeWordInfo {
        id: cfg.wake.id.clone(),
        wake_word: cfg.wake.phrase.clone(),
        trained_languages: vec!["en".into()],
    }];
    dc.active_wake_words = vec![cfg.wake.id.clone()];
    pinna_esphome::echo_dot_entities(&mut dc);
    dc.encryption_key = identity::load_encryption_key(&cfg.esphome)?;
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
    if cfg.esphome.mdns
        && let Err(e) = identity::publish_mdns(&dc)
    {
        tracing::warn!("mDNS: {e:#}");
    }
    let (dc_name, dc_mac) = (dc.name.clone(), dc.mac_string());
    let (esp, mut ha) = pinna_esphome::serve(dc).await?;
    tracing::info!("ESPHome API listening on {}", esp.local_addr());

    // LEDs.
    let (led_tx, led_rx) = watch::channel(LedState {
        brightness: initial.led_brightness,
        ..Default::default()
    });
    let ring = led_ring(&cfg.leds)?;
    let led_backend = match (&ring, cfg.leds.backend.as_str()) {
        (None, _) => "none",
        (Some(_), "mock") => "mock",
        (Some(_), _) => "is31",
    };
    tokio::spawn(leds::run(ring, led_rx));

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
    let speaker = backend
        .clone()
        .filter(|b| !matches!(b, Backend::WavFile(_)))
        .map(|b| {
            let echo_dot = b.echo_dot_mixer();
            let mut sc = SpeakerConfig::new(b);
            sc.initial_volume = initial.volume;
            sc.profile.codec_flat = cfg.audio.speaker_profile.eq_ignore_ascii_case("flat");
            if echo_dot && sc.profile.codec_flat {
                // LibreEcho's Echo Dot (biscuit) profile: pre_gain_db=6.00.
                // The volume curve tops out at -12 dB, so this cannot clip.
                sc.pre_gain_db = 6.0;
            }
            Arc::new(Speaker::new(sc))
        });

    let shared = Arc::new(Shared::default());
    let (in_tx, in_rx) = mpsc::channel::<Input>(64);

    // Microphone + wake word.
    if let Some(b) = backend {
        let detector = if cfg.wake.enabled {
            let wc = pinna_wakeword::WakeConfig {
                threshold: initial.wake_threshold(),
                ..Default::default()
            };
            match pinna_wakeword::Detector::load(&cfg.wake.models, &cfg.wake.keyword, wc) {
                Ok(d) => {
                    tracing::info!("wake word: {} ({})", cfg.wake.phrase, cfg.wake.keyword);
                    Some(d)
                }
                Err(e) => {
                    tracing::error!("wake word disabled: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        let gain_db = cfg.audio.capture_gain_db.unwrap_or(match b {
            Backend::EchoDotPipeWire => 12.0,
            _ => 0.0,
        });
        tracing::info!("capture gain: {gain_db:+.1} dB");
        let gain = 10f32.powf(gain_db / 20.0);
        tokio::spawn(capture_loop(
            b,
            gain,
            detector,
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

    // Hardware mic privacy latch.
    let has_hw_privacy = pinna_hal::privacy::read_privacy_state().is_some();
    if has_hw_privacy {
        let mut privacy = pinna_hal::privacy::privacy_stream();
        let tx = in_tx.clone();
        tokio::spawn(async move {
            while let Some(muted) = privacy.recv().await {
                if tx.send(Input::Privacy(muted)).await.is_err() {
                    break;
                }
            }
        });
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

    // Local control API.
    let (status_tx, status_rx) = watch::channel(Snapshot::default());
    let device_api = Arc::new(api::DeviceApi::new(
        api::Info {
            name: dc_name,
            mac: dc_mac,
            wake_phrase: cfg.wake.phrase.clone(),
            audio_backend: audio_backend.into(),
            led_backend: led_backend.into(),
        },
        status_rx,
        in_tx.clone(),
    ));
    let api_addr = api::start(device_api.clone(), &cfg.api.listen).await?;
    tracing::info!("control API listening on {api_addr}");
    // The dashboard is not essential to the satellite: a failed bind is logged, not fatal.
    match web::start(device_api, &cfg.web).await {
        Ok(Some(addr)) => tracing::info!("dashboard listening on http://{addr}"),
        Ok(None) => {}
        Err(e) => tracing::error!("dashboard: {e:#}"),
    }

    let brain = Brain::new(
        esp,
        speaker,
        shared,
        led_tx,
        status_tx,
        in_tx,
        Options {
            key_file: cfg.esphome.key_file.clone(),
            wake_phrase: cfg.wake.phrase.clone(),
            wake_sound: cfg.audio.wake_sound,
            has_hw_privacy,
            settings_file: cli.settings.clone(),
            settings: initial,
        },
    );
    tokio::select! {
        _ = brain.run(in_rx) => {}
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}
