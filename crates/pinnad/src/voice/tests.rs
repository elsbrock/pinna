//! Home Assistant event sequences before standalone mode (ADR 0017).

use super::*;

struct Device {
    brain: Brain,
    events: mpsc::Receiver<DeviceEvent>,
    _dir: tempfile::TempDir,
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (esp, events) = EsphomeHandle::detached(64);
        let brain = Brain::new(
            esp,
            None,
            Arc::default(),
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
                settings: Settings::default(),
            },
        );
        Self {
            brain,
            events,
            _dir: dir,
        }
    }

    #[track_caller]
    fn expect(&mut self, expected: &[DeviceEvent]) {
        let mut actual = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            actual.push(event);
        }
        assert_eq!(actual, expected);
    }

    async fn ha(&mut self, cmd: HaCommand) {
        self.brain.handle(Input::Ha(cmd)).await;
    }

    async fn voice(&mut self, event: VoiceEventType, data: &[(&str, &str)]) {
        self.ha(HaCommand::VoiceEvent {
            event,
            data: data.iter().map(|&(k, v)| (k.into(), v.into())).collect(),
        })
        .await;
    }

    async fn wake(&mut self) {
        self.brain
            .handle(Input::Wake {
                id: "alexa_v0.1".into(),
                score: 0.9,
            })
            .await;
    }

    async fn listen(&mut self) {
        self.ha(HaCommand::VoiceSubscribed(true)).await;
        self.wake().await;
        assert!(matches!(self.brain.phase, Phase::Starting(_)));
        assert!(!self.brain.conversation_id.is_empty());
        self.expect(&[
            last_wake(),
            voice_event("wake_word"),
            start(&self.brain.conversation_id, Some("Alexa")),
        ]);
        self.ha(HaCommand::VoiceStarted {
            port: 0,
            error: false,
        })
        .await;
        self.expect(&[voice_event("listening")]);
        assert!(matches!(self.brain.phase, Phase::Listening(_)));
        assert!(self.brain.shared.streaming.load(Ordering::SeqCst));
    }

    async fn reply(&mut self) -> u64 {
        self.voice(VoiceEventType::SttVadEnd, &[]).await;
        assert!(matches!(self.brain.phase, Phase::Thinking(_)));
        assert!(!self.brain.shared.streaming.load(Ordering::SeqCst));
        self.expect(&[voice_event("thinking")]);
        self.voice(VoiceEventType::TtsEnd, &[("url", "http://ha.test/reply")])
            .await;
        let token = self.brain.playback.as_ref().unwrap().0;
        self.brain.handle(Input::PlaybackStarted { token }).await;
        assert_eq!(self.brain.phase, Phase::Speaking);
        self.expect(&[voice_event("replying")]);
        token
    }
}

fn event(entity: &str, kind: &str) -> DeviceEvent {
    DeviceEvent::Event {
        object_id: entity.into(),
        event_type: kind.into(),
    }
}
fn voice_event(kind: &str) -> DeviceEvent {
    event("voice_assistant", kind)
}
fn last_wake() -> DeviceEvent {
    DeviceEvent::TextSensorState {
        object_id: "last_wake_word".into(),
        value: "Alexa".into(),
    }
}
fn start(id: &str, phrase: Option<&str>) -> DeviceEvent {
    DeviceEvent::VoiceStart {
        conversation_id: id.into(),
        wake_word_phrase: phrase.map(str::to_owned),
        use_vad: true,
    }
}

#[tokio::test]
async fn wake_without_subscription_never_starts() {
    let mut d = Device::new();
    d.wake().await;
    d.expect(&[last_wake(), voice_event("wake_word")]);
    assert_eq!(d.brain.phase, Phase::Idle);
}

#[tokio::test]
async fn run_waits_for_reply_completion() {
    let mut d = Device::new();
    d.listen().await;
    let token = d.reply().await;
    d.voice(VoiceEventType::RunEnd, &[]).await;
    assert_eq!(d.brain.phase, Phase::Speaking);
    d.expect(&[]);
    d.brain
        .handle(Input::PlaybackDone { token, ok: true })
        .await;
    d.expect(&[
        DeviceEvent::AnnounceFinished { success: true },
        voice_event("finished"),
    ]);
    assert_eq!(d.brain.phase, Phase::Idle);
}

#[tokio::test]
async fn continuing_run_uses_the_conversation_id_from_home_assistant() {
    let mut d = Device::new();
    d.listen().await;
    d.voice(
        VoiceEventType::IntentEnd,
        &[
            ("continue_conversation", "1"),
            ("conversation_id", "conversation"),
        ],
    )
    .await;
    d.voice(VoiceEventType::RunEnd, &[]).await;
    d.expect(&[voice_event("finished"), start("conversation", None)]);
}

#[tokio::test]
async fn wake_interrupts_the_reply_before_starting_again() {
    let mut d = Device::new();
    d.listen().await;
    d.reply().await;
    d.wake().await;
    d.expect(&[
        DeviceEvent::AnnounceFinished { success: true },
        voice_event("stopped"),
        DeviceEvent::VoiceStop,
        last_wake(),
        voice_event("wake_word"),
        start(&d.brain.conversation_id, Some("Alexa")),
    ]);
}

#[tokio::test]
async fn stop_discards_late_events_through_run_end() {
    let mut d = Device::new();
    d.listen().await;
    let (tx, rx) = oneshot::channel();
    d.brain
        .handle(Input::Api(ApiCommand::StopConversation(tx)))
        .await;
    rx.await.unwrap().unwrap();
    d.expect(&[voice_event("stopped"), DeviceEvent::VoiceStop]);
    d.voice(VoiceEventType::TtsEnd, &[("url", "http://ha.test/stale")])
        .await;
    d.voice(VoiceEventType::RunEnd, &[]).await;
    d.expect(&[]);
    assert_eq!(d.brain.phase, Phase::Idle);
    assert!(d.brain.playback.is_none());
    assert!(!d.brain.shared.streaming.load(Ordering::SeqCst));
    assert!(!d.brain.stale_run);
}

#[tokio::test]
async fn last_client_disconnect_stops_without_sending_voice_stop() {
    let mut d = Device::new();
    d.brain.clients = 1;
    d.listen().await;
    d.ha(HaCommand::ClientDisconnected).await;
    d.expect(&[voice_event("stopped")]);
    assert_eq!(d.brain.phase, Phase::Idle);
}

#[tokio::test]
async fn timer_finish_interrupts_and_action_dismisses() {
    let mut d = Device::new();
    d.listen().await;
    let timer = |event| HaCommand::Timer {
        event,
        id: "tea".into(),
        name: "Tea".into(),
        total_seconds: 10,
        seconds_left: 10,
        is_active: true,
    };
    d.ha(timer(TimerEventType::Started)).await;
    d.expect(&[event("timer", "started")]);
    assert_eq!(d.brain.snapshot().timers[0].id, "tea");
    d.ha(timer(TimerEventType::Finished)).await;
    d.expect(&[
        event("timer", "finished"),
        voice_event("stopped"),
        DeviceEvent::VoiceStop,
    ]);
    assert!(d.brain.snapshot().timers.is_empty());
    assert!(d.brain.snapshot().alarm_ringing);
    d.brain
        .handle(Input::Button(ButtonEvent::Press(Button::Action)))
        .await;
    d.expect(&[event("action", "press")]);
    assert!(!d.brain.snapshot().alarm_ringing);
    assert!(d.brain.playback.is_none());
}

#[tokio::test]
async fn announcement_reports_completion() {
    let mut d = Device::new();
    d.ha(HaCommand::Announce {
        media_id: "http://ha.test/announce".into(),
        text: String::new(),
        preannounce_media_id: String::new(),
        start_conversation: false,
    })
    .await;
    assert_eq!(d.brain.phase, Phase::Speaking);
    d.expect(&[]);
    let token = d.brain.playback.as_ref().unwrap().0;
    d.brain
        .handle(Input::PlaybackDone { token, ok: true })
        .await;
    d.expect(&[DeviceEvent::AnnounceFinished { success: true }]);
    assert_eq!(d.brain.phase, Phase::Idle);
}

#[tokio::test]
async fn connected_client_owns_the_wake_word_selection() {
    let mut d = Device::new();
    d.brain.clients = 1;
    let (tx, rx) = oneshot::channel();
    let patch = SettingsPatch {
        wake_words: Some(vec!["hey_jarvis_v0.1".into()]),
        ..Default::default()
    };
    d.brain
        .handle(Input::Api(ApiCommand::UpdateSettings(patch, tx)))
        .await;
    assert_eq!(
        rx.await.unwrap().unwrap_err(),
        "Home Assistant chooses the wake words while it is connected. Choose them on this satellite's device page there."
    );
    d.expect(&[]);
}

#[tokio::test]
async fn mute_interrupts_listening() {
    let mut d = Device::new();
    d.listen().await;
    d.ha(HaCommand::MuteSwitch(true)).await;
    d.expect(&[
        voice_event("stopped"),
        DeviceEvent::VoiceStop,
        DeviceEvent::MuteState(true),
    ]);
    assert_eq!(d.brain.phase, Phase::Idle);
    assert!(!d.brain.shared.streaming.load(Ordering::SeqCst));
}

#[tokio::test]
async fn opening_the_latch_also_clears_the_software_mute_and_saves_it() {
    let mut d = Device::new();
    d.ha(HaCommand::MuteSwitch(true)).await;
    d.brain.handle(Input::Privacy(true)).await;
    d.brain.local.pending_mute = true;
    assert!(d.brain.settings.muted);

    d.brain.handle(Input::Privacy(false)).await;

    assert!(!d.brain.muted());
    assert!(!d.brain.local.pending_mute);
    assert!(!d.brain.settings.muted);
    assert!(!d.brain.shared.muted.load(Ordering::SeqCst));
}

#[tokio::test]
async fn closing_the_latch_leaves_the_software_mute_alone() {
    let mut d = Device::new();
    d.ha(HaCommand::MuteSwitch(true)).await;

    d.brain.handle(Input::Privacy(true)).await;
    assert!(d.brain.sw_muted);
    d.brain.handle(Input::Privacy(false)).await;
    d.ha(HaCommand::MuteSwitch(true)).await;
    d.brain.handle(Input::Privacy(false)).await;

    assert!(
        d.brain.sw_muted,
        "a reading that is no change keeps a saved mute"
    );
}

#[tokio::test]
async fn thinking_times_out_on_tick() {
    let mut d = Device::new();
    d.brain.phase = Phase::Thinking(Instant::now() - THINK_TIMEOUT - Duration::from_secs(1));
    d.brain.tick().await;
    d.expect(&[voice_event("stopped"), DeviceEvent::VoiceStop]);
    assert_eq!(d.brain.phase, Phase::Idle);
}

#[test]
fn the_mdns_record_follows_the_satellite_group() {
    const ID: &str = "00112233445566778899aabbccddeeff";
    let mut mdns = Some(pinna_esphome::DeviceConfig::new(
        "pinna-test",
        [2, 0, 0, 0, 0, 1],
    ));
    let mut publish = |group: Option<&str>| {
        group_record_changed(&mut mdns, group.map(str::to_owned)).map(|dc| dc.group)
    };
    // The record at boot already says what the group says: nothing to do.
    assert_eq!(publish(None), None);
    // Each change is published once, and an unchanged record never again.
    for group in [Some(""), Some(ID), Some(""), None] {
        assert_eq!(publish(group), Some(group.map(str::to_owned)), "{group:?}");
        assert_eq!(publish(group), None, "{group:?}");
    }
    assert_eq!(mdns.unwrap().group, None);

    // Without mDNS there is no record to write.
    let mut off = None;
    assert!(group_record_changed(&mut off, Some(ID.into())).is_none());
    assert!(off.is_none());
}
