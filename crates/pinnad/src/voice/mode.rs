//! Mode-specific readiness and Home Assistant command gating (ADR 0017).

use pinna_esphome::HaCommand;

use super::Brain;
use crate::settings::Mode;

impl Brain {
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

    use pinna_esphome::{DeviceEvent, EsphomeHandle, TimerEventType, VoiceEventType};
    use tokio::sync::{mpsc, oneshot, watch};

    use super::*;
    use crate::leds::LedState;
    use crate::settings::Settings;
    use crate::standalone::AssistLive;
    use crate::voice::{
        ApiCommand, Defaults, LocalOptions, NoopSpawner, Options, Phase, SettingsPatch, Shared,
        Snapshot,
    };

    struct Device {
        brain: Brain,
        events: mpsc::Receiver<DeviceEvent>,
        live: watch::Sender<Arc<AssistLive>>,
        _dir: tempfile::TempDir,
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
                    },
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    airplay: false,
                    bluetooth: false,
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
                _dir: dir,
            }
        }

        fn set_ready(&self, ready: bool) {
            self.live
                .send(Arc::new(AssistLive {
                    ready,
                    slots: [ready; 3],
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
        assert_eq!(assist.slots, [true; 3]);
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
            "Set up speech-to-text, the brain and text-to-speech under Assistant and check them first."
        );
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
}
