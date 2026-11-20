//! The mode switch, mode-specific readiness and Home Assistant command
//! gating (ADR 0017).

use std::time::{Instant, SystemTime};

use pinna_esphome::{DeviceEvent, HaCommand};

use super::{Brain, Phase, SettingsPatch};
use crate::settings::Mode;

/// Why standalone mode can't be switched on yet.
pub(super) const NOT_READY: &str =
    "Set up speech-to-text, the brain and text-to-speech under Assistant and check them first.";

impl Brain {
    /// Switch between Home Assistant and standalone mode. Whatever runs in
    /// the old mode stops, timers and the conversation are forgotten, and
    /// Home Assistant reconnects to read the new voice features once the
    /// satellite is idle. Media keeps playing. The caller saves the settings.
    pub(super) async fn set_mode(&mut self, new: Mode) -> Result<(), String> {
        // Whoever sets a mode now has the say over a switch still waiting.
        self.pending_mode = None;
        if new == self.mode() {
            return Ok(());
        }
        if new == Mode::Standalone && !self.local.ready() {
            return Err(NOT_READY.into());
        }
        tracing::info!("voice mode: {new:?}");
        if self.phase != Phase::Idle {
            self.interrupt().await;
        }
        if self.ringing.take().is_some() {
            self.stop_playback();
        }
        self.timers.clear();
        self.timer_store
            .clear(Instant::now(), SystemTime::now(), crate::clock::synced());
        self.local.forget_conversation();
        self.continue_conversation = false;
        self.stale_run = false;
        self.settings.mode = new;
        self.send(DeviceEvent::VoiceFeatureFlags(new.voice_feature_flags()))
            .await;
        self.send_mode().await;
        self.reconnect_for_mode = self.clients > 0;
        self.refresh();
        Ok(())
    }

    /// Nothing is running, ringing or playing, and no standalone run is open:
    /// the conditions under which Home Assistant reconnects for a new mode.
    pub(super) fn switch_idle(&self) -> bool {
        self.phase == Phase::Idle
            && self.ringing.is_none()
            && self.playback.is_none()
            && self.local.run.is_none()
    }

    /// The `switch_assistant` tool. `set_mode` interrupts whatever runs, so
    /// a switch asked for during a run waits for the satellite to be idle;
    /// otherwise the run's own reply would be cut off. It is saved the way
    /// the dashboard's and Home Assistant's switch is.
    pub(super) async fn switch_mode(&mut self, mode: Mode) -> Result<(), String> {
        if mode == self.mode() {
            self.pending_mode = None;
            return Ok(());
        }
        if mode == Mode::Standalone && !self.local.ready() {
            return Err(NOT_READY.into());
        }
        if !self.switch_idle() {
            tracing::info!("switching to {} once the satellite is idle", mode.label());
            self.pending_mode = Some(mode);
            return Ok(());
        }
        let patch = SettingsPatch {
            mode: Some(mode),
            ..SettingsPatch::default()
        };
        self.update_settings(patch).await
    }

    /// The "Voice mode" select. A refusal leaves the mode as it was, and the
    /// select goes back to it. Like the tool, it waits until the satellite is
    /// idle: an Assist script that sets it runs inside Home Assistant's own
    /// request, whose spoken reply would otherwise be cut off.
    pub(super) async fn mode_from_ha(&mut self, label: &str) {
        let Some(mode) = Mode::from_label(label) else {
            self.send_mode().await;
            return;
        };
        if let Err(why) = self.switch_mode(mode).await {
            tracing::info!("Home Assistant asked for {label}: {why}");
            self.send_mode().await;
        }
    }

    /// The "Voice mode" select's state.
    pub(super) async fn send_mode(&self) {
        self.send(DeviceEvent::SelectState {
            object_id: "assistant".into(),
            state: self.mode().label().into(),
        })
        .await;
    }

    pub(super) fn mode(&self) -> Mode {
        self.settings.mode
    }

    pub(super) fn assist_ready(&self) -> bool {
        match self.mode() {
            Mode::HomeAssistant => self.voice_subscribed,
            Mode::Standalone => self.local.ready(),
        }
    }

    pub(super) fn ha_allowed(&self, cmd: &HaCommand) -> bool {
        if self.mode() == Mode::HomeAssistant {
            return true;
        }
        !matches!(
            cmd,
            HaCommand::VoiceStarted { .. }
                | HaCommand::VoiceEvent { .. }
                | HaCommand::Timer { .. }
                | HaCommand::SetActiveWakeWords(_)
                | HaCommand::Announce { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use pinna_audio::{MediaPhase, MediaState};
    use pinna_esphome::{EsphomeHandle, TimerEventType, VOICE_FEATURE_FLAGS, VoiceEventType};
    use tokio::sync::{mpsc, oneshot, watch};

    use pinna_assist::presets::Slots;

    use super::*;
    use crate::leds::LedState;
    use crate::settings::Settings;
    use crate::standalone::AssistLive;
    use crate::voice::{
        ApiCommand, Defaults, Input, LocalOptions, NoopSpawner, Options, SettingsPatch, Shared,
        Snapshot,
    };

    struct Device {
        brain: Brain,
        events: mpsc::Receiver<DeviceEvent>,
        live: watch::Sender<Arc<AssistLive>>,
        dir: tempfile::TempDir,
    }

    impl Device {
        fn new(mode: Mode) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (esp, events) = EsphomeHandle::detached(64);
            let (live_tx, live) = watch::channel(Arc::new(AssistLive::empty()));
            let brain = Brain::new_with_local(
                esp,
                None,
                Arc::new(Shared::default()),
                watch::channel(LedState::default()).0,
                watch::channel(Snapshot::default()).0,
                mpsc::channel(64).0,
                Options {
                    key_file: dir.path().join("key"),
                    settings_file: dir.path().join("settings.toml"),
                    wake_words: crate::wakewords::test_catalogue(),
                    active_wake_words: vec!["alexa_v0.1".into()],
                    keywords: None,
                    defaults: Defaults {
                        node_name: "pinna-test".into(),
                        friendly_name: "Test".into(),
                        default_name: true,
                        wake_sound: false,
                        update_channel: crate::updates::UpdateChannel::Off,
                    },
                    updates: None,
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    privacy_trigger: Vec::new(),
                    airplay: false,
                    bluetooth: false,
                    sendspin: None,
                    settings: Settings {
                        mode,
                        ..Settings::default()
                    },
                },
                LocalOptions {
                    live,
                    spawner: Arc::new(NoopSpawner),
                    timers_file: dir.path().join("timers.toml"),
                },
            );
            Self {
                brain,
                events,
                live: live_tx,
                dir,
            }
        }

        fn set_ready(&self, ready: bool) {
            self.live
                .send(Arc::new(AssistLive {
                    ready,
                    slots: Slots::all(ready),
                    label: "test brain".into(),
                    ..AssistLive::empty()
                }))
                .unwrap();
        }

        fn events(&mut self) -> Vec<DeviceEvent> {
            let mut events = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                events.push(event);
            }
            events
        }

        async fn set_mode(&mut self, mode: Mode) -> Result<Snapshot, String> {
            let (reply, result) = oneshot::channel();
            self.brain
                .handle(Input::Api(ApiCommand::UpdateSettings(
                    SettingsPatch {
                        mode: Some(mode),
                        ..SettingsPatch::default()
                    },
                    reply,
                )))
                .await;
            result.await.unwrap()
        }

        fn saved(&self, file: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(file)).unwrap_or_default()
        }

        async fn ha_timer(&mut self, event: TimerEventType, id: &str) {
            self.brain
                .ha(HaCommand::Timer {
                    event,
                    id: id.into(),
                    name: "Tea".into(),
                    total_seconds: 60,
                    seconds_left: 60,
                    is_active: true,
                })
                .await;
        }

        async fn start_conversation(&mut self) -> Result<Snapshot, String> {
            let (reply, result) = oneshot::channel();
            self.brain.api(ApiCommand::StartConversation(reply)).await;
            result.await.unwrap()
        }
    }

    #[test]
    fn readiness_comes_from_the_selected_mode() {
        let mut device = Device::new(Mode::HomeAssistant);
        assert!(!device.brain.assist_ready());
        device.brain.voice_subscribed = true;
        assert!(device.brain.assist_ready());

        device.brain.settings.mode = Mode::Standalone;
        assert!(!device.brain.assist_ready());
        device.set_ready(true);
        assert!(device.brain.assist_ready());
        let assist = device.brain.snapshot().assist;
        assert!(assist.ready);
        assert_eq!(assist.slots, Slots::all(true));
        assert_eq!(assist.label, "test brain");
    }

    #[test]
    fn the_wake_word_gate_uses_the_same_readiness() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.brain.boot_done = true;
        device.brain.refresh();
        assert!(!device.brain.shared.wake_enabled.load(Ordering::SeqCst));

        device.brain.voice_subscribed = true;
        device.brain.refresh();
        assert!(device.brain.shared.wake_enabled.load(Ordering::SeqCst));

        device.brain.settings.mode = Mode::Standalone;
        device.brain.refresh();
        assert!(!device.brain.shared.wake_enabled.load(Ordering::SeqCst));
        device.set_ready(true);
        device.brain.refresh();
        assert!(device.brain.shared.wake_enabled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn start_conversation_explains_the_missing_readiness_in_each_mode() {
        let mut ha = Device::new(Mode::HomeAssistant);
        assert_eq!(
            ha.start_conversation().await.unwrap_err(),
            "Home Assistant has not subscribed to voice assistant runs"
        );

        let mut standalone = Device::new(Mode::Standalone);
        assert_eq!(
            standalone.start_conversation().await.unwrap_err(),
            NOT_READY
        );
    }

    fn mode_states(events: &[DeviceEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                DeviceEvent::SelectState { object_id, state } if object_id == "assistant" => {
                    Some(state.as_str())
                }
                _ => None,
            })
            .collect()
    }

    async fn ha_select(device: &mut Device, state: &str) {
        device
            .brain
            .ha(HaCommand::Select {
                object_id: "assistant".into(),
                state: state.into(),
            })
            .await;
    }

    #[tokio::test]
    async fn the_select_set_during_a_home_assistant_request_waits_until_it_ends() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.set_ready(true);
        device.brain.ha(HaCommand::VoiceSubscribed(true)).await;
        device
            .brain
            .handle(Input::Wake {
                id: "alexa_v0.1".into(),
                score: 0.9,
            })
            .await;
        device
            .brain
            .ha(HaCommand::VoiceStarted {
                port: 0,
                error: false,
            })
            .await;
        device.events();

        // An Assist script sets the select while the request runs.
        ha_select(&mut device, "Standalone").await;
        assert_eq!(device.brain.mode(), Mode::HomeAssistant);
        assert_eq!(device.brain.pending_mode, Some(Mode::Standalone));
        assert!(!device.events().contains(&DeviceEvent::VoiceStop));

        device
            .brain
            .ha(HaCommand::VoiceEvent {
                event: VoiceEventType::RunEnd,
                data: Vec::new(),
            })
            .await;
        assert_eq!(device.brain.phase, Phase::Idle);
        device.brain.tick().await;

        assert_eq!(device.brain.mode(), Mode::Standalone);
        assert_eq!(mode_states(&device.events()), ["Standalone"]);
        assert!(device.saved("settings.toml").contains("standalone"));
    }

    #[tokio::test]
    async fn home_assistant_switches_the_mode_once_standalone_is_ready() {
        let mut device = Device::new(Mode::HomeAssistant);

        // Not ready: refused, and the select goes back.
        ha_select(&mut device, "Standalone").await;
        assert_eq!(device.brain.mode(), Mode::HomeAssistant);
        assert_eq!(mode_states(&device.events()), ["Home Assistant"]);

        device.set_ready(true);
        ha_select(&mut device, "Standalone").await;
        assert_eq!(device.brain.mode(), Mode::Standalone);
        let events = device.events();
        assert_eq!(mode_states(&events), ["Standalone"]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, DeviceEvent::VoiceFeatureFlags(0)))
        );
        assert!(device.saved("settings.toml").contains("standalone"));

        ha_select(&mut device, "Home Assistant").await;
        assert_eq!(device.brain.mode(), Mode::HomeAssistant);
        assert_eq!(mode_states(&device.events()), ["Home Assistant"]);
    }

    #[tokio::test]
    async fn the_select_follows_a_dashboard_switch_and_ignores_an_unknown_option() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.set_ready(true);
        device.set_mode(Mode::Standalone).await.unwrap();
        assert_eq!(mode_states(&device.events()), ["Standalone"]);

        ha_select(&mut device, "Elsewhere").await;
        assert_eq!(device.brain.mode(), Mode::Standalone);
        assert_eq!(mode_states(&device.events()), ["Standalone"]);
    }

    #[tokio::test]
    async fn standalone_ignores_voice_commands_and_refuses_an_announcement() {
        let mut device = Device::new(Mode::Standalone);
        device.brain.phase = Phase::Starting(std::time::Instant::now());

        device
            .brain
            .ha(HaCommand::VoiceStarted {
                port: 0,
                error: false,
            })
            .await;
        assert!(matches!(device.brain.phase, Phase::Starting(_)));
        assert!(!device.brain.shared.streaming.load(Ordering::SeqCst));

        device
            .brain
            .ha(HaCommand::VoiceEvent {
                event: VoiceEventType::RunEnd,
                data: Vec::new(),
            })
            .await;
        assert!(matches!(device.brain.phase, Phase::Starting(_)));

        device
            .brain
            .ha(HaCommand::Timer {
                event: TimerEventType::Started,
                id: "tea".into(),
                name: "Tea".into(),
                total_seconds: 10,
                seconds_left: 10,
                is_active: true,
            })
            .await;
        assert!(device.brain.timers.is_empty());

        device
            .brain
            .ha(HaCommand::SetActiveWakeWords(vec![
                "hey_jarvis_v0.1".into(),
            ]))
            .await;
        assert_eq!(device.brain.active_wake, ["alexa_v0.1".to_string()]);

        device
            .brain
            .ha(HaCommand::Announce {
                media_id: "http://ha.test/announce".into(),
                text: String::new(),
                preannounce_media_id: String::new(),
                start_conversation: false,
            })
            .await;
        assert_eq!(
            device.events(),
            [DeviceEvent::AnnounceFinished { success: false }]
        );
        assert!(device.brain.playback.is_none());
        assert!(matches!(device.brain.phase, Phase::Starting(_)));

        device.brain.ha(HaCommand::VoiceSubscribed(true)).await;
        assert!(device.brain.voice_subscribed);
        assert!(!device.brain.assist_ready());
    }

    #[test]
    fn home_assistant_mode_leaves_the_command_gate_open() {
        let device = Device::new(Mode::HomeAssistant);
        for cmd in [
            HaCommand::VoiceStarted {
                port: 0,
                error: false,
            },
            HaCommand::VoiceEvent {
                event: VoiceEventType::RunEnd,
                data: Vec::new(),
            },
            HaCommand::Timer {
                event: TimerEventType::Started,
                id: "tea".into(),
                name: "Tea".into(),
                total_seconds: 10,
                seconds_left: 10,
                is_active: true,
            },
            HaCommand::SetActiveWakeWords(Vec::new()),
            HaCommand::Announce {
                media_id: "http://ha.test/announce".into(),
                text: String::new(),
                preannounce_media_id: String::new(),
                start_conversation: false,
            },
        ] {
            assert!(device.brain.ha_allowed(&cmd));
        }
    }

    #[tokio::test]
    async fn a_connected_home_assistant_does_not_own_wake_words_in_standalone_mode() {
        let mut device = Device::new(Mode::Standalone);
        device.brain.clients = 1;
        let ids = vec!["hey_jarvis_v0.1".into()];
        let (reply, result) = oneshot::channel();

        device
            .brain
            .api(ApiCommand::UpdateSettings(
                SettingsPatch {
                    wake_words: Some(ids.clone()),
                    ..SettingsPatch::default()
                },
                reply,
            ))
            .await;

        result.await.unwrap().unwrap();
        assert_eq!(device.brain.active_wake, ids);
        assert_eq!(device.events(), [DeviceEvent::ActiveWakeWords(ids)]);
    }

    #[tokio::test]
    async fn standalone_mode_is_refused_until_the_assistant_is_ready() {
        let mut device = Device::new(Mode::HomeAssistant);

        assert_eq!(
            device.set_mode(Mode::Standalone).await.unwrap_err(),
            NOT_READY
        );

        assert_eq!(device.brain.mode(), Mode::HomeAssistant);
        assert!(device.events().is_empty());
        assert!(!device.saved("settings.toml").contains("mode"));
    }

    #[tokio::test]
    async fn a_switch_with_a_refused_setting_changes_nothing() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.set_ready(true);
        let (reply, result) = oneshot::channel();
        device
            .brain
            .handle(Input::Api(ApiCommand::UpdateSettings(
                SettingsPatch {
                    mode: Some(Mode::Standalone),
                    airplay: Some(true),
                    ..SettingsPatch::default()
                },
                reply,
            )))
            .await;

        assert!(result.await.unwrap().is_err());
        assert_eq!(device.brain.mode(), Mode::HomeAssistant);
        assert!(device.events().is_empty());
        assert!(!device.saved("settings.toml").contains("mode"));
    }

    #[tokio::test]
    async fn a_switch_stops_a_home_assistant_run_before_the_voice_features_change() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.brain.ha(HaCommand::VoiceSubscribed(true)).await;
        device
            .brain
            .handle(Input::Wake {
                id: "alexa_v0.1".into(),
                score: 0.9,
            })
            .await;
        device
            .brain
            .ha(HaCommand::VoiceStarted {
                port: 0,
                error: false,
            })
            .await;
        assert!(matches!(device.brain.phase, Phase::Listening(_)));
        device.events();
        device.set_ready(true);

        let snapshot = device.set_mode(Mode::Standalone).await.unwrap();

        assert_eq!(
            device.events(),
            [
                DeviceEvent::Event {
                    object_id: "voice_assistant".into(),
                    event_type: "stopped".into(),
                },
                DeviceEvent::VoiceStop,
                DeviceEvent::VoiceFeatureFlags(0),
                DeviceEvent::SelectState {
                    object_id: "assistant".into(),
                    state: "Standalone".into(),
                },
            ]
        );
        assert_eq!(snapshot.mode, Mode::Standalone);
        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(!device.brain.stale_run);
        assert!(!device.brain.shared.streaming.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_switch_cancels_a_local_run_without_telling_home_assistant() {
        let mut device = Device::new(Mode::Standalone);
        device.set_ready(true);
        device.start_conversation().await.unwrap();
        assert!(device.brain.local.run.is_some());
        device.events();

        device.set_mode(Mode::HomeAssistant).await.unwrap();

        assert!(device.brain.local.run.is_none());
        assert_eq!(device.brain.phase, Phase::Idle);
        let events = device.events();
        assert!(!events.contains(&DeviceEvent::VoiceStop));
        assert_eq!(
            events[events.len() - 2..],
            [
                DeviceEvent::VoiceFeatureFlags(VOICE_FEATURE_FLAGS),
                DeviceEvent::SelectState {
                    object_id: "assistant".into(),
                    state: "Home Assistant".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_switch_clears_every_timer_and_stops_a_ringing_alarm() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.ha_timer(TimerEventType::Started, "tea").await;
        device.ha_timer(TimerEventType::Started, "eggs").await;
        device.ha_timer(TimerEventType::Finished, "eggs").await;
        assert!(device.brain.ringing.is_some());
        assert_eq!(device.brain.timers.len(), 1);
        device.set_ready(true);

        device.set_mode(Mode::Standalone).await.unwrap();

        assert!(device.brain.timers.is_empty());
        assert!(device.brain.ringing.is_none());
        assert!(device.brain.playback.is_none());
        assert!(!device.brain.snapshot().alarm_ringing);

        // A device timer goes with the next switch, from memory and from disk.
        let (reply, id) = oneshot::channel();
        device
            .brain
            .handle(Input::Api(ApiCommand::SetTimer {
                name: Some("Tea".into()),
                seconds: 60,
                reply,
            }))
            .await;
        id.await.unwrap().unwrap();
        assert!(device.saved("timers.toml").contains("[[timer]]"));

        device.set_mode(Mode::HomeAssistant).await.unwrap();

        assert!(device.brain.timers.is_empty());
        assert!(!device.saved("timers.toml").contains("[[timer]]"));
    }

    #[tokio::test]
    async fn home_assistant_reconnects_once_idle_even_while_media_plays() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.brain.clients = 1;
        device
            .brain
            .handle(Input::Media(MediaState {
                phase: MediaPhase::Playing,
                url: Some("http://radio.test/stream".into()),
                ..MediaState::default()
            }))
            .await;
        assert!(!device.brain.quiet());
        device.set_ready(true);

        device.set_mode(Mode::Standalone).await.unwrap();
        assert_eq!(
            device.events(),
            [
                DeviceEvent::VoiceFeatureFlags(0),
                DeviceEvent::SelectState {
                    object_id: "assistant".into(),
                    state: "Standalone".into(),
                }
            ]
        );

        device.brain.tick().await;
        assert!(device.events().contains(&DeviceEvent::Disconnect));
        device.brain.tick().await;
        assert!(!device.events().contains(&DeviceEvent::Disconnect));
    }

    impl Device {
        async fn switch_mode(&mut self, mode: Mode) -> Result<Snapshot, String> {
            let (reply, result) = oneshot::channel();
            self.brain
                .handle(Input::Api(ApiCommand::SwitchMode(mode, reply)))
                .await;
            result.await.unwrap()
        }
    }

    #[tokio::test]
    async fn switching_to_standalone_is_refused_until_it_is_ready() {
        let mut device = Device::new(Mode::HomeAssistant);

        assert_eq!(
            device.switch_mode(Mode::Standalone).await.unwrap_err(),
            NOT_READY
        );

        assert_eq!(device.brain.mode(), Mode::HomeAssistant);
        assert_eq!(device.brain.pending_mode, None);
    }

    #[tokio::test]
    async fn an_idle_satellite_switches_at_once_and_saves_it() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.set_ready(true);

        let snapshot = device.switch_mode(Mode::Standalone).await.unwrap();

        assert_eq!(snapshot.mode, Mode::Standalone);
        assert_eq!(device.brain.pending_mode, None);
        assert!(
            device
                .saved("settings.toml")
                .contains("mode = \"standalone\"")
        );
    }

    #[tokio::test]
    async fn asking_for_the_mode_it_is_in_changes_nothing() {
        let mut device = Device::new(Mode::Standalone);
        device.set_ready(true);
        device.brain.pending_mode = Some(Mode::HomeAssistant);

        device.switch_mode(Mode::Standalone).await.unwrap();

        assert_eq!(device.brain.mode(), Mode::Standalone);
        assert_eq!(device.brain.pending_mode, None);
        assert!(device.events().is_empty());
    }

    #[tokio::test]
    async fn switching_back_restores_the_voice_features_and_the_settings_file() {
        let mut device = Device::new(Mode::HomeAssistant);
        device.set_ready(true);

        device.set_mode(Mode::Standalone).await.unwrap();
        assert_eq!(
            device.events(),
            [
                DeviceEvent::VoiceFeatureFlags(0),
                DeviceEvent::SelectState {
                    object_id: "assistant".into(),
                    state: "Standalone".into(),
                }
            ]
        );
        assert!(
            device
                .saved("settings.toml")
                .contains("mode = \"standalone\"")
        );

        device.set_mode(Mode::HomeAssistant).await.unwrap();
        assert_eq!(
            device.events(),
            [
                DeviceEvent::VoiceFeatureFlags(VOICE_FEATURE_FLAGS),
                DeviceEvent::SelectState {
                    object_id: "assistant".into(),
                    state: "Home Assistant".into(),
                }
            ]
        );
        assert!(!device.saved("settings.toml").contains("mode"));
        assert_eq!(device.brain.mode(), Mode::HomeAssistant);
    }
}
