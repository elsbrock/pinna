//! The device's state machine: wake word / button → Home Assistant Assist run
//! over the ESPHome API → TTS playback, plus timers, announcements, mute, the
//! light entity and volume. Single task, all inputs arrive on one channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pinna_audio::{Speaker, tone};
use pinna_esphome::{DeviceEvent, EsphomeHandle, HaCommand, TimerEventType, VoiceEventType};
use pinna_hal::anim::Effect;
use pinna_hal::buttons::{Button, ButtonEvent};
use pinna_hal::led::Rgb;
use tokio::sync::{mpsc, watch};

use crate::leds::LedState;

/// Flags shared with the capture task.
#[derive(Default)]
pub struct Shared {
    /// Forward microphone frames to Home Assistant.
    pub streaming: AtomicBool,
    /// Run the wake word detector on microphone frames.
    pub wake_enabled: AtomicBool,
    /// Microphone muted (hardware latch or Home Assistant switch).
    pub muted: AtomicBool,
}

#[derive(Debug)]
pub enum Input {
    Ha(HaCommand),
    Button(ButtonEvent),
    Privacy(bool),
    Wake { score: f32 },
    PlaybackDone { token: u64, ok: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// VoiceStart sent, waiting for HA's VoiceAssistantResponse.
    Starting(Instant),
    /// Streaming the microphone to HA.
    Listening(Instant),
    /// HA is processing (STT/intent/TTS).
    Thinking(Instant),
    /// Playing TTS or an announcement.
    Speaking,
}

#[derive(Debug, Clone, PartialEq)]
enum PlaybackKind {
    Tts,
    Announce { start_conversation: bool },
    Alarm,
    Cue,
}

struct HaTimer {
    total: u32,
    left: u32,
    active: bool,
    at: Instant,
}

impl HaTimer {
    fn remaining(&self, now: Instant) -> u32 {
        if self.active {
            self.left
                .saturating_sub(now.saturating_duration_since(self.at).as_secs() as u32)
        } else {
            self.left
        }
    }
}

pub struct Options {
    pub wake_phrase: String,
    pub wake_sound: bool,
    pub has_hw_privacy: bool,
}

pub struct Brain {
    esp: EsphomeHandle,
    speaker: Option<Arc<Speaker>>,
    shared: Arc<Shared>,
    leds: watch::Sender<LedState>,
    self_tx: mpsc::Sender<Input>,
    http: reqwest::Client,
    opts: Options,

    phase: Phase,
    conversation_id: String,
    continue_conversation: bool,
    run_ended: bool,
    tts_started: bool,
    tts_url: Option<String>,

    hw_muted: bool,
    sw_muted: bool,
    voice_subscribed: bool,
    clients: u32,

    light_on: bool,
    light_brightness: f32,
    light_rgb: (f32, f32, f32),

    timers: HashMap<String, HaTimer>,
    ringing: Option<(String, Instant)>,

    playback: Option<(u64, PlaybackKind, tokio::task::JoinHandle<()>)>,
    next_token: u64,
    booted_at: Instant,
    boot_done: bool,
}

const START_TIMEOUT: Duration = Duration::from_secs(5);
const LISTEN_TIMEOUT: Duration = Duration::from_secs(15);
const THINK_TIMEOUT: Duration = Duration::from_secs(30);
const RING_TIMEOUT: Duration = Duration::from_secs(300);
const BOOT_ANIMATION: Duration = Duration::from_millis(1200);
const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;

impl Brain {
    pub fn new(
        esp: EsphomeHandle,
        speaker: Option<Arc<Speaker>>,
        shared: Arc<Shared>,
        leds: watch::Sender<LedState>,
        self_tx: mpsc::Sender<Input>,
        opts: Options,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        Self {
            esp,
            speaker,
            shared,
            leds,
            self_tx,
            http,
            opts,
            phase: Phase::Idle,
            conversation_id: String::new(),
            continue_conversation: false,
            run_ended: false,
            tts_started: false,
            tts_url: None,
            hw_muted: false,
            sw_muted: false,
            voice_subscribed: false,
            clients: 0,
            light_on: false,
            light_brightness: 1.0,
            light_rgb: (1.0, 1.0, 1.0),
            timers: HashMap::new(),
            ringing: None,
            playback: None,
            next_token: 1,
            booted_at: Instant::now(),
            boot_done: false,
        }
    }

    pub async fn run(mut self, mut inputs: mpsc::Receiver<Input>) {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.refresh();
        loop {
            tokio::select! {
                input = inputs.recv() => match input {
                    Some(input) => self.handle(input).await,
                    None => return,
                },
                _ = tick.tick() => self.tick().await,
            }
            self.refresh();
        }
    }

    fn muted(&self) -> bool {
        self.hw_muted || self.sw_muted
    }

    async fn send(&self, ev: DeviceEvent) {
        if self.esp.send(ev).await.is_err() {
            tracing::error!("ESPHome server stopped");
        }
    }

    async fn handle(&mut self, input: Input) {
        match input {
            Input::Wake { score } => {
                tracing::info!("wake word detected (score {score:.2})");
                if self.phase == Phase::Idle && self.ringing.is_none() && !self.muted() {
                    self.start_run(Some(self.opts.wake_phrase.clone())).await;
                }
            }
            Input::Button(ev) => self.button(ev).await,
            Input::Privacy(muted) => {
                tracing::info!("hardware mute: {muted}");
                self.hw_muted = muted;
                self.apply_mute().await;
            }
            Input::PlaybackDone { token, ok } => self.playback_done(token, ok).await,
            Input::Ha(cmd) => self.ha(cmd).await,
        }
    }

    // --- Home Assistant -----------------------------------------------------

    async fn ha(&mut self, cmd: HaCommand) {
        match cmd {
            HaCommand::ClientConnected { peer } => {
                self.clients += 1;
                tracing::info!("Home Assistant connected from {peer}");
                self.send(DeviceEvent::MuteState(self.muted())).await;
                self.send_light().await;
            }
            HaCommand::ClientDisconnected => {
                self.clients = self.clients.saturating_sub(1);
                tracing::info!("Home Assistant disconnected");
                if self.clients == 0 && self.phase != Phase::Idle {
                    self.abort_run(false).await;
                }
            }
            HaCommand::VoiceSubscribed(on) => {
                tracing::info!("voice assistant subscription: {on}");
                self.voice_subscribed = on;
            }
            HaCommand::VoiceStarted { port, error } => {
                if let Phase::Starting(_) = self.phase {
                    if error || port != 0 {
                        tracing::warn!(
                            "Home Assistant refused the run (port {port}, error {error})"
                        );
                        self.fail().await;
                    } else {
                        self.phase = Phase::Listening(Instant::now());
                        self.shared.streaming.store(true, Ordering::SeqCst);
                    }
                }
            }
            HaCommand::VoiceEvent { event, data } => self.voice_event(event, data).await,
            HaCommand::Timer {
                event,
                id,
                name,
                total_seconds,
                seconds_left,
                is_active,
            } => {
                self.timer(event, id, name, total_seconds, seconds_left, is_active)
                    .await
            }
            HaCommand::Announce {
                media_id,
                preannounce_media_id,
                start_conversation,
                ..
            } => {
                tracing::info!("announcement: {media_id}");
                if self.phase != Phase::Idle {
                    self.abort_run(true).await;
                }
                let urls: Vec<String> = [preannounce_media_id, media_id]
                    .into_iter()
                    .filter(|u| !u.is_empty())
                    .collect();
                self.phase = Phase::Speaking;
                self.play_urls(urls, PlaybackKind::Announce { start_conversation });
            }
            HaCommand::Light {
                on,
                brightness,
                rgb,
                effect,
            } => {
                if let Some(on) = on {
                    self.light_on = on;
                }
                if let Some(b) = brightness {
                    self.light_brightness = b.clamp(0.0, 1.0);
                }
                if let Some(c) = rgb {
                    self.light_rgb = c;
                }
                if let Some(e) = effect.filter(|e| e != "None") {
                    tracing::debug!("light effect {e} not supported");
                }
                self.send_light().await;
            }
            HaCommand::MuteSwitch(on) => {
                self.sw_muted = on;
                self.apply_mute().await;
            }
            HaCommand::SetActiveWakeWords(ids) => tracing::info!("active wake words: {ids:?}"),
            other => tracing::debug!("ignored: {other:?}"),
        }
    }

    async fn voice_event(&mut self, event: VoiceEventType, data: Vec<(String, String)>) {
        let get = |k: &str| {
            data.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        tracing::debug!("voice event {event:?} {data:?}");
        match event {
            VoiceEventType::RunStart => {
                if let Some(url) = get("url") {
                    self.tts_url = Some(url);
                }
            }
            VoiceEventType::SttVadEnd | VoiceEventType::SttEnd => {
                self.stop_streaming();
                if matches!(self.phase, Phase::Listening(_) | Phase::Starting(_)) {
                    self.phase = Phase::Thinking(Instant::now());
                }
            }
            VoiceEventType::IntentProgress
                if get("tts_start_streaming").as_deref() == Some("1") =>
            {
                self.stop_streaming();
                if let Some(url) = self.tts_url.clone().or_else(|| get("url")) {
                    self.start_tts(url);
                }
            }
            VoiceEventType::IntentEnd => {
                self.continue_conversation = get("continue_conversation").as_deref() == Some("1");
                if let Some(cid) = get("conversation_id").filter(|c| !c.is_empty()) {
                    self.conversation_id = cid;
                }
            }
            VoiceEventType::TtsEnd => {
                self.stop_streaming();
                if !self.tts_started {
                    match get("url").or_else(|| self.tts_url.clone()) {
                        Some(url) => self.start_tts(url),
                        None => tracing::warn!("TTS_END without url"),
                    }
                }
            }
            VoiceEventType::RunEnd => {
                self.run_ended = true;
                self.stop_streaming();
                if self.phase != Phase::Speaking {
                    self.finish_run().await;
                }
            }
            VoiceEventType::Error => {
                tracing::warn!(
                    "Assist error: {} {}",
                    get("code").unwrap_or_default(),
                    get("message").unwrap_or_default()
                );
                self.stop_streaming();
                self.continue_conversation = false;
                self.phase = Phase::Idle;
                self.stop_playback();
                self.error_feedback();
            }
            _ => {}
        }
    }

    async fn timer(
        &mut self,
        event: TimerEventType,
        id: String,
        name: String,
        total: u32,
        left: u32,
        active: bool,
    ) {
        tracing::info!("timer {event:?} {id} {name:?} {left}/{total}s");
        match event {
            TimerEventType::Started | TimerEventType::Updated => {
                self.timers.insert(
                    id,
                    HaTimer {
                        total: total.max(1),
                        left,
                        active,
                        at: Instant::now(),
                    },
                );
            }
            TimerEventType::Cancelled => {
                self.timers.remove(&id);
            }
            TimerEventType::Finished => {
                self.timers.remove(&id);
                if self.phase != Phase::Idle {
                    self.abort_run(true).await;
                }
                self.ringing = Some((id, Instant::now()));
                self.play_pcm(tone::timer_alarm(), PlaybackKind::Alarm);
            }
            TimerEventType::Unknown(_) => {}
        }
    }

    async fn send_light(&self) {
        self.send(DeviceEvent::LightState {
            on: self.light_on,
            brightness: self.light_brightness,
            rgb: self.light_rgb,
            effect: None,
        })
        .await;
    }

    // --- buttons --------------------------------------------------------------

    async fn button(&mut self, ev: ButtonEvent) {
        match ev {
            ButtonEvent::Press(b @ (Button::VolumeUp | Button::VolumeDown))
            | ButtonEvent::Repeat(b @ (Button::VolumeUp | Button::VolumeDown)) => {
                let Some(speaker) = &self.speaker else { return };
                let v = speaker.volume() as i16;
                let v = if b == Button::VolumeUp { v + 5 } else { v - 5 }.clamp(0, 100) as u8;
                speaker.set_volume(v);
                self.overlay(Effect::Volume(v));
                if matches!(ev, ButtonEvent::Press(_)) && self.playback.is_none() {
                    self.play_pcm(tone::beep(880.0, 40, 0.3), PlaybackKind::Cue);
                }
            }
            ButtonEvent::Press(Button::Action) => {
                if self.ringing.is_some() {
                    tracing::info!("timer alarm dismissed");
                    self.ringing = None;
                    self.stop_playback();
                } else if self.phase != Phase::Idle {
                    tracing::info!("run cancelled by button");
                    self.abort_run(true).await;
                } else if !self.muted() {
                    self.start_run(None).await;
                } else {
                    self.error_feedback();
                }
            }
            ButtonEvent::Press(Button::Mute) if !self.opts.has_hw_privacy => {
                // No hardware latch (dev machines): treat the button as a toggle.
                self.sw_muted = !self.sw_muted;
                self.apply_mute().await;
            }
            ButtonEvent::LongPress(b) => tracing::info!("long press: {b:?}"),
            _ => {}
        }
    }

    // --- runs -----------------------------------------------------------------

    async fn start_run(&mut self, wake_phrase: Option<String>) {
        if !self.voice_subscribed {
            tracing::warn!("no Home Assistant voice subscription; cannot start a run");
            self.error_feedback();
            return;
        }
        if self.conversation_id.is_empty() || !self.continue_conversation {
            self.conversation_id = format!("{:016x}", fastrand::u64(..));
        }
        self.continue_conversation = false;
        self.run_ended = false;
        self.tts_started = false;
        self.tts_url = None;
        self.phase = Phase::Starting(Instant::now());
        tracing::info!("starting Assist run {}", self.conversation_id);
        self.send(DeviceEvent::VoiceStart {
            conversation_id: self.conversation_id.clone(),
            wake_word_phrase: wake_phrase,
            use_vad: true,
        })
        .await;
        if self.opts.wake_sound {
            self.play_pcm(tone::wake_chime(), PlaybackKind::Cue);
        }
    }

    fn stop_streaming(&self) {
        self.shared.streaming.store(false, Ordering::SeqCst);
    }

    async fn abort_run(&mut self, tell_ha: bool) {
        self.stop_streaming();
        if tell_ha && self.phase != Phase::Idle {
            self.send(DeviceEvent::VoiceStop).await;
        }
        self.stop_playback();
        self.phase = Phase::Idle;
        self.continue_conversation = false;
    }

    async fn finish_run(&mut self) {
        self.stop_streaming();
        self.phase = Phase::Idle;
        if self.continue_conversation && !self.muted() {
            tracing::info!("continuing conversation {}", self.conversation_id);
            self.continue_conversation = true;
            self.start_run(None).await;
        } else {
            self.continue_conversation = false;
        }
    }

    async fn fail(&mut self) {
        self.stop_streaming();
        self.phase = Phase::Idle;
        self.continue_conversation = false;
        self.error_feedback();
    }

    fn error_feedback(&mut self) {
        self.overlay(Effect::Error);
        if self.playback.is_none() {
            self.play_pcm(tone::error_tone(), PlaybackKind::Cue);
        }
    }

    async fn apply_mute(&mut self) {
        let m = self.muted();
        self.shared.muted.store(m, Ordering::SeqCst);
        if m && self.phase != Phase::Idle {
            self.abort_run(true).await;
        }
        if let Err(e) = pinna_hal::led::set_mute_lamp(m) {
            tracing::debug!("mute lamp: {e}");
        }
        self.send(DeviceEvent::MuteState(m)).await;
    }

    // --- playback ---------------------------------------------------------------

    fn start_tts(&mut self, url: String) {
        self.tts_started = true;
        self.phase = Phase::Speaking;
        self.play_urls(vec![url], PlaybackKind::Tts);
    }

    fn stop_playback(&mut self) {
        if let Some((_, _, task)) = self.playback.take() {
            task.abort();
            if let Some(s) = &self.speaker {
                s.stop();
            }
        }
    }

    fn begin_playback(&mut self, kind: PlaybackKind) -> u64 {
        self.stop_playback();
        let token = self.next_token;
        self.next_token += 1;
        let _ = kind;
        token
    }

    fn play_pcm(&mut self, pcm: pinna_audio::Pcm, kind: PlaybackKind) {
        let token = self.begin_playback(kind.clone());
        let speaker = self.speaker.clone();
        let tx = self.self_tx.clone();
        let task = tokio::spawn(async move {
            let ok = match speaker {
                Some(s) => s.play_pcm(pcm).await.is_ok(),
                None => false,
            };
            let _ = tx.send(Input::PlaybackDone { token, ok }).await;
        });
        self.playback = Some((token, kind, task));
    }

    fn play_urls(&mut self, urls: Vec<String>, kind: PlaybackKind) {
        let token = self.begin_playback(kind.clone());
        let speaker = self.speaker.clone();
        let http = self.http.clone();
        let tx = self.self_tx.clone();
        let task = tokio::spawn(async move {
            let mut ok = true;
            for url in urls {
                if let Err(e) = play_url(&http, speaker.as_deref(), &url).await {
                    tracing::warn!("playing {url}: {e:#}");
                    ok = false;
                    break;
                }
            }
            let _ = tx.send(Input::PlaybackDone { token, ok }).await;
        });
        self.playback = Some((token, kind, task));
    }

    async fn playback_done(&mut self, token: u64, ok: bool) {
        let Some((current, kind, _)) = &self.playback else {
            return;
        };
        if *current != token {
            return;
        }
        let kind = kind.clone();
        self.playback = None;
        match kind {
            PlaybackKind::Tts => {
                if self.run_ended {
                    self.finish_run().await;
                } else {
                    // Audio finished before RUN_END; wait for it.
                    self.phase = Phase::Thinking(Instant::now());
                }
            }
            PlaybackKind::Announce { start_conversation } => {
                self.send(DeviceEvent::AnnounceFinished { success: ok })
                    .await;
                self.phase = Phase::Idle;
                if start_conversation && ok {
                    self.start_run(None).await;
                }
            }
            PlaybackKind::Alarm => {
                if let Some((_, since)) = &self.ringing {
                    if since.elapsed() < RING_TIMEOUT && ok {
                        self.play_pcm(tone::timer_alarm(), PlaybackKind::Alarm);
                    } else {
                        self.ringing = None;
                    }
                }
            }
            PlaybackKind::Cue => {}
        }
    }

    // --- housekeeping -------------------------------------------------------------

    async fn tick(&mut self) {
        let now = Instant::now();
        match self.phase {
            Phase::Starting(t) if now - t > START_TIMEOUT => {
                tracing::warn!("Home Assistant did not answer the run start");
                self.send(DeviceEvent::VoiceStop).await;
                self.fail().await;
            }
            Phase::Listening(t) if now - t > LISTEN_TIMEOUT => {
                tracing::warn!("no end of speech from Home Assistant; stopping the microphone");
                self.stop_streaming();
                self.phase = Phase::Thinking(now);
            }
            Phase::Thinking(t) if now - t > THINK_TIMEOUT => {
                tracing::warn!("Assist run timed out");
                self.abort_run(true).await;
                self.error_feedback();
            }
            _ => {}
        }
        if !self.boot_done && now - self.booted_at > BOOT_ANIMATION {
            self.boot_done = true;
        }
    }

    fn overlay(&mut self, effect: Effect) {
        self.leds
            .send_modify(|s| s.overlay = Some((effect, Instant::now())));
    }

    /// Recompute derived outputs (LED state, wake word gate) from the state.
    fn refresh(&mut self) {
        let muted = self.muted();
        let base = if !self.boot_done {
            Effect::Boot
        } else if muted {
            Effect::Muted
        } else if self.ringing.is_some() {
            Effect::TimerRinging
        } else {
            match self.phase {
                Phase::Idle => Effect::Idle,
                Phase::Starting(_) | Phase::Listening(_) => Effect::Listening,
                Phase::Thinking(_) => Effect::Thinking,
                Phase::Speaking => Effect::Speaking,
            }
        };
        let light = self.light_on.then(|| {
            let (r, g, b) = self.light_rgb;
            let k = 255.0 * self.light_brightness;
            Rgb {
                r: (r * k).round().clamp(0.0, 255.0) as u8,
                g: (g * k).round().clamp(0.0, 255.0) as u8,
                b: (b * k).round().clamp(0.0, 255.0) as u8,
            }
        });
        let now = Instant::now();
        let timer = self
            .timers
            .values()
            .filter(|t| t.active)
            .map(|t| t.remaining(now) as f32 / t.total as f32)
            .reduce(f32::min);
        self.leds.send_if_modified(|s| {
            let changed = s.base != base || s.light != light || s.timer != timer;
            s.base = base;
            s.light = light;
            s.timer = timer;
            changed
        });
        let wake = self.phase == Phase::Idle
            && !muted
            && self.voice_subscribed
            && self.ringing.is_none()
            && self.boot_done;
        self.shared.wake_enabled.store(wake, Ordering::SeqCst);
    }
}

async fn play_url(
    http: &reqwest::Client,
    speaker: Option<&Speaker>,
    url: &str,
) -> anyhow::Result<()> {
    let speaker = speaker.ok_or_else(|| anyhow::anyhow!("no speaker"))?;
    let resp = http
        .get(url)
        .header("Accept", "audio/wav, audio/mpeg, audio/flac, audio/ogg")
        .send()
        .await?
        .error_for_status()?;
    let bytes = resp.bytes().await?;
    anyhow::ensure!(
        bytes.len() <= MAX_AUDIO_BYTES,
        "audio too large ({} bytes)",
        bytes.len()
    );
    speaker.play_bytes(&bytes).await?;
    Ok(())
}
