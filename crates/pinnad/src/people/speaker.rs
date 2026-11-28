//! Who speaks in the current request, as recognition found out so far
//! (ADR 0038).
//!
//! One slot, shared by the speaker tap, which fills it, and by whoever needs
//! the answer: the Brain for "Last speaker" and, in a later step, the
//! standalone run. A new session resets it, so nobody reads the previous
//! request's outcome. Readers take what is there now; nothing waits.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// What one recognition run found.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Person {
        id: String,
        name: String,
        score: f32,
        margin: f32,
    },
    /// Nobody matched closely enough. The best guess, with its score.
    Uncertain {
        best: Option<(String, f32)>,
        margin: f32,
    },
    /// The request had under 0.5 s of speech.
    TooShort,
    /// Nobody has a usable voiceprint, or the model isn't ready.
    NoModel,
    /// The model was busy or failed. The sentence says why.
    Failed(String),
}

impl Outcome {
    /// The metrics label.
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Person { .. } => "person",
            Outcome::Uncertain { .. } => "uncertain",
            Outcome::TooShort => "too_short",
            Outcome::NoModel => "no_model",
            Outcome::Failed(_) => "failed",
        }
    }
}

/// An outcome, with which run of the request found it.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub outcome: Outcome,
    /// 1 or 2.
    pub run: u8,
    /// Seconds of speech the run used.
    pub seconds: f32,
    /// From asking the model until the match.
    pub took: Duration,
}

impl Run {
    /// The facts for the timeline and the log: "Anna (0.82, margin 0.31),
    /// 1.5 s, run 1".
    pub fn describe(&self) -> String {
        let what = match &self.outcome {
            Outcome::Person {
                name,
                score,
                margin,
                ..
            } => format!("{name} ({score:.2}, margin {margin:.2})"),
            Outcome::Uncertain {
                best: Some((name, score)),
                margin,
            } => format!("uncertain (best {name} {score:.2}, margin {margin:.2})"),
            Outcome::Uncertain { best: None, .. } => "uncertain (nobody to compare)".into(),
            Outcome::TooShort => "too short".into(),
            Outcome::NoModel => "no model".into(),
            Outcome::Failed(why) => format!("failed ({why})"),
        };
        format!("{what}, {:.1} s, run {}", self.seconds, self.run)
    }
}

/// What a session decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Person { id: String, name: String },
    EveryoneElse,
}

/// What a change to the slot asks of its caller.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// The new value of "Last speaker", when it changed in this session.
    pub publish: Option<String>,
    /// The log line, once, when the session's decision is final.
    pub settled: Option<String>,
}

#[derive(Default)]
struct State {
    /// Counts sessions and resets, so a run that finishes late knows its
    /// session is gone.
    epoch: u64,
    runs: Vec<Run>,
    /// Runs asked for that haven't answered.
    pending: usize,
    /// The session's frames ended.
    over: bool,
    /// What "Last speaker" shows in this session: empty at its start.
    published: String,
    settled: bool,
}

impl State {
    fn decision(&self) -> Option<Decision> {
        let person = self.runs.iter().rev().find_map(|r| match &r.outcome {
            Outcome::Person { id, name, .. } => Some(Decision::Person {
                id: id.clone(),
                name: name.clone(),
            }),
            _ => None,
        });
        match person {
            Some(person) => Some(person),
            None if self.over && self.pending == 0 => Some(Decision::EveryoneElse),
            None => None,
        }
    }

    fn update(&mut self) -> Update {
        let Some(decision) = self.decision() else {
            return Update::default();
        };
        let value = match &decision {
            Decision::Person { name, .. } => name.clone(),
            Decision::EveryoneElse => "Everyone else".into(),
        };
        let publish = (value != self.published).then(|| value.clone());
        self.published = value;
        let settled = (self.over && self.pending == 0 && !self.settled).then(|| {
            self.settled = true;
            match (&decision, self.runs.last()) {
                (Decision::Person { .. }, _) => self
                    .runs
                    .iter()
                    .rev()
                    .find(|r| matches!(r.outcome, Outcome::Person { .. }))
                    .map(Run::describe)
                    .unwrap_or_default(),
                (Decision::EveryoneElse, Some(last)) => {
                    format!("Everyone else; {}", last.describe())
                }
                (Decision::EveryoneElse, None) => "Everyone else; no runs".into(),
            }
        });
        Update { publish, settled }
    }
}

/// The slot. A session is an epoch: every change names its epoch and is
/// ignored when a newer session has reset the slot meanwhile.
#[derive(Default)]
pub struct Speaker(Mutex<State>);

impl Speaker {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A request starts: forget everything and return the new session's
    /// epoch. The Brain calls it too, so nothing of an older session
    /// reaches "Last speaker" after the sensor was cleared.
    pub fn reset(&self) -> u64 {
        let mut state = self.state();
        let epoch = state.epoch + 1;
        *state = State {
            epoch,
            ..State::default()
        };
        epoch
    }

    pub fn epoch(&self) -> u64 {
        self.state().epoch
    }

    /// What the session decided so far: the newest confident person, else
    /// Everyone else once the session is over and every run has answered,
    /// else nothing yet.
    #[allow(dead_code)] // The standalone run reads it, in a later step.
    pub fn decision(&self) -> Option<Decision> {
        self.state().decision()
    }

    #[cfg(test)]
    pub fn runs(&self) -> (Vec<Run>, usize) {
        let state = self.state();
        (state.runs.clone(), state.pending)
    }

    /// A run was asked for. False when the session is gone.
    pub fn begin_run(&self, epoch: u64) -> bool {
        let mut state = self.state();
        if state.epoch != epoch {
            return false;
        }
        state.pending += 1;
        true
    }

    /// A run asked for answered.
    pub fn finish_run(&self, epoch: u64, run: Run) -> Update {
        let mut state = self.state();
        if state.epoch != epoch {
            return Update::default();
        }
        state.pending = state.pending.saturating_sub(1);
        state.runs.push(run);
        state.update()
    }

    /// An outcome that needed no run: too short, or no model.
    pub fn note(&self, epoch: u64, run: Run) -> Update {
        let mut state = self.state();
        if state.epoch != epoch {
            return Update::default();
        }
        state.runs.push(run);
        state.update()
    }

    /// The session's frames ended.
    pub fn end(&self, epoch: u64) -> Update {
        let mut state = self.state();
        if state.epoch != epoch {
            return Update::default();
        }
        state.over = true;
        state.update()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, run: u8) -> Run {
        Run {
            outcome: Outcome::Person {
                id: format!("id-{name}"),
                name: name.into(),
                score: 0.82,
                margin: 0.31,
            },
            run,
            seconds: 1.5,
            took: Duration::from_millis(800),
        }
    }

    fn uncertain(run: u8) -> Run {
        Run {
            outcome: Outcome::Uncertain {
                best: Some(("Anna".into(), 0.41)),
                margin: 0.05,
            },
            run,
            seconds: 0.9,
            took: Duration::from_millis(800),
        }
    }

    #[test]
    fn a_new_session_resets_the_slot() {
        let slot = Speaker::default();
        let first = slot.reset();
        assert!(slot.begin_run(first));
        slot.finish_run(first, person("Anna", 1));
        assert!(matches!(slot.decision(), Some(Decision::Person { .. })));

        let second = slot.reset();
        assert_ne!(first, second);
        assert_eq!(slot.decision(), None);
    }

    #[test]
    fn a_run_of_an_older_session_is_ignored() {
        let slot = Speaker::default();
        let old = slot.reset();
        assert!(slot.begin_run(old));
        let new = slot.reset();
        assert_eq!(slot.finish_run(old, person("Anna", 1)), Update::default());
        assert_eq!(slot.end(old), Update::default());
        assert!(!slot.begin_run(old));
        assert_eq!(slot.epoch(), new);
        assert_eq!(slot.decision(), None);
    }

    #[test]
    fn a_confident_person_is_the_decision_at_once_and_published_once() {
        let slot = Speaker::default();
        let epoch = slot.reset();
        assert!(slot.begin_run(epoch));
        let update = slot.finish_run(epoch, person("Anna", 1));
        assert_eq!(update.publish.as_deref(), Some("Anna"));
        assert_eq!(update.settled, None);
        // The same person again, and the session's end, change nothing.
        assert!(slot.begin_run(epoch));
        let update = slot.finish_run(epoch, person("Anna", 2));
        assert_eq!(update.publish, None);
        let update = slot.end(epoch);
        assert_eq!(update.publish, None);
        assert_eq!(
            update.settled.as_deref(),
            Some("Anna (0.82, margin 0.31), 1.5 s, run 2")
        );
    }

    #[test]
    fn the_newest_confident_person_wins() {
        let slot = Speaker::default();
        let epoch = slot.reset();
        for (name, run) in [("Anna", 1), ("Ben", 2)] {
            assert!(slot.begin_run(epoch));
            slot.finish_run(epoch, person(name, run));
        }
        // A later uncertain run doesn't take it back.
        assert!(slot.begin_run(epoch));
        slot.finish_run(epoch, uncertain(2));
        assert_eq!(
            slot.decision(),
            Some(Decision::Person {
                id: "id-Ben".into(),
                name: "Ben".into()
            })
        );
    }

    #[test]
    fn everyone_else_waits_for_the_end_and_every_run() {
        let slot = Speaker::default();
        let epoch = slot.reset();
        assert!(slot.begin_run(epoch));
        assert_eq!(slot.end(epoch), Update::default());
        assert_eq!(slot.decision(), None);
        let update = slot.finish_run(epoch, uncertain(1));
        assert_eq!(update.publish.as_deref(), Some("Everyone else"));
        assert_eq!(
            update.settled.as_deref(),
            Some("Everyone else; uncertain (best Anna 0.41, margin 0.05), 0.9 s, run 1")
        );
        assert_eq!(slot.decision(), Some(Decision::EveryoneElse));
    }

    #[test]
    fn a_session_without_runs_ends_as_everyone_else() {
        let slot = Speaker::default();
        let epoch = slot.reset();
        let update = slot.note(
            epoch,
            Run {
                outcome: Outcome::TooShort,
                run: 1,
                seconds: 0.3,
                took: Duration::ZERO,
            },
        );
        assert_eq!(update, Update::default());
        let update = slot.end(epoch);
        assert_eq!(update.publish.as_deref(), Some("Everyone else"));
        assert_eq!(
            update.settled.as_deref(),
            Some("Everyone else; too short, 0.3 s, run 1")
        );
    }

    #[test]
    fn a_reader_never_sees_the_previous_decision() {
        let slot = Speaker::default();
        let epoch = slot.reset();
        slot.end(epoch);
        assert_eq!(slot.decision(), Some(Decision::EveryoneElse));
        slot.reset();
        assert_eq!(slot.decision(), None);
    }
}
