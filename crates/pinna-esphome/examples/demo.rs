//! Fake Pinna device for manual testing and the conformance test.
//!
//! Usage: `demo [port] [--key <base64>]` (default port 6053, 0 picks a free port; with a key
//! the API is Noise encrypted). Prints one JSON object per
//! line on stdout (`ready`, `command`, `sent`); logs go to stderr. Commands on stdin:
//! `voice [wake word phrase]` starts a voice run, `stop` aborts the run, `time` requests the time,
//! `event <object_id> <type>` fires an event, `text <object_id> <value>` sets a text sensor, `quit` exits.
//! Pressing the `Cancel request` button emits the `action` press event and sets `Last wake word` to Alexa.

use std::time::Duration;

use bytes::Bytes;
use pinna_esphome::{
    DeviceConfig, DeviceEvent, HaCommand, LightState, MediaPlayerCommand, MediaPlayerStateKind,
    WakeWordInfo, echo_dot_entities, parse_encryption_key, serve,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

const FRAMES: usize = 25;

fn out(v: Value) {
    println!("{v}");
}

fn sine_frame(n: usize) -> Bytes {
    // 20 ms of 440 Hz at 16 kHz, s16le mono.
    let mut b = Vec::with_capacity(640);
    for i in 0..320 {
        let t = (n * 320 + i) as f32 / 16000.0;
        let s = (t * 440.0 * std::f32::consts::TAU).sin() * 8000.0;
        b.extend_from_slice(&(s as i16).to_le_bytes());
    }
    Bytes::from(b)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let mut port = 6053;
    let mut key = None;
    while let Some(a) = args.next() {
        if a == "--key" {
            key = Some(parse_encryption_key(&args.next().unwrap_or_default())?);
        } else {
            port = a.parse()?;
        }
    }

    let mut cfg = DeviceConfig::new("pinna-demo", [0x02, 0x00, 0x00, 0x00, 0x00, 0x42]);
    cfg.friendly_name = "Pinna Demo".into();
    cfg.suggested_area = Some("Lab".into());
    cfg.port = port;
    cfg.bind = [127, 0, 0, 1].into();
    cfg.wake_words = vec![
        WakeWordInfo {
            id: "okay_nabu".into(),
            wake_word: "Okay Nabu".into(),
            trained_languages: vec!["en".into()],
        },
        WakeWordInfo {
            id: "alexa".into(),
            wake_word: "Alexa".into(),
            trained_languages: vec!["en".into()],
        },
    ];
    cfg.active_wake_words = vec!["okay_nabu".into()];
    echo_dot_entities(&mut cfg);
    cfg.encryption_key = key;

    let (handle, mut cmds) = serve(cfg).await?;
    handle.send(DeviceEvent::MuteState(false)).await?;
    handle
        .send(DeviceEvent::LightState {
            on: false,
            brightness: 1.0,
            rgb: (1.0, 1.0, 1.0),
            effect: None,
        })
        .await?;
    out(json!({"ready": true, "port": handle.local_addr().port()}));

    let mut light = LightState::default();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_open = true;
    let mut run = 0u32;
    let (mut mp_state, mut mp_volume, mut mp_muted) = (MediaPlayerStateKind::Idle, 0.5f32, false);

    loop {
        tokio::select! {
            line = lines.next_line(), if stdin_open => {
                let Some(line) = line? else { stdin_open = false; continue };
                let line = line.trim();
                match line.split_once(' ').unwrap_or((line, "")) {
                    ("voice", phrase) => {
                        run += 1;
                        let phrase = if phrase.is_empty() { "okay nabu" } else { phrase };
                        handle.send(DeviceEvent::VoiceStart {
                            conversation_id: format!("conv-{run}"),
                            wake_word_phrase: Some(phrase.to_owned()),
                            use_vad: true,
                        }).await?;
                        out(json!({"sent": "voice_start", "conversation_id": format!("conv-{run}")}));
                    }
                    ("stop", _) => handle.send(DeviceEvent::VoiceStop).await?,
                    ("time", _) => handle.send(DeviceEvent::RequestTime).await?,
                    ("event", rest) => {
                        let (object_id, event_type) = rest.split_once(' ').unwrap_or((rest, ""));
                        handle.send(DeviceEvent::Event {
                            object_id: object_id.to_owned(),
                            event_type: event_type.to_owned(),
                        }).await?;
                    }
                    ("text", rest) => {
                        let (object_id, value) = rest.split_once(' ').unwrap_or((rest, ""));
                        handle.send(DeviceEvent::TextSensorState {
                            object_id: object_id.to_owned(),
                            value: value.to_owned(),
                        }).await?;
                    }
                    ("quit", _) => break,
                    _ => {}
                }
            }
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    HaCommand::Light { on, brightness, rgb, effect } => {
                        out(json!({"command": "light", "on": on, "brightness": brightness, "rgb": rgb, "effect": effect}));
                        light.on = on.unwrap_or(light.on);
                        light.brightness = brightness.unwrap_or(light.brightness);
                        light.rgb = rgb.unwrap_or(light.rgb);
                        if effect.is_some() {
                            light.effect = effect.filter(|e| e != "None");
                        }
                        handle.send(DeviceEvent::LightState {
                            on: light.on, brightness: light.brightness, rgb: light.rgb, effect: light.effect.clone(),
                        }).await?;
                    }
                    HaCommand::MuteSwitch(m) => {
                        out(json!({"command": "mute", "state": m}));
                        handle.send(DeviceEvent::MuteState(m)).await?;
                    }
                    HaCommand::Switch { object_id, on } => {
                        out(json!({"command": "switch", "object_id": object_id, "state": on}));
                        handle.send(DeviceEvent::SwitchState { object_id, on }).await?;
                    }
                    HaCommand::Number { object_id, value } => {
                        out(json!({"command": "number", "object_id": object_id, "value": value}));
                        handle.send(DeviceEvent::NumberState { object_id, value }).await?;
                    }
                    HaCommand::Button { object_id } => {
                        out(json!({"command": "button", "object_id": object_id}));
                        handle.send(DeviceEvent::Event {
                            object_id: "action".into(),
                            event_type: "press".into(),
                        }).await?;
                        handle.send(DeviceEvent::TextSensorState {
                            object_id: "last_wake_word".into(),
                            value: "Alexa".into(),
                        }).await?;
                    }
                    HaCommand::MediaPlayer { command, volume, media_url, announcement } => {
                        out(json!({"command": "media_player", "media_command": command.map(|c| c.to_i32()),
                            "volume": volume, "media_url": media_url, "announcement": announcement}));
                        if let Some(v) = volume {
                            mp_volume = v;
                        }
                        if media_url.is_some() {
                            mp_state = if announcement {
                                MediaPlayerStateKind::Announcing
                            } else {
                                MediaPlayerStateKind::Playing
                            };
                        }
                        match command {
                            Some(MediaPlayerCommand::Play) => mp_state = MediaPlayerStateKind::Playing,
                            Some(MediaPlayerCommand::Pause) => mp_state = MediaPlayerStateKind::Paused,
                            Some(MediaPlayerCommand::Stop) => mp_state = MediaPlayerStateKind::Idle,
                            Some(MediaPlayerCommand::Mute) => mp_muted = true,
                            Some(MediaPlayerCommand::Unmute) => mp_muted = false,
                            _ => {}
                        }
                        handle.send(DeviceEvent::MediaPlayerState {
                            state: mp_state, volume: mp_volume, muted: mp_muted,
                        }).await?;
                    }
                    HaCommand::VoiceStarted { port, error } => {
                        out(json!({"command": "voice_started", "port": port, "error": error}));
                        if !error {
                            let h = handle.clone();
                            tokio::spawn(async move {
                                for n in 0..FRAMES {
                                    if h.send(DeviceEvent::VoiceAudio(sine_frame(n))).await.is_err() { return; }
                                    tokio::time::sleep(Duration::from_millis(20)).await;
                                }
                                let _ = h.send(DeviceEvent::VoiceAudioEnd).await;
                                out(json!({"sent": "voice_audio_end", "frames": FRAMES}));
                            });
                        }
                    }
                    HaCommand::VoiceEvent { event, data } => {
                        out(json!({"command": "voice_event", "event": event.to_i32(), "data": data}));
                    }
                    HaCommand::Timer { event, id, name, total_seconds, seconds_left, is_active } => {
                        out(json!({"command": "timer", "event": event.to_i32(), "id": id, "name": name,
                            "total_seconds": total_seconds, "seconds_left": seconds_left, "is_active": is_active}));
                    }
                    HaCommand::Announce { media_id, text, preannounce_media_id, start_conversation } => {
                        out(json!({"command": "announce", "media_id": media_id, "text": text,
                            "preannounce_media_id": preannounce_media_id, "start_conversation": start_conversation}));
                        handle.send(DeviceEvent::AnnounceFinished { success: true }).await?;
                    }
                    HaCommand::SetActiveWakeWords(w) => out(json!({"command": "set_active_wake_words", "ids": w})),
                    HaCommand::Time { epoch_seconds } => out(json!({"command": "time", "epoch_seconds": epoch_seconds})),
                    HaCommand::ClientConnected { peer } => out(json!({"command": "client_connected", "peer": peer})),
                    HaCommand::ClientDisconnected => out(json!({"command": "client_disconnected"})),
                    HaCommand::VoiceSubscribed(s) => out(json!({"command": "voice_subscribed", "subscribed": s})),
                    HaCommand::EncryptionKeySet(k) => out(json!({"command": "encryption_key_set", "key": k.to_vec()})),
                }
            }
        }
    }
    Ok(())
}
