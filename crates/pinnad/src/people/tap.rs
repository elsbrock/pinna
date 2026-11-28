//! The tap that recognizes who speaks in each request (ADR 0038).
//!
//! [`crate::mic::MicRouter`] owns it and feeds it the frames of a request, the
//! pre-roll included, in both modes. It keeps the speech as
//! `pinna_speaker::speech` does, and asks the speaker model for a voiceprint
//! at most twice per request:
//!
//! - the first run starts once there is 0.5 s of speech at the first pause,
//!   or when speech reaches 1.5 s, whichever comes first;
//! - a second starts when speech reaches 1.5 s after a first that had less.
//!
//! `route` only runs the VAD and decides. Waiting for the model and matching
//! happen on a tokio task, so the capture loop never blocks. The outcomes go
//! to the [`Speaker`] slot, and a change to "Last speaker" to the Brain.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use pinna_speaker::{
    MIN_SPEECH_SAMPLES, Thresholds, Voiceprint, WINDOW_SAMPLES, best_match, fill_window,
};
use pinna_vad::Vad;
use tokio::sync::mpsc;

use super::People;
use super::recognizer::State;
use super::speaker::{Outcome, Run, Speaker, Update};
use crate::metrics::Counters;
use crate::timeline;
use crate::voice::Input;

/// The VAD's frame, 10 ms.
const FRAME: usize = 160;
const SAMPLE_RATE: f32 = 16_000.0;

/// When a request's runs start, from the VAD's decision on each frame.
#[derive(Debug, Default)]
struct Trigger {
    /// Runs that started.
    runs: u8,
    /// The speech the first run had, in samples.
    first: usize,
    was_speech: bool,
}

impl Trigger {
    /// One frame: `len` samples of speech are kept so far (at most 1.5 s),
    /// and `speech` says whether this frame is speech. Returns the number of
    /// the run to start, if one starts here.
    fn frame(&mut self, len: usize, speech: bool) -> Option<u8> {
        let pause = self.was_speech && !speech;
        self.was_speech = speech;
        let run = match self.runs {
            0 if len >= WINDOW_SAMPLES || (pause && len >= MIN_SPEECH_SAMPLES) => {
                self.first = len;
                1
            }
            1 if self.first < WINDOW_SAMPLES && len >= WINDOW_SAMPLES => 2,
            _ => return None,
        };
        self.runs = run;
        Some(run)
    }
}

struct Session {
    epoch: u64,
    vad: Vad,
    /// Samples of a frame that isn't complete yet.
    carry: Vec<i16>,
    /// The speech so far, at most 1.5 s.
    speech: Vec<i16>,
    trigger: Trigger,
    /// Runs that asked the model.
    launched: u8,
}

pub struct Tap {
    people: Arc<People>,
    speaker: Arc<Speaker>,
    brain: mpsc::Sender<Input>,
    counters: Counters,
    session: Option<Session>,
}

impl Tap {
    pub fn new(
        people: Arc<People>,
        speaker: Arc<Speaker>,
        brain: mpsc::Sender<Input>,
        counters: Counters,
    ) -> Self {
        Self {
            people,
            speaker,
            brain,
            counters,
            session: None,
        }
    }

    /// A request's frames start. A session still open is replaced.
    pub fn start(&mut self) {
        self.session = Some(Session {
            epoch: self.speaker.reset(),
            vad: Vad::new(),
            carry: Vec::new(),
            speech: Vec::new(),
            trigger: Trigger::default(),
            launched: 0,
        });
    }

    /// A frame of 16 kHz s16 LE mono.
    pub fn frame(&mut self, frame: &Bytes) {
        let Some(session) = &mut self.session else {
            return;
        };
        session.carry.extend(
            frame
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]])),
        );
        let mut start = Vec::new();
        let mut used = 0;
        for chunk in session.carry.chunks_exact(FRAME) {
            used += FRAME;
            let speech = session.vad.process(chunk);
            if speech {
                let room = WINDOW_SAMPLES - session.speech.len();
                session.speech.extend_from_slice(&chunk[..room.min(FRAME)]);
            }
            start.extend(session.trigger.frame(session.speech.len(), speech));
        }
        session.carry.drain(..used);
        for run in start {
            self.launch(run);
        }
    }

    /// The request's frames ended.
    pub fn end(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let (epoch, speech) = (session.epoch, session.speech.len());
        if session.launched == 0 && speech >= MIN_SPEECH_SAMPLES {
            self.launch(1);
        }
        let launched = self.session.take().map_or(0, |s| s.launched);
        if launched == 0 {
            // No run asked the model: either it can't, or there was nothing
            // to ask it about.
            let outcome = if self.ready() {
                Outcome::TooShort
            } else {
                Outcome::NoModel
            };
            self.note(epoch, outcome, speech);
        }
        let update = self.speaker.end(epoch);
        self.deliver(epoch, update);
    }

    fn ready(&self) -> bool {
        self.people.recognizer().state() == State::Ready
    }

    fn note(&self, epoch: u64, outcome: Outcome, speech: usize) {
        let run = Run {
            outcome,
            run: 1,
            seconds: speech as f32 / SAMPLE_RATE,
            took: Duration::ZERO,
        };
        record(&self.counters, &run);
        let update = self.speaker.note(epoch, run);
        self.deliver(epoch, update);
    }

    fn deliver(&self, epoch: u64, update: Update) {
        deliver(&self.brain, epoch, update);
    }

    /// Start run `run` on the speech so far, unless the model isn't ready.
    fn launch(&mut self, run: u8) {
        if !self.ready() {
            return;
        }
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(window) = fill_window(&session.speech) else {
            return;
        };
        if !self.speaker.begin_run(session.epoch) {
            return;
        }
        session.launched += 1;
        let job = Job {
            people: self.people.clone(),
            speaker: self.speaker.clone(),
            brain: self.brain.clone(),
            counters: self.counters.clone(),
            epoch: session.epoch,
            run,
            seconds: session.speech.len() as f32 / SAMPLE_RATE,
        };
        tokio::spawn(job.run(window));
    }
}

fn deliver(brain: &mpsc::Sender<Input>, epoch: u64, update: Update) {
    if let Some(line) = update.settled {
        tracing::info!("speaker: {line}");
    }
    if let Some(value) = update.publish {
        // A full queue loses a name that the session's end would repeat only
        // if it changed again; the Brain's queue is deep enough in practice.
        if brain.try_send(Input::LastSpeaker { epoch, value }).is_err() {
            tracing::warn!("Last speaker: the brain's queue is full");
        }
    }
}

/// A run's metrics and timeline span. A run that asked the model records how
/// long it took.
fn record(counters: &Counters, run: &Run) {
    let (score, margin) = match &run.outcome {
        Outcome::Person { score, margin, .. } => (Some(*score), Some(*margin)),
        Outcome::Uncertain {
            best: Some((_, score)),
            margin,
        } => (Some(*score), Some(*margin)),
        _ => (None, None),
    };
    let asked = matches!(
        run.outcome,
        Outcome::Person { .. } | Outcome::Uncertain { .. } | Outcome::Failed(_)
    );
    counters.speaker(
        run.outcome.label(),
        score,
        margin,
        asked.then_some(run.took),
    );
    timeline::stage(|| format!("speaker: {}", run.describe()), run.took, 0);
}

struct Job {
    people: Arc<People>,
    speaker: Arc<Speaker>,
    brain: mpsc::Sender<Input>,
    counters: Counters,
    epoch: u64,
    run: u8,
    seconds: f32,
}

impl Job {
    async fn run(self, window: Vec<i16>) {
        let started = Instant::now();
        let outcome = identify(&self.people, window).await;
        let run = Run {
            outcome,
            run: self.run,
            seconds: self.seconds,
            took: started.elapsed(),
        };
        record(&self.counters, &run);
        let update = self.speaker.finish_run(self.epoch, run);
        deliver(&self.brain, self.epoch, update);
    }
}

/// The window's voiceprint, matched against the usable ones.
async fn identify(people: &Arc<People>, window: Vec<i16>) -> Outcome {
    let enrolled = {
        let people = people.clone();
        tokio::task::spawn_blocking(move || people.usable_voiceprints()).await
    };
    let enrolled = match enrolled {
        Ok(enrolled) if !enrolled.is_empty() => enrolled,
        Ok(_) => return Outcome::NoModel,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let probe = match people.recognizer().identify(window).await {
        Ok(Ok(voiceprint)) => voiceprint,
        Ok(Err(why)) => return Outcome::Failed(why),
        Err(_) => return Outcome::Failed("the speaker model stopped".into()),
    };
    matched(&probe, &enrolled)
}

fn matched(probe: &Voiceprint, enrolled: &[(String, String, Voiceprint)]) -> Outcome {
    let prints: Vec<(usize, Voiceprint)> = enrolled
        .iter()
        .enumerate()
        .map(|(at, (_, _, voiceprint))| (at, voiceprint.clone()))
        .collect();
    match best_match(probe, &prints, &Thresholds::default()) {
        pinna_speaker::Match::Person { id, score, margin } => Outcome::Person {
            id: enrolled[id].0.clone(),
            name: enrolled[id].1.clone(),
            score,
            margin,
        },
        pinna_speaker::Match::Uncertain { best, margin } => Outcome::Uncertain {
            best: best.map(|(at, score)| (enrolled[at].1.clone(), score)),
            margin,
        },
    }
}

#[cfg(test)]
mod tests;
