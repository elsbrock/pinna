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
                mute_lamp: None,
                airplay: false,
                bluetooth: false,
                sendspin: None,
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
async fn a_confirmation_takes_the_action_press_for_itself() {
    let mut d = Device::new();
    d.listen().await;
    d.ha(HaCommand::Timer {
        event: TimerEventType::Finished,
        id: "tea".into(),
        name: "Tea".into(),
        total_seconds: 10,
        seconds_left: 0,
        is_active: true,
    })
    .await;
    while d.events.try_recv().is_ok() {}
    assert!(d.brain.snapshot().alarm_ringing);
    d.brain.sw_muted = true;
    d.brain.boot_done = true;

    let (pressed, press) = oneshot::channel();
    d.brain
        .handle(Input::Presence(Presence {
            until: Instant::now() + Duration::from_secs(60),
            purpose: crate::auth::Purpose::Password,
            pressed,
        }))
        .await;
    d.brain.refresh();
    // The ring shows it, above the muted ring.
    assert_eq!(d.brain.leds.borrow().base, Effect::Confirm);

    d.brain
        .handle(Input::Button(ButtonEvent::Press(Button::Action)))
        .await;
    press.await.expect("the press reached the confirmation");
    // Held on past the long-press time and let go: still the confirmation's.
    d.brain
        .handle(Input::Button(ButtonEvent::LongPress(Button::Action)))
        .await;
    d.brain
        .handle(Input::Button(ButtonEvent::Release(Button::Action)))
        .await;
    // It stopped no alarm and told Home Assistant nothing.
    assert!(d.brain.snapshot().alarm_ringing);
    d.expect(&[]);
    d.brain.refresh();
    assert_ne!(d.brain.leds.borrow().base, Effect::Confirm);

    // The next press is the satellite's again.
    d.brain
        .handle(Input::Button(ButtonEvent::Press(Button::Action)))
        .await;
    d.expect(&[event("action", "press")]);
}

#[tokio::test]
async fn a_confirmation_nobody_presses_ends_on_its_own() {
    let mut d = Device::new();
    let (pressed, press) = oneshot::channel();
    d.brain
        .handle(Input::Presence(Presence {
            until: Instant::now() + Duration::from_secs(60),
            purpose: crate::auth::Purpose::Password,
            pressed,
        }))
        .await;
    assert!(d.brain.presence.is_some());
    // The asker went away: nothing waits for the press any more.
    drop(press);
    d.brain.tick().await;
    assert!(d.brain.presence.is_none());

    let (pressed, _press) = oneshot::channel();
    d.brain
        .handle(Input::Presence(Presence {
            until: Instant::now(),
            purpose: crate::auth::Purpose::Password,
            pressed,
        }))
        .await;
    d.brain.tick().await;
    assert!(d.brain.presence.is_none(), "the minute is over");
}

#[tokio::test]
async fn the_ring_stops_when_the_waiting_side_goes_away() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    let (pressed, press) = oneshot::channel();
    d.brain
        .handle(Input::Presence(Presence {
            until: Instant::now() + Duration::from_secs(60),
            purpose: crate::auth::Purpose::Password,
            pressed,
        }))
        .await;
    d.brain.refresh();
    assert_eq!(d.brain.leds.borrow().base, Effect::Confirm);
    // The confirmation was canceled or stopped: the next tick ends the ring,
    // not the next button event.
    drop(press);
    d.brain.tick().await;
    d.brain.refresh();
    assert_ne!(d.brain.leds.borrow().base, Effect::Confirm);

    let (pressed, _press) = oneshot::channel();
    d.brain
        .handle(Input::Presence(Presence {
            until: Instant::now() - Duration::from_millis(1),
            purpose: crate::auth::Purpose::Password,
            pressed,
        }))
        .await;
    d.brain.tick().await;
    d.brain.refresh();
    assert_ne!(d.brain.leds.borrow().base, Effect::Confirm);
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

/// The run loop rewrites the record when the standalone service publishes
/// another group, and only then (ADR 0026).
#[tokio::test]
async fn the_run_loop_rewrites_the_record_when_the_group_changes() {
    use crate::group::{Joined, Persisted, seal::Key};
    use crate::standalone::AssistLive;
    const ID: &str = "00112233445566778899aabbccddeeff";

    let Device {
        mut brain,
        events: _events,
        _dir,
    } = Device::new();
    let (live, live_rx) = watch::channel(Arc::new(AssistLive::empty()));
    brain.local = local::Local::new(live_rx, Arc::new(local::NoopSpawner));
    brain.opts.mdns = Some(pinna_esphome::DeviceConfig::new(
        "pinna-test",
        [2, 0, 0, 0, 0, 1],
    ));
    let (written_tx, mut written) = mpsc::unbounded_channel();
    brain.mdns_writer = crate::identity::RecordWriter::with(move |dc| {
        let _ = written_tx.send(dc.group.clone());
        Ok(())
    });
    let publish = |group: Persisted| {
        live.send_replace(Arc::new(AssistLive {
            group,
            ..AssistLive::empty()
        }));
    };
    let on = Persisted {
        on: true,
        ..Persisted::default()
    };
    let in_group = Persisted {
        joined: Some(Joined {
            id: ID.into(),
            key: Key::new([9; 32]),
            members: Vec::new(),
        }),
        ..on.clone()
    };

    let (_inputs, inputs_rx) = mpsc::channel(8);
    tokio::select! {
        () = brain.run(inputs_rx) => panic!("the Brain stopped"),
        () = async {
            const WRITE: Duration = Duration::from_secs(5);
            publish(on.clone());
            let record = tokio::time::timeout(WRITE, written.recv()).await;
            assert_eq!(record.unwrap(), Some(Some(String::new())));
            publish(in_group.clone());
            let record = tokio::time::timeout(WRITE, written.recv()).await;
            assert_eq!(record.unwrap(), Some(Some(ID.to_owned())));
            // The same group published again rewrites nothing: the loop
            // passes at least twice a second.
            publish(in_group.clone());
            let quiet = tokio::time::timeout(Duration::from_millis(700), written.recv()).await;
            assert!(quiet.is_err(), "an unchanged record was written");
            publish(Persisted::default());
            let record = tokio::time::timeout(WRITE, written.recv()).await;
            assert_eq!(record.unwrap(), Some(None));
        } => {}
    }
}

/// Home Assistant's *Group messages* switch (ADR 0026): it reports the
/// setting when Home Assistant connects, and a change is saved and echoed.
#[tokio::test]
async fn group_messages_is_a_home_assistant_switch() {
    let mut d = Device::new();
    let state = |on| DeviceEvent::SwitchState {
        object_id: "group_messages".into(),
        on,
    };
    d.ha(HaCommand::ClientConnected {
        peer: "192.0.2.1:6053".into(),
    })
    .await;
    let mut events = Vec::new();
    while let Ok(event) = d.events.try_recv() {
        events.push(event);
    }
    assert!(events.contains(&state(true)), "{events:?}");

    d.ha(HaCommand::Switch {
        object_id: "group_messages".into(),
        on: false,
    })
    .await;
    d.expect(&[state(false)]);
    assert!(!d.brain.settings.group_messages());
    let saved = std::fs::read_to_string(d._dir.path().join("settings.toml")).unwrap();
    assert!(saved.contains("group_messages = false"), "{saved}");
}

/// Home Assistant's *Include in the statistics* switch (ADR 0039): on until
/// switched off, and the dashboard's change reaches Home Assistant.
#[tokio::test]
async fn include_in_the_statistics_is_a_home_assistant_switch() {
    let mut d = Device::new();
    let state = |on| DeviceEvent::SwitchState {
        object_id: "include_in_statistics".into(),
        on,
    };
    d.ha(HaCommand::ClientConnected {
        peer: "192.0.2.1:6053".into(),
    })
    .await;
    let mut events = Vec::new();
    while let Ok(event) = d.events.try_recv() {
        events.push(event);
    }
    assert!(events.contains(&state(true)), "{events:?}");

    d.ha(HaCommand::Switch {
        object_id: "include_in_statistics".into(),
        on: false,
    })
    .await;
    d.expect(&[state(false)]);
    assert!(!d.brain.settings.include_in_statistics());
    let saved = std::fs::read_to_string(d._dir.path().join("settings.toml")).unwrap();
    assert!(saved.contains("include_in_statistics = false"), "{saved}");

    // From the dashboard.
    d.brain
        .update_settings(SettingsPatch {
            include_in_statistics: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    d.expect(&[state(true)]);
    assert!(d.brain.settings.include_in_statistics());
}

/// Home Assistant's *Install updates* select and the window's two times
/// (ADR 0022): they report the setting when Home Assistant connects, a change
/// is saved and echoed, and a window without length is refused.
#[tokio::test]
async fn install_updates_and_the_window_are_home_assistant_entities() {
    let mut d = Device::new();
    let mode = |label: &str| DeviceEvent::SelectState {
        object_id: "install_updates".into(),
        state: label.into(),
    };
    let time = |object_id: &str, hour, minute| DeviceEvent::TimeState {
        object_id: object_id.into(),
        hour,
        minute,
    };
    d.ha(HaCommand::ClientConnected {
        peer: "192.0.2.1:6053".into(),
    })
    .await;
    let mut events = Vec::new();
    while let Ok(event) = d.events.try_recv() {
        events.push(event);
    }
    assert!(events.contains(&mode("Manually")), "{events:?}");
    assert!(events.contains(&time("install_window_start", 3, 0)));
    assert!(events.contains(&time("install_window_end", 5, 0)));

    d.ha(HaCommand::Select {
        object_id: "install_updates".into(),
        state: "Automatically".into(),
    })
    .await;
    d.expect(&[mode("Automatically")]);
    d.ha(HaCommand::TimeOfDay {
        object_id: "install_window_end".into(),
        hour: 1,
        minute: 30,
        second: 0,
    })
    .await;
    d.expect(&[
        time("install_window_start", 3, 0),
        time("install_window_end", 1, 30),
    ]);
    // Start equal to end has no length: the entity shows the old time again.
    d.ha(HaCommand::TimeOfDay {
        object_id: "install_window_start".into(),
        hour: 1,
        minute: 30,
        second: 0,
    })
    .await;
    d.expect(&[
        time("install_window_start", 3, 0),
        time("install_window_end", 1, 30),
    ]);
    let saved = std::fs::read_to_string(d._dir.path().join("settings.toml")).unwrap();
    assert!(
        saved.contains("install_updates = \"automatically\""),
        "{saved}"
    );
    assert!(
        saved.contains("install_window_start = \"03:00\""),
        "{saved}"
    );
    assert!(saved.contains("install_window_end = \"01:30\""), "{saved}");
}

// --- Sendspin (ADR 0027) -----------------------------------------------------

use crate::sendspin::{Asked, Control, Report};
use pinna_esphome::MediaPlayerCommand;

impl Device {
    /// With a stand-in for the Sendspin service that answers pause, play and
    /// stop with `controls`, as `run` would start the service.
    fn with_sendspin(
        controls: bool,
    ) -> (Self, watch::Sender<Report>, mpsc::UnboundedReceiver<Asked>) {
        let mut d = Self::new();
        let (handle, report, asked) = crate::sendspin::fake(controls);
        d.brain.sendspin = Some(handle);
        d.brain.sendspin_report = report.subscribe();
        d.brain.sendspin_available = true;
        (d, report, asked)
    }

    async fn media(&mut self, command: Option<MediaPlayerCommand>, volume: Option<f32>) {
        self.ha(HaCommand::MediaPlayer {
            command,
            volume,
            media_url: None,
            announcement: false,
        })
        .await;
    }

    async fn lane(&mut self, state: MediaState) {
        self.brain.handle(Input::Media(state)).await;
    }

    /// A new report, and the run loop's answer to it.
    fn report(&mut self, report: &watch::Sender<Report>, change: impl FnOnce(&mut Report)) {
        report.send_modify(change);
        self.brain.update_media_state();
    }
}

/// What the Brain asked of the Sendspin service since the last call.
async fn asked(rx: &mut mpsc::UnboundedReceiver<Asked>) -> Vec<Asked> {
    // The stand-in's task takes what was sent once this task yields.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// The media lane as it shows a Sendspin stream.
fn sendspin_stream(phase: MediaPhase) -> MediaState {
    MediaState {
        phase,
        url: Some(STREAM_URL.into()),
        source: "sendspin".into(),
        track: pinna_audio::Track {
            title: "Song".into(),
            artist: "Artist".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn radio(phase: MediaPhase) -> MediaState {
    MediaState {
        phase,
        url: Some("http://radio.test/live.mp3".into()),
        ..Default::default()
    }
}

/// The "Sendspin" switch: reported when Home Assistant connects, saved, and
/// handed to the service under the satellite's name, which a rename follows.
#[tokio::test]
async fn sendspin_is_a_home_assistant_switch() {
    let (mut d, _report, mut rx) = Device::with_sendspin(true);
    let state = |on| DeviceEvent::SwitchState {
        object_id: "sendspin".into(),
        on,
    };
    d.ha(HaCommand::ClientConnected {
        peer: "192.0.2.1:6053".into(),
    })
    .await;
    let events: Vec<_> = std::iter::from_fn(|| d.events.try_recv().ok()).collect();
    assert!(events.contains(&state(false)), "{events:?}");

    d.ha(HaCommand::Switch {
        object_id: "sendspin".into(),
        on: true,
    })
    .await;
    d.expect(&[state(true)]);
    assert_eq!(asked(&mut rx).await, [Asked::Enable(Some("Test".into()))]);
    assert!(d.brain.snapshot().settings.sendspin);
    let saved = std::fs::read_to_string(d._dir.path().join("settings.toml")).unwrap();
    assert!(saved.contains("sendspin = true"), "{saved}");

    let (reply, result) = oneshot::channel();
    d.brain
        .handle(Input::Api(ApiCommand::UpdateSettings(
            SettingsPatch {
                friendly_name: Some("Kitchen".into()),
                ..SettingsPatch::default()
            },
            reply,
        )))
        .await;
    result.await.unwrap().unwrap();
    assert_eq!(asked(&mut rx).await, [Asked::Rename("Kitchen".into())]);

    // The rename's own events (the friendly name) are not this test's.
    while d.events.try_recv().is_ok() {}
    d.ha(HaCommand::Switch {
        object_id: "sendspin".into(),
        on: false,
    })
    .await;
    d.expect(&[state(false)]);
    assert_eq!(asked(&mut rx).await, [Asked::Enable(None)]);
    let saved = std::fs::read_to_string(d._dir.path().join("settings.toml")).unwrap();
    assert!(saved.contains("sendspin = false"), "{saved}");
}

/// Without a speaker the switch stays off, and the dashboard is told why.
#[tokio::test]
async fn sendspin_is_refused_without_a_speaker() {
    let mut d = Device::new();
    d.ha(HaCommand::Switch {
        object_id: "sendspin".into(),
        on: true,
    })
    .await;
    d.expect(&[DeviceEvent::SwitchState {
        object_id: "sendspin".into(),
        on: false,
    }]);
    let (reply, result) = oneshot::channel();
    d.brain
        .handle(Input::Api(ApiCommand::UpdateSettings(
            SettingsPatch {
                sendspin: Some(true),
                ..SettingsPatch::default()
            },
            reply,
        )))
        .await;
    assert_eq!(
        result.await.unwrap().unwrap_err(),
        "This satellite has no working speaker, so it can't play from Music Assistant. \
         Logs, under System, says why."
    );
    assert!(!d.brain.snapshot().settings.sendspin);
}

/// The server's time is the local monotonic time, a second on.
struct SameClock(Instant);

impl pinna_audio::ServerClock for SameClock {
    fn server_us(&self, local: Instant) -> Option<i64> {
        Some(local.saturating_duration_since(self.0).as_micros() as i64 + 1_000_000)
    }

    fn rate(&self) -> f64 {
        1.0
    }
}

/// Pause while a Sendspin stream plays pauses the whole group through the
/// server (ADR 0027 §14), and leaves the lane alone: the server ends the
/// stream. Only a server that can't take it has the lane pause, which ends the
/// stream here alone.
#[tokio::test]
async fn pause_while_sendspin_plays_goes_to_the_group_not_the_lane() {
    for controls in [true, false] {
        let (mut d, _report, mut rx) = Device::with_sendspin(controls);
        let speaker = Arc::new(Speaker::new(pinna_audio::SpeakerConfig::new(
            pinna_audio::Backend::WavFile(d._dir.path().join("out.wav")),
        )));
        d.brain.speaker = Some(speaker.clone());
        let clock = Arc::new(SameClock(Instant::now()));
        let sink = speaker.media().begin_timed("sendspin", clock.clone());
        let start = pinna_audio::ServerClock::server_us(&*clock, Instant::now()).unwrap() + 300_000;
        for i in 0..40 {
            sink.push(start + i * 50_000, 48_000, vec![0; 2 * 2_400])
                .unwrap();
        }
        let mut state = sink.state();
        tokio::time::timeout(
            Duration::from_secs(2),
            state.wait_for(|s| *s == pinna_audio::TimedState::Playing),
        )
        .await
        .expect("plays")
        .unwrap();
        let lane = speaker.media().state().borrow().clone();
        d.lane(lane).await;

        d.media(Some(MediaPlayerCommand::Pause), None).await;

        assert_eq!(asked(&mut rx).await, [Asked::Control(Control::Pause)]);
        let paused = tokio::time::timeout(
            Duration::from_millis(500),
            state
                .wait_for(|s| *s == pinna_audio::TimedState::Ended(pinna_audio::EndReason::Paused)),
        )
        .await
        .is_ok();
        assert_eq!(paused, !controls, "the lane paused it");
    }
}

/// A paused group shows as Paused, with what it played, and Play resumes the
/// group; stop goes to it too.
#[tokio::test]
async fn a_paused_sendspin_group_shows_paused_and_play_resumes_it() {
    let (mut d, report, mut rx) = Device::with_sendspin(true);
    d.lane(sendspin_stream(MediaPhase::Playing)).await;
    // The server ended the stream; then its group says it stopped.
    d.lane(MediaState::default()).await;
    assert_eq!(d.brain.media_state, MediaPlayerStateKind::Idle);
    d.report(&report, |r| r.group_paused = true);

    assert_eq!(d.brain.media_state, MediaPlayerStateKind::Paused);
    let m = d.brain.snapshot().media;
    assert_eq!(
        (m.source.as_str(), m.track.title.as_str(), m.stream),
        ("sendspin", "Song", false)
    );
    d.media(Some(MediaPlayerCommand::Play), None).await;
    assert_eq!(asked(&mut rx).await, [Asked::Control(Control::Play)]);
    d.media(Some(MediaPlayerCommand::Stop), None).await;
    assert_eq!(asked(&mut rx).await, [Asked::Control(Control::Stop)]);

    // The group plays elsewhere, or the connection ended: nothing to show.
    d.report(&report, |r| r.group_paused = false);
    assert_eq!(d.brain.media_state, MediaPlayerStateKind::Idle);
    assert_eq!(d.brain.snapshot().media.source, "");
    d.media(Some(MediaPlayerCommand::Play), None).await;
    assert!(asked(&mut rx).await.is_empty());
}

/// "Cancel request" stops what plays: a Sendspin stream's whole group.
#[tokio::test]
async fn cancel_request_stops_the_sendspin_group() {
    let (mut d, _report, mut rx) = Device::with_sendspin(true);
    d.lane(sendspin_stream(MediaPhase::Playing)).await;
    d.ha(HaCommand::Button {
        object_id: "cancel_request".into(),
    })
    .await;
    assert_eq!(asked(&mut rx).await, [Asked::Control(Control::Stop)]);
}

/// Another source taking the lane while the group is paused leaves the
/// group, so that the group's next play doesn't cut it off (ADR 0027 §14).
#[tokio::test]
async fn another_source_leaves_a_paused_sendspin_group() {
    let (mut d, report, mut rx) = Device::with_sendspin(true);
    d.report(&report, |r| r.group_paused = true);
    // The group playing again is no other source.
    d.lane(sendspin_stream(MediaPhase::Buffering)).await;
    assert!(asked(&mut rx).await.is_empty());

    // A URL that is still opening hasn't taken the lane: one that fails to
    // open changes nothing (ADR 0011 §6).
    d.lane(radio(MediaPhase::Buffering)).await;
    assert!(asked(&mut rx).await.is_empty());
    d.lane(MediaState {
        error: Some("HTTP 404 Not Found".into()),
        ..radio(MediaPhase::Idle)
    })
    .await;
    assert!(asked(&mut rx).await.is_empty());

    d.lane(radio(MediaPhase::Playing)).await;
    assert_eq!(asked(&mut rx).await, [Asked::AnotherSource]);
    let m = d.brain.snapshot().media;
    assert_eq!(
        (m.state, m.source.as_str()),
        (MediaPlayerStateKind::Playing, "")
    );

    // Outside a paused group, a source is just a source.
    d.report(&report, |r| r.group_paused = false);
    d.lane(radio(MediaPhase::Playing)).await;
    assert!(asked(&mut rx).await.is_empty());
}

/// While Sendspin's stream plays, the media player's volume and mute are the
/// stream's own, which the server hears (ADR 0027 §15); the speaker's volume
/// stays where it is.
#[tokio::test]
async fn the_media_volume_is_sendspins_while_it_plays() {
    let (mut d, report, mut rx) = Device::with_sendspin(true);
    d.report(&report, |r| r.volume = 30);
    d.lane(sendspin_stream(MediaPhase::Playing)).await;
    assert_eq!(d.brain.snapshot().media.volume, 30);

    d.media(None, Some(0.6)).await;
    d.report(&report, |r| r.volume = 60);
    d.media(Some(MediaPlayerCommand::VolumeUp), None).await;
    d.media(Some(MediaPlayerCommand::Mute), None).await;
    assert_eq!(
        asked(&mut rx).await,
        [Asked::Volume(60), Asked::Volume(65), Asked::Muted(true)]
    );
    d.report(&report, |r| r.muted = true);
    assert!(d.brain.snapshot().media.muted);
    assert_eq!((d.brain.settings.volume, d.brain.stream_volume), (50, 100));

    // Another source plays: the media player's volume is the speaker's again.
    d.lane(radio(MediaPhase::Playing)).await;
    let m = d.brain.snapshot().media;
    assert_eq!((m.volume, m.muted), (50, false));
    d.media(None, Some(0.2)).await;
    assert!(asked(&mut rx).await.is_empty());
    assert_eq!(d.brain.settings.volume, 20);
}

/// While the Sendspin group only waits paused, the media player's volume and
/// mute are the speaker's, as on any idle satellite: a volume set before an
/// announcement reaches the speaker, not the paused stream (ADR 0027 §15).
#[tokio::test]
async fn the_media_volume_is_the_speakers_while_the_sendspin_group_is_paused() {
    let (mut d, report, mut rx) = Device::with_sendspin(true);
    d.report(&report, |r| {
        r.volume = 30;
        r.muted = true;
    });
    d.lane(sendspin_stream(MediaPhase::Playing)).await;
    d.lane(MediaState::default()).await;
    d.report(&report, |r| r.group_paused = true);
    assert_eq!(d.brain.media_state, MediaPlayerStateKind::Paused);
    let m = d.brain.snapshot().media;
    assert_eq!((m.volume, m.muted), (50, false));

    d.media(None, Some(0.2)).await;
    d.media(Some(MediaPlayerCommand::VolumeUp), None).await;
    d.media(Some(MediaPlayerCommand::Mute), None).await;
    assert!(asked(&mut rx).await.is_empty());
    assert_eq!(d.brain.settings.volume, 25);

    // The group plays again: the stream's own level is back.
    d.report(&report, |r| r.group_paused = false);
    d.lane(sendspin_stream(MediaPhase::Playing)).await;
    let m = d.brain.snapshot().media;
    assert_eq!((m.volume, m.muted), (30, true));
}

#[tokio::test]
async fn a_recording_confirmation_shows_its_own_pattern_and_takes_the_press_the_same_way() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    let (pressed, press) = oneshot::channel();
    d.brain
        .handle(Input::Presence(Presence {
            until: Instant::now() + Duration::from_secs(60),
            purpose: crate::auth::Purpose::Recording,
            pressed,
        }))
        .await;
    d.brain.refresh();
    assert_eq!(d.brain.leds.borrow().base, Effect::ConfirmRecording);
    d.brain
        .handle(Input::Button(ButtonEvent::Press(Button::Action)))
        .await;
    press.await.expect("the press reached the confirmation");
    // It started no request.
    assert_eq!(d.brain.phase, Phase::Idle);
    d.brain.refresh();
    assert_eq!(d.brain.leds.borrow().base, Effect::Idle);
}

#[tokio::test]
async fn a_recording_shows_listening_below_everything_and_leaves_the_phase_alone() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    let from = "192.168.1.9".parse().unwrap();
    d.brain
        .handle(Input::RecordingStarted {
            id: 1,
            until: Instant::now() + Duration::from_secs(30),
            from,
            kind: RecordingKind::Audio,
        })
        .await;
    d.brain.refresh();
    assert_eq!(d.brain.leds.borrow().base, Effect::Listening);
    assert_eq!(d.brain.phase, Phase::Idle);
    let s = d.brain.snapshot();
    assert_eq!(s.phase, pinna_api::Phase::PHASE_IDLE);
    assert_eq!(s.recording.map(|r| r.from), Some(from));

    // Muted keeps its own pattern.
    d.brain.sw_muted = true;
    d.brain.refresh();
    assert_eq!(d.brain.leds.borrow().base, Effect::Muted);
    d.brain.sw_muted = false;

    // The wake word still starts a request, which shows its own pattern.
    d.ha(HaCommand::VoiceSubscribed(true)).await;
    d.wake().await;
    assert!(matches!(d.brain.phase, Phase::Starting(_)));
    d.brain.refresh();
    assert!(d.brain.snapshot().recording.is_some());

    // Another recording's end doesn't end this one; its own does.
    d.brain.handle(Input::RecordingEnded { id: 2 }).await;
    assert!(d.brain.recording.is_some());
    d.brain.handle(Input::RecordingEnded { id: 1 }).await;
    assert!(d.brain.recording.is_none());
    assert!(d.brain.snapshot().recording.is_none());
}

/// An enrollment running: its recording on the Brain's taps, and the Brain
/// told. The recording is kept for the test to read how it ended.
async fn enrolling(d: &mut Device) -> crate::recording::Recording {
    let (tx, _inputs) = mpsc::channel(8);
    let recorder = crate::recording::Recorder::new(
        d.brain.shared.clone(),
        tx,
        watch::channel(Snapshot::default()).1,
        None,
    );
    let from = "192.168.1.9".parse().unwrap();
    let recording = recorder
        .start_kind(from, 30, RecordingKind::Enrollment)
        .await
        .unwrap();
    d.brain
        .handle(Input::RecordingStarted {
            id: 1,
            until: Instant::now() + Duration::from_secs(30),
            from,
            kind: RecordingKind::Enrollment,
        })
        .await;
    d.brain.refresh();
    recording
}

async fn how_it_ended(mut recording: crate::recording::Recording) -> crate::recording::Finish {
    match recording.step().await {
        crate::recording::Step::Ended(finish) => finish,
        crate::recording::Step::Chunk(_) => panic!("it still runs"),
    }
}

#[tokio::test]
async fn an_enrollment_shows_listening_and_pauses_the_wake_word() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    d.ha(HaCommand::VoiceSubscribed(true)).await;
    let _recording = enrolling(&mut d).await;
    assert_eq!(d.brain.leds.borrow().base, Effect::Listening);
    assert_eq!(d.brain.snapshot().phase, pinna_api::Phase::PHASE_IDLE);
    assert_eq!(
        d.brain.snapshot().recording.map(|r| r.kind),
        Some(RecordingKind::Enrollment)
    );
    // The detector is gated off, and a wake that comes anyway starts nothing.
    assert!(!d.brain.shared.wake_enabled.load(Ordering::SeqCst));
    d.wake().await;
    assert_eq!(d.brain.phase, Phase::Idle);
    // An audio recording leaves the wake word alone.
    d.brain.handle(Input::RecordingEnded { id: 1 }).await;
    d.brain.refresh();
    d.wake().await;
    assert!(matches!(d.brain.phase, Phase::Starting(_)));
}

#[tokio::test]
async fn a_press_of_action_ends_an_enrollment_and_does_nothing_else() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    let recording = enrolling(&mut d).await;
    d.brain
        .handle(Input::Button(ButtonEvent::Press(Button::Action)))
        .await;
    let finish = how_it_ended(recording).await;
    assert_eq!(
        finish.reason,
        pinna_api::RecordingEnd::RECORDING_END_STOPPED
    );
    assert_eq!(finish.detail, "Someone pressed Action.");
    assert_eq!(d.brain.phase, Phase::Idle);
}

#[tokio::test]
async fn an_alarm_or_an_announcement_that_arrives_ends_an_enrollment_and_plays() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    let recording = enrolling(&mut d).await;
    d.ha(HaCommand::Announce {
        media_id: "http://ha.test/announce".into(),
        text: String::new(),
        preannounce_media_id: String::new(),
        start_conversation: false,
    })
    .await;
    d.brain.refresh();
    let finish = how_it_ended(recording).await;
    assert_eq!(
        finish.reason,
        pinna_api::RecordingEnd::RECORDING_END_STOPPED
    );
    assert!(finish.detail.contains("announcement"), "{}", finish.detail);
    assert_eq!(d.brain.phase, Phase::Speaking, "it plays");

    let mut d = Device::new();
    d.brain.boot_done = true;
    let recording = enrolling(&mut d).await;
    d.ha(HaCommand::Timer {
        event: TimerEventType::Finished,
        id: "tea".into(),
        name: "Tea".into(),
        total_seconds: 10,
        seconds_left: 0,
        is_active: false,
    })
    .await;
    d.brain.refresh();
    let finish = how_it_ended(recording).await;
    assert!(finish.detail.contains("alarm"), "{}", finish.detail);
    assert!(d.brain.snapshot().alarm_ringing, "it rings");
}

#[tokio::test]
async fn the_snapshot_says_when_the_satellite_is_quiet() {
    let mut d = Device::new();
    assert!(d.brain.snapshot().quiet);
    d.ha(HaCommand::Announce {
        media_id: "http://ha.test/announce".into(),
        text: String::new(),
        preannounce_media_id: String::new(),
        start_conversation: false,
    })
    .await;
    assert!(!d.brain.snapshot().quiet);
}

#[tokio::test]
async fn a_recording_nobody_ended_is_forgotten_after_its_time_and_a_grace() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    d.brain
        .handle(Input::RecordingStarted {
            id: 1,
            until: Instant::now() - crate::recording::BRAIN_GRACE - Duration::from_millis(1),
            from: "192.168.1.9".parse().unwrap(),
            kind: RecordingKind::Audio,
        })
        .await;
    d.brain.tick().await;
    assert!(d.brain.recording.is_none());
}

/// The Debugging binary sensor (ADR 0036): it follows what the timeline tells
/// the Brain, is part of the status, and is sent again when Home Assistant
/// connects.
#[tokio::test]
async fn debugging_is_a_binary_sensor_and_part_of_the_status() {
    let mut d = Device::new();
    let state = |state| DeviceEvent::BinarySensorState {
        object_id: "debugging".into(),
        state,
    };
    let now = Instant::now();
    let snapshot = DebuggingSnapshot {
        until: now + Duration::from_secs(900),
        levels_until: Some(now + Duration::from_secs(900)),
        text_until: None,
    };
    d.brain.handle(Input::Debugging(Some(snapshot))).await;
    d.expect(&[state(true)]);
    assert_eq!(d.brain.snapshot().debugging, Some(snapshot));

    d.ha(HaCommand::ClientConnected {
        peer: "192.0.2.1:6053".into(),
    })
    .await;
    let mut events = Vec::new();
    while let Ok(event) = d.events.try_recv() {
        events.push(event);
    }
    assert!(events.contains(&state(true)), "{events:?}");

    d.brain.handle(Input::Debugging(None)).await;
    d.expect(&[state(false)]);
    assert_eq!(d.brain.snapshot().debugging, None);
}

/// The events the timeline kept, as (kind, detail).
fn timeline_kept(t: &crate::timeline::Timeline) -> Vec<(i32, String)> {
    t.backlog()
        .batches
        .into_iter()
        .flat_map(|b| b.events)
        .map(|e| (e.kind.to_i32(), e.detail))
        .collect()
}

fn kind(kind: crate::timeline::EventKind) -> i32 {
    use buffa::Enumeration as _;
    use pinna_api::TimelineEventKind as K;
    match kind {
        crate::timeline::EventKind::Phase => K::TIMELINE_EVENT_KIND_PHASE,
        crate::timeline::EventKind::Run => K::TIMELINE_EVENT_KIND_RUN,
        crate::timeline::EventKind::Interrupted => K::TIMELINE_EVENT_KIND_INTERRUPTED,
        crate::timeline::EventKind::HomeAssistant => K::TIMELINE_EVENT_KIND_HOME_ASSISTANT,
        _ => unreachable!("not used by these tests"),
    }
    .to_i32()
}

fn kept_of(t: &crate::timeline::Timeline, k: crate::timeline::EventKind) -> Vec<String> {
    timeline_kept(t)
        .into_iter()
        .filter(|(n, _)| *n == kind(k))
        .map(|(_, d)| d)
        .collect()
}

#[tokio::test]
async fn the_timeline_hears_each_phase_change_once_and_the_runs_milestones() {
    use crate::timeline::EventKind;
    let t = crate::timeline::for_this_test();
    t.start("127.0.0.1".parse().unwrap(), 15, false, false);
    let mut d = Device::new();
    // The run loop refreshes after each input.
    d.listen().await;
    d.brain.refresh();
    d.brain.refresh();
    d.voice(VoiceEventType::SttVadEnd, &[]).await;
    d.brain.refresh();
    d.voice(VoiceEventType::TtsEnd, &[("url", "http://ha.test/reply")])
        .await;
    let token = d.brain.playback.as_ref().unwrap().0;
    d.brain.handle(Input::PlaybackStarted { token }).await;
    d.brain.refresh();
    d.voice(VoiceEventType::RunEnd, &[]).await;
    d.brain
        .handle(Input::PlaybackDone { token, ok: true })
        .await;
    d.brain.refresh();
    assert_eq!(
        kept_of(&t, EventKind::Phase),
        ["listening", "thinking", "speaking", "idle"]
    );
    assert_eq!(
        kept_of(&t, EventKind::Run),
        ["wake_word", "listening", "thinking", "replying", "finished"]
    );
    // The event type only, never its data (the reply's address).
    let ha = kept_of(&t, EventKind::HomeAssistant);
    assert!(ha.contains(&"tts-end".to_owned()), "{ha:?}");
    assert!(ha.iter().all(|d| !d.contains("ha.test")), "{ha:?}");
}

#[tokio::test]
async fn the_wake_word_interrupting_a_reply_is_an_interruption() {
    use crate::timeline::EventKind;
    let t = crate::timeline::for_this_test();
    t.start("127.0.0.1".parse().unwrap(), 15, false, false);
    let mut d = Device::new();
    d.listen().await;
    d.reply().await;
    d.wake().await;
    assert_eq!(kept_of(&t, EventKind::Interrupted), ["wake word"]);
}

#[tokio::test]
async fn nothing_reaches_the_timeline_while_debugging_is_off() {
    let t = crate::timeline::for_this_test();
    let mut d = Device::new();
    d.listen().await;
    d.reply().await;
    d.wake().await;
    d.brain.refresh();
    assert!(timeline_kept(&t).is_empty());
}

#[tokio::test]
async fn the_setup_hotspot_shows_on_the_ring_and_changes_nothing_else() {
    let mut d = Device::new();
    d.brain.boot_done = true;
    d.brain.refresh();
    assert!(!d.brain.leds.borrow().setup);

    d.brain.handle(Input::Hotspot(true)).await;
    d.brain.refresh();
    assert!(d.brain.leds.borrow().setup);
    assert_eq!(d.brain.leds.borrow().base, Effect::Idle);
    assert_eq!(d.brain.phase, Phase::Idle);
    d.expect(&[]);

    d.brain.handle(Input::Hotspot(false)).await;
    d.brain.refresh();
    assert!(!d.brain.leds.borrow().setup);
}
