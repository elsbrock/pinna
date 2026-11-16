//! The log lines of pinnad and its media sources (AirPlay, Bluetooth) from
//! the journal, for the dashboard.
//!
//! The page has no login, so DEBUG and TRACE lines (which carry voice
//! transcripts) never leave this module.

use std::process::Stdio;

use futures::Stream;
use pinna_api::LogLine;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

const LEVELS: [&str; 5] = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"];

/// `journalctl -f` for pinnad's unit and those it starts; the child dies with
/// the stream.
pub fn watch() -> std::io::Result<impl Stream<Item = LogLine>> {
    let mut child = Command::new("journalctl")
        .args([
            "-u",
            "pinnad.service",
            "-u",
            "pinna-airplay.service",
            "-u",
            "pinna-bt.service",
            "-u",
            "bluetooth.service",
            "-u",
            "bluealsa.service",
            "-o",
            "json",
            "-n",
            "1000",
            "-f",
            "--no-pager",
        ])
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let lines = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
    Ok(futures::stream::unfold(
        (child, lines),
        |(child, mut lines): (Child, tokio::io::Lines<BufReader<ChildStdout>>)| async move {
            loop {
                match lines.next_line().await {
                    Ok(Some(raw)) => {
                        if let Some(line) = parse_journal_line(&raw) {
                            return Some((line, (child, lines)));
                        }
                    }
                    _ => return None,
                }
            }
        },
    ))
}

/// One `journalctl -o json` entry; `None` when it is malformed or not for
/// the page (DEBUG, TRACE).
pub fn parse_journal_line(raw: &str) -> Option<LogLine> {
    let entry: Value = serde_json::from_str(raw).ok()?;
    let message = match entry.get("MESSAGE")? {
        Value::String(s) => s.clone(),
        // journald stores messages with control characters as byte arrays.
        Value::Array(a) => {
            let bytes: Vec<u8> = a
                .iter()
                .filter_map(|b| b.as_u64()?.try_into().ok())
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => return None,
    };
    let unix_ms = entry
        .get("__REALTIME_TIMESTAMP")
        .and_then(Value::as_str)
        .and_then(|us| us.parse::<u64>().ok())
        .unwrap_or_default()
        / 1000;
    let message = strip_ansi(&message);
    let (level, message) = match message.split_whitespace().next() {
        Some(first) if LEVELS.contains(&first) => (
            first.to_owned(),
            message.trim_start()[first.len()..].trim().to_owned(),
        ),
        _ => {
            let priority = entry
                .get("PRIORITY")
                .and_then(Value::as_str)
                .and_then(|p| p.parse::<u8>().ok());
            let level = match priority {
                Some(0..=3) => "ERROR",
                Some(4) => "WARN",
                Some(7) => "DEBUG",
                _ => "INFO",
            };
            (level.to_owned(), message)
        }
    };
    if level == "DEBUG" || level == "TRACE" {
        return None;
    }
    Some(LogLine {
        unix_ms,
        level,
        target: entry
            .get("SYSLOG_IDENTIFIER")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        message,
        ..Default::default()
    })
}

/// Drop `ESC [ … <final byte>` sequences (colours).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            let mut rest = chars.clone();
            if rest.next() == Some('[') {
                for c in rest.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
                chars = rest;
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_message_with_level() {
        let raw = r#"{"MESSAGE":"INFO esphome api listening addr=0.0.0.0:6053","PRIORITY":"6","SYSLOG_IDENTIFIER":"pinnad","__REALTIME_TIMESTAMP":"1731000000123456"}"#;
        let l = parse_journal_line(raw).unwrap();
        assert_eq!(l.unix_ms, 1_731_000_000_123);
        assert_eq!(l.level, "INFO");
        assert_eq!(l.target, "pinnad");
        assert_eq!(l.message, "esphome api listening addr=0.0.0.0:6053");
    }

    #[test]
    fn byte_array_message_with_ansi() {
        let text = "\x1b[33m WARN\x1b[0m wake word model is slow";
        let bytes: Vec<String> = text.bytes().map(|b| b.to_string()).collect();
        let raw = format!(
            r#"{{"MESSAGE":[{}],"__REALTIME_TIMESTAMP":"2000"}}"#,
            bytes.join(",")
        );
        let l = parse_journal_line(&raw).unwrap();
        assert_eq!(l.unix_ms, 2);
        assert_eq!(l.level, "WARN");
        assert_eq!(l.message, "wake word model is slow");
        assert_eq!(l.target, "");
    }

    #[test]
    fn debug_and_trace_are_dropped() {
        for level in ["DEBUG", "TRACE"] {
            let raw = format!(r#"{{"MESSAGE":"{level} transcript text=hello","PRIORITY":"6"}}"#);
            assert!(parse_journal_line(&raw).is_none());
        }
    }

    #[test]
    fn priority_is_the_fallback_level() {
        let raw = |p: &str| format!(r#"{{"MESSAGE":"Started pinnad.","PRIORITY":"{p}"}}"#);
        let l = parse_journal_line(&raw("6")).unwrap();
        assert_eq!(
            (l.level.as_str(), l.message.as_str()),
            ("INFO", "Started pinnad.")
        );
        assert_eq!(parse_journal_line(&raw("3")).unwrap().level, "ERROR");
        assert_eq!(parse_journal_line(&raw("4")).unwrap().level, "WARN");
        assert!(parse_journal_line(&raw("7")).is_none());
        let l = parse_journal_line(r#"{"MESSAGE":"no priority"}"#).unwrap();
        assert_eq!(l.level, "INFO");
    }

    #[test]
    fn malformed_json_is_none() {
        assert!(parse_journal_line("{not json").is_none());
        assert!(parse_journal_line(r#"{"PRIORITY":"6"}"#).is_none());
    }
}
