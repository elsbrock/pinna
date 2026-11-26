//! One log line per standalone request (ADR 0017 §"Errors"): what each stage
//! took, what it cost and how the request ended, never what was said. Each
//! run gets its own [`RunLog`] as its observer, which passes everything on to
//! the metrics and writes the line once the run is gone, so a run that was
//! cut off (the wake word, the Action button, a mode switch) gets one too.

use std::sync::{Arc, Mutex};

use pinna_assist::error::{ProviderError, Slot};
use pinna_assist::observe::{Guard, Observer, Spend};
use pinna_assist::presets::PresetId;

use crate::voice::RunId;

pub struct RunLog {
    run: RunId,
    metrics: Arc<dyn Observer>,
    seen: Mutex<Seen>,
}

#[derive(Default)]
struct Seen {
    stt: Option<f64>,
    /// Until the brain's first words, once for each round that had words.
    brain: Vec<f64>,
    /// Until the first sentence's audio.
    tts: Option<f64>,
    audio: f32,
    tokens: (u64, u64),
    characters: u64,
    guards: Vec<Guard>,
    failed: Option<String>,
    finished: bool,
}

impl RunLog {
    pub fn new(run: RunId, metrics: Arc<dyn Observer>) -> Arc<Self> {
        Arc::new(Self {
            run,
            metrics,
            seen: Mutex::default(),
        })
    }

    /// The run returned: whatever it reported is how it ended.
    pub fn finished(&self) {
        self.seen.lock().unwrap().finished = true;
    }

    fn line(&self) -> String {
        let seen = self.seen.lock().unwrap();
        let mut parts = Vec::new();
        if let Some(s) = seen.stt {
            parts.push(format!(
                "speech-to-text {s:.1} s ({:.1} s of audio)",
                seen.audio
            ));
        }
        if let Some(first) = seen.brain.first() {
            let rounds = match seen.brain.len() {
                1 => String::new(),
                n => format!(", {n} rounds with words"),
            };
            parts.push(format!("brain {first:.1} s to its first words{rounds}"));
        }
        if let Some(s) = seen.tts {
            parts.push(format!("text-to-speech {s:.1} s for the first sentence"));
        }
        if seen.tokens != (0, 0) {
            parts.push(format!(
                "{} tokens in, {} out",
                seen.tokens.0, seen.tokens.1
            ));
        }
        if seen.characters > 0 {
            parts.push(format!("{} characters spoken", seen.characters));
        }
        if seen.guards.contains(&Guard::Ack) {
            parts.push("the acknowledgment played".into());
        }
        let ended = if let Some(failure) = &seen.failed {
            format!("failed: {failure}")
        } else if seen.guards.contains(&Guard::NoSpeech) {
            "nothing heard".into()
        } else if seen.guards.contains(&Guard::Invented) {
            "the transcript was only what speech-to-text invents on silence".into()
        } else if seen.finished {
            "answered".into()
        } else {
            "stopped before it ended".into()
        };
        if parts.is_empty() {
            format!("request {}: {ended}", self.run.0)
        } else {
            format!("request {}: {}; {ended}", self.run.0, parts.join(", "))
        }
    }
}

impl Observer for RunLog {
    fn stage(&self, slot: Slot, seconds: f64) {
        self.metrics.stage(slot, seconds);
        // Tool calls are timed where they run, with the tool's name.
        let name = match slot {
            Slot::Stt => Some("speech-to-text"),
            Slot::Chat => Some("model"),
            Slot::Tts => Some("text-to-speech"),
            Slot::Tool => None,
        };
        if let Some(name) = name {
            crate::timeline::stage(
                || name.to_owned(),
                std::time::Duration::from_secs_f64(seconds.max(0.0)),
                self.run.0,
            );
        }
        let mut seen = self.seen.lock().unwrap();
        match slot {
            Slot::Stt => seen.stt = Some(seconds),
            Slot::Chat => seen.brain.push(seconds),
            Slot::Tts => {
                seen.tts.get_or_insert(seconds);
            }
            Slot::Tool => {}
        }
    }

    fn spent(&self, preset: PresetId, spend: Spend) {
        self.metrics.spent(preset, spend);
        let mut seen = self.seen.lock().unwrap();
        match spend {
            Spend::AudioSeconds(s) => seen.audio += s,
            Spend::Tokens { input, output } => {
                seen.tokens.0 += input;
                seen.tokens.1 += output;
            }
            Spend::Characters(n) => seen.characters += n,
        }
    }

    fn guard(&self, guard: Guard) {
        self.metrics.guard(guard);
        self.seen.lock().unwrap().guards.push(guard);
    }

    fn failed(&self, error: &ProviderError) {
        self.metrics.failed(error);
        self.seen.lock().unwrap().failed = Some(error.to_string());
    }
}

impl Drop for RunLog {
    fn drop(&mut self) {
        tracing::info!("{}", self.line());
    }
}

#[cfg(test)]
mod tests {
    use pinna_assist::error::Reason;

    use super::*;

    struct Nothing;
    impl Observer for Nothing {
        fn stage(&self, _: Slot, _: f64) {}
        fn spent(&self, _: PresetId, _: Spend) {}
        fn guard(&self, _: Guard) {}
        fn failed(&self, _: &ProviderError) {}
    }

    fn log() -> Arc<RunLog> {
        RunLog::new(RunId(7), Arc::new(Nothing))
    }

    #[tokio::test]
    async fn a_stage_is_a_span_that_ends_now() {
        let t = crate::timeline::for_this_test();
        t.start("127.0.0.1".parse().unwrap(), 15, false, false);
        let log = log();
        log.stage(Slot::Stt, 0.5);
        log.stage(Slot::Tool, 0.1);
        let events: Vec<_> = t
            .backlog()
            .batches
            .into_iter()
            .flat_map(|b| b.events)
            .collect();
        // A tool call is timed where it runs.
        assert_eq!(events.len(), 1);
        let stage = &events[0];
        assert_eq!(stage.detail, "speech-to-text");
        assert_eq!(stage.run, 7);
        assert!(
            (499..=501).contains(&(stage.end_unix_ms - stage.unix_ms)),
            "{} to {}",
            stage.unix_ms,
            stage.end_unix_ms
        );
    }

    #[test]
    fn an_answered_request_has_its_stages_and_costs() {
        let log = log();
        log.spent(PresetId::OpenAi, Spend::AudioSeconds(2.5));
        log.stage(Slot::Stt, 0.62);
        log.stage(Slot::Chat, 1.4);
        log.stage(Slot::Chat, 0.9);
        log.spent(
            PresetId::OpenAi,
            Spend::Tokens {
                input: 120,
                output: 9,
            },
        );
        log.spent(
            PresetId::OpenAi,
            Spend::Tokens {
                input: 140,
                output: 12,
            },
        );
        log.stage(Slot::Tts, 0.41);
        log.stage(Slot::Tts, 0.3);
        log.spent(PresetId::OpenAi, Spend::Characters(62));
        log.finished();
        assert_eq!(
            log.line(),
            "request 7: speech-to-text 0.6 s (2.5 s of audio), brain 1.4 s to its first words, \
             2 rounds with words, text-to-speech 0.4 s for the first sentence, 260 tokens in, 21 out, \
             62 characters spoken; \
             answered"
        );
    }

    #[test]
    fn each_ending_is_named() {
        let quiet = log();
        quiet.guard(Guard::NoSpeech);
        quiet.finished();
        assert_eq!(quiet.line(), "request 7: nothing heard");

        let refused = log();
        refused.stage(Slot::Stt, 0.5);
        refused.failed(&ProviderError::new(
            Slot::Chat,
            PresetId::OpenAi,
            Reason::Http(400),
        ));
        refused.finished();
        assert!(
            refused
                .line()
                .ends_with("; failed: brain via openai: HTTP 400"),
            "{}",
            refused.line()
        );

        let cut = log();
        cut.stage(Slot::Stt, 0.5);
        assert!(cut.line().ends_with("; stopped before it ended"));
    }
}
