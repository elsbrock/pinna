//! Device tools: each checks its arguments with `checks.rs` and sends the
//! `ApiCommand` the dashboard sends, so it reaches the same Brain setters as
//! Home Assistant's entities (ADR 0017).

use std::time::Instant;

use pinna_esphome::{MediaPlayerCommand, MediaPlayerStateKind};
use serde_json::{Value, json};

use super::{Args, Caller, DeviceTools};
use crate::checks;
use crate::leds::LightEffect;
use crate::voice::{ApiCommand, RingLight, RingLightPatch, SettingsPatch, Snapshot, TimerSnapshot};

/// The longest timer name, in characters.
const TIMER_NAME_MAX: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Op {
    SetVolume,
    ChangeVolume,
    SetReplyVolume,
    RingLight,
    PlayUrl,
    PauseMedia,
    ResumeMedia,
    StopMedia,
    ClearNotification,
    MuteMicrophone,
    SetTimer,
    ListTimers,
    CancelTimer,
    CancelRequest,
    Status,
    Time,
    Weather,
}

/// Runs a device tool: what it did, or why not.
pub(super) async fn run(tools: &DeviceTools, op: Op, args: &Args) -> Result<String, String> {
    match op {
        Op::Time => {
            let zone = tools.status.borrow().settings.time_zone.clone();
            let (time, zone) = crate::clock::local_now(zone.as_deref(), std::time::SystemTime::now());
            Ok(format!("{time} ({zone})"))
        }
        Op::Weather => {
            let out = tools
                .weather
                .forecast(tools.live.place.as_ref(), tools.live.language.as_deref())
                .await;
            if out.is_error { Err(out.text) } else { Ok(out.text) }
        }
        Op::SetVolume => {
            let percent = percent(args, "percent")?;
            let s = tools.ask(|r| ApiCommand::SetVolume(percent, r)).await?;
            Ok(format!("The volume is {}%.", s.volume))
        }
        Op::ChangeVolume => {
            let by = args.need_int("by")?;
            let volume = i64::from(tools.status.borrow().volume);
            let target = volume.saturating_add(by).clamp(0, 100) as u8;
            let s = tools.ask(|r| ApiCommand::SetVolume(target, r)).await?;
            Ok(format!("The volume is {}%.", s.volume))
        }
        Op::SetReplyVolume => {
            let patch = SettingsPatch {
                reply_volume: Some(percent(args, "percent")?),
                ..Default::default()
            };
            let s = tools.ask(|r| ApiCommand::UpdateSettings(patch, r)).await?;
            Ok(format!("Replies play at {}%.", s.settings.reply_volume()))
        }
        Op::RingLight => {
            let patch = light_patch(args)?;
            let s = tools.ask(|r| ApiCommand::SetRingLight(patch, r)).await?;
            Ok(describe_light(&s.ring_light))
        }
        Op::PlayUrl => {
            let url = checks::media_url(args.need_text("url")?)?;
            tools.ask(|r| ApiCommand::PlayMediaChecked(url, r)).await?;
            Ok("Playing.".into())
        }
        Op::PauseMedia => media(tools, MediaPlayerCommand::Pause, "Paused.").await,
        Op::ResumeMedia => media(tools, MediaPlayerCommand::Play, "Resumed.").await,
        Op::StopMedia => media(tools, MediaPlayerCommand::Stop, "Stopped.").await,
        Op::ClearNotification => {
            tools.ask(ApiCommand::ClearNotification).await?;
            Ok("The notification is cleared.".into())
        }
        Op::MuteMicrophone => {
            tools.ask(|r| ApiCommand::SetMute(true, r)).await?;
            Ok(match tools.caller {
                // The Brain mutes once the run that asked has ended (T28).
                Caller::Run(_) => {
                    "The microphone mutes when this reply ends. Only a person can unmute it."
                }
                Caller::Mcp => "The microphone is muted. Only a person can unmute it.",
            }
            .into())
        }
        Op::SetTimer => {
            // The Brain refuses a length out of range with its own sentence.
            let seconds = unsigned(args.need_int("seconds")?);
            let name = timer_name(args.text("name"))?;
            let label = name
                .as_deref()
                .map(|n| format!("Timer \"{n}\""))
                .unwrap_or_else(|| "Timer".into());
            let id = tools
                .ask(|reply| ApiCommand::SetTimer {
                    name,
                    seconds,
                    reply,
                })
                .await?;
            Ok(format!("{label} set for {} (id {id}).", spoken(seconds)))
        }
        Op::ListTimers => {
            let s = tools.status.borrow();
            Ok(json!({ "timers": timers(&s.timers, Instant::now()) }).to_string())
        }
        Op::CancelTimer => {
            let timer = args.need_text("timer")?.trim().to_owned();
            tools.ask(|r| ApiCommand::CancelTimer(timer, r)).await?;
            Ok("The timer is cancelled.".into())
        }
        Op::CancelRequest => {
            tools.ask(ApiCommand::StopConversation).await?;
            Ok("Stopped.".into())
        }
        Op::Status => Ok(status(&tools.status.borrow(), Instant::now()).to_string()),
    }
}

async fn media(
    tools: &DeviceTools,
    command: MediaPlayerCommand,
    done: &str,
) -> Result<String, String> {
    tools
        .ask(|r| ApiCommand::ControlMedia(Some(command), None, r))
        .await?;
    Ok(done.into())
}

/// A negative or huge number becomes one that every range check refuses.
fn unsigned(v: i64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

fn percent(args: &Args, name: &str) -> Result<u8, String> {
    checks::percent(name, unsigned(args.need_int(name)?))
}

fn light_patch(args: &Args) -> Result<RingLightPatch, String> {
    let brightness = match args.int("brightness") {
        Some(b) => Some(checks::percent("brightness", unsigned(b))?),
        None => None,
    };
    let rgb = args.text("color").map(color).transpose()?;
    let effect =
        match args.text("effect") {
            Some(name) => Some(LightEffect::from_name(name).ok_or_else(|| {
                format!("effect must be one of {}", LightEffect::NAMES.join(", "))
            })?),
            None => None,
        };
    let patch = RingLightPatch {
        on: args.flag("on"),
        brightness: brightness.map(|b| f32::from(b) / 100.0),
        rgb,
        effect,
    };
    if patch.on.is_none()
        && patch.brightness.is_none()
        && patch.rgb.is_none()
        && patch.effect.is_none()
    {
        return Err("ring_light needs on, brightness, color or effect.".into());
    }
    Ok(patch)
}

/// `#rrggbb` (the `#` is optional) as the Brain's 0..=1 channels.
fn color(text: &str) -> Result<(f32, f32, f32), String> {
    let hex = text.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    let rgb = (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
        .ok_or("color must be a hex colour such as #ff8800")?;
    let channel = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.0;
    Ok((channel(16), channel(8), channel(0)))
}

fn describe_light(light: &RingLight) -> String {
    if !light.on {
        return "The ring light is off.".into();
    }
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (r, g, b) = light.rgb;
    format!(
        "The ring light is on at {}%, colour #{:02x}{:02x}{:02x}, effect {}.",
        (light.brightness * 100.0).round(),
        byte(r),
        byte(g),
        byte(b),
        light.effect.name()
    )
}

/// A timer name as given, trimmed; empty means none.
fn timer_name(name: Option<&str>) -> Result<Option<String>, String> {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if name.chars().count() > TIMER_NAME_MAX || name.chars().any(char::is_control) {
        return Err(format!(
            "A timer name must be one line of at most {TIMER_NAME_MAX} characters."
        ));
    }
    Ok(Some(name.to_owned()))
}

/// A length the way it is said, such as `1 hour 30 minutes`.
fn spoken(seconds: u32) -> String {
    let parts: Vec<String> = [
        (seconds / 3600, "hour"),
        (seconds / 60 % 60, "minute"),
        (seconds % 60, "second"),
    ]
    .into_iter()
    .filter(|&(n, _)| n > 0)
    .map(|(n, unit)| format!("{n} {unit}{}", if n == 1 { "" } else { "s" }))
    .collect();
    if parts.is_empty() {
        "0 seconds".into()
    } else {
        parts.join(" ")
    }
}

fn timers(timers: &[TimerSnapshot], now: Instant) -> Value {
    timers
        .iter()
        .map(|t| {
            let left = if t.active {
                let elapsed = now.saturating_duration_since(t.at).as_secs();
                t.left
                    .saturating_sub(u32::try_from(elapsed).unwrap_or(u32::MAX))
            } else {
                t.left
            };
            json!({
                "id": t.id,
                "name": t.name,
                "seconds_left": left,
                "total_seconds": t.total,
                "paused": !t.active,
            })
        })
        .collect()
}

/// What an MCP client may see of the device. Media addresses can carry
/// tokens, so no URL is ever part of it.
fn status(s: &Snapshot, now: Instant) -> Value {
    let phase = match s.phase {
        pinna_api::Phase::PHASE_LISTENING => "listening",
        pinna_api::Phase::PHASE_THINKING => "thinking",
        pinna_api::Phase::PHASE_SPEAKING => "speaking",
        _ => "idle",
    };
    let state = match s.media.state {
        MediaPlayerStateKind::Idle => "idle",
        MediaPlayerStateKind::Playing => "playing",
        MediaPlayerStateKind::Paused => "paused",
        MediaPlayerStateKind::Announcing => "announcing",
    };
    let mut media = json!({ "state": state });
    let track = &s.media.track;
    for (key, value) in [
        ("title", &track.title),
        ("artist", &track.artist),
        ("station", &track.station),
    ] {
        if !value.is_empty() {
            media[key] = value.as_str().into();
        }
    }
    json!({
        "mode": s.mode,
        "phase": phase,
        "volume": s.volume,
        "reply_volume": s.settings.reply_volume(),
        "muted": s.hw_muted || s.sw_muted,
        "media": media,
        "timers": timers(&s.timers, now),
        "alarm_ringing": s.alarm_ringing,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::settings::{Mode, Settings};
    use crate::voice::MediaSnapshot;

    #[test]
    fn lengths_are_spoken_in_units() {
        assert_eq!(spoken(1), "1 second");
        assert_eq!(spoken(300), "5 minutes");
        assert_eq!(spoken(5_400), "1 hour 30 minutes");
        assert_eq!(spoken(86_400), "24 hours");
        assert_eq!(spoken(3_661), "1 hour 1 minute 1 second");
    }

    #[test]
    fn colours_are_six_hex_digits() {
        assert_eq!(color("#ff0000"), Ok((1.0, 0.0, 0.0)));
        assert_eq!(color(" 00FF00 "), Ok((0.0, 1.0, 0.0)));
        for bad in ["#fff", "+fffff", "#gg0000", "#ff00000", "red", ""] {
            assert!(color(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn timer_names_are_one_short_line() {
        assert_eq!(timer_name(None), Ok(None));
        assert_eq!(timer_name(Some("  ")), Ok(None));
        assert_eq!(timer_name(Some(" Tea ")), Ok(Some("Tea".into())));
        assert!(timer_name(Some(&"é".repeat(TIMER_NAME_MAX))).is_ok());
        assert!(timer_name(Some(&"é".repeat(TIMER_NAME_MAX + 1))).is_err());
        assert!(timer_name(Some("tea\nnow")).is_err());
    }

    #[test]
    fn status_never_shows_a_media_address() {
        let now = Instant::now();
        let s = Snapshot {
            mode: Mode::Standalone,
            phase: pinna_api::Phase::PHASE_SPEAKING,
            volume: 40,
            settings: Settings {
                volume: 40,
                reply_volume: Some(25),
                ..Default::default()
            },
            sw_muted: true,
            alarm_ringing: true,
            timers: vec![TimerSnapshot {
                id: "local-1".into(),
                name: "Tea".into(),
                total: 300,
                left: 120,
                active: true,
                at: now,
            }],
            media: MediaSnapshot {
                state: MediaPlayerStateKind::Playing,
                url: Some("https://radio.test/live?token=secret".into()),
                error: Some("https://radio.test/old failed".into()),
                error_url: Some("https://radio.test/old".into()),
                track: pinna_audio::Track {
                    title: "Song".into(),
                    station: "Radio".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };

        let shown = status(&s, now + Duration::from_secs(20));

        assert_eq!(
            shown,
            json!({
                "mode": "standalone",
                "phase": "speaking",
                "volume": 40,
                "reply_volume": 25,
                "muted": true,
                "media": {"state": "playing", "title": "Song", "station": "Radio"},
                "timers": [{
                    "id": "local-1",
                    "name": "Tea",
                    "seconds_left": 100,
                    "total_seconds": 300,
                    "paused": false,
                }],
                "alarm_ringing": true,
            })
        );
        assert!(!shown.to_string().contains("radio.test"));
    }

    #[test]
    fn a_paused_timer_keeps_its_seconds() {
        let now = Instant::now();
        let paused = TimerSnapshot {
            id: "ha".into(),
            name: String::new(),
            total: 60,
            left: 30,
            active: false,
            at: now,
        };
        assert_eq!(
            timers(&[paused], now + Duration::from_secs(10))[0]["seconds_left"],
            30
        );
    }
}
