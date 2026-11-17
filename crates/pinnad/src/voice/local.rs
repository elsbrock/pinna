//! Standalone run state owned by the Brain (ADR 0017).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use pinna_assist::audio::MicInput;
use pinna_assist::conversation::{Conversation, Turn};
use pinna_assist::error::ProviderError;
use pinna_assist::limits::RunLimits;
use pinna_assist::observe::Guard;
use pinna_assist::presets::HistoryPolicy;
use pinna_assist::reply::{ReplySink, ReplySource};
use pinna_assist::run::{RunEvent, RunKind};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::{AssistStatus, Phase, RunId};
use crate::standalone::AssistLive;

/// Standalone wiring kept separate from `Options` so existing test builders
/// remain exhaustive struct literals.
pub struct LocalOptions {
    pub live: watch::Receiver<Arc<AssistLive>>,
    pub spawner: Arc<dyn RunSpawner>,
    pub timers_file: PathBuf,
}

impl LocalOptions {
    #[cfg(test)]
    fn beside_settings(settings_file: &std::path::Path) -> Self {
        let (_tx, live) = watch::channel(Arc::new(AssistLive::empty()));
        Self {
            live,
            spawner: Arc::new(NoopSpawner),
            timers_file: settings_file.with_file_name("timers.toml"),
        }
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        watch::Receiver<Arc<AssistLive>>,
        Arc<dyn RunSpawner>,
        PathBuf,
    ) {
        (self.live, self.spawner, self.timers_file)
    }
}

pub(super) struct Local {
    #[allow(dead_code)]
    pub(super) run: Option<LocalRun>,
    #[allow(dead_code)]
    next_run: u64,
    #[allow(dead_code)]
    conversation: Conversation,
    #[allow(dead_code)]
    chat_gen: u64,
    #[allow(dead_code)]
    noise_seed: Option<u64>,
    live: watch::Receiver<Arc<AssistLive>>,
    #[allow(dead_code)]
    spawner: Arc<dyn RunSpawner>,
    last_error: Option<String>,
    #[allow(dead_code)]
    pending_mute: bool,
}

impl Local {
    pub(super) fn new(
        live: watch::Receiver<Arc<AssistLive>>,
        spawner: Arc<dyn RunSpawner>,
    ) -> Self {
        Self {
            run: None,
            next_run: 1,
            conversation: Conversation::new(HistoryPolicy::Turns(10)),
            chat_gen: 0,
            noise_seed: None,
            live,
            spawner,
            last_error: None,
            pending_mute: false,
        }
    }

    #[cfg(test)]
    pub(super) fn beside_settings(settings_file: &std::path::Path) -> LocalOptions {
        LocalOptions::beside_settings(settings_file)
    }

    pub(super) fn status(&self) -> AssistStatus {
        let live = self.live.borrow();
        AssistStatus {
            ready: live.ready,
            slots: live.slots,
            label: live.label.clone(),
            last_error: self.last_error.clone(),
        }
    }
}

#[allow(dead_code)]
pub(super) struct LocalRun {
    id: RunId,
    kind: RunKind,
    task: Option<JoinHandle<()>>,
    deadline: Instant,
    limits: RunLimits,
    source: Option<ReplySource>,
    done: Option<Option<Turn>>,
    spoke: bool,
    player_done: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
enum Effect {
    CloseMic,
    EndOfSpeech,
    Event(&'static str),
    ErrorCue,
    StartPlayer,
    Finish,
    Fail(ProviderError),
    Guard(Guard),
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadlineRule {
    Keep,
    Think,
    Round,
    Reply,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
struct Step {
    phase: Phase,
    deadline: DeadlineRule,
    effects: Vec<Effect>,
}

#[allow(dead_code)]
fn unchanged(phase: Phase) -> Step {
    Step {
        phase,
        deadline: DeadlineRule::Keep,
        effects: Vec::new(),
    }
}

#[allow(dead_code)]
fn step(phase: Phase, kind: &RunKind, ev: &RunEvent, now: Instant) -> Step {
    match (phase, kind, ev) {
        (Phase::Listening(_), RunKind::Request | RunKind::FollowUp, RunEvent::SpeechEnded) => {
            Step {
                phase: Phase::Thinking(now),
                deadline: DeadlineRule::Think,
                effects: vec![
                    Effect::CloseMic,
                    Effect::EndOfSpeech,
                    Effect::Event("thinking"),
                ],
            }
        }
        (Phase::Listening(_), RunKind::Request, RunEvent::NoSpeech { invented }) => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![
                Effect::CloseMic,
                Effect::Guard(if *invented {
                    Guard::Invented
                } else {
                    Guard::NoSpeech
                }),
                Effect::Event("stopped"),
                Effect::ErrorCue,
            ],
        },
        (Phase::Listening(_), RunKind::FollowUp, RunEvent::NoSpeech { invented }) => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![
                Effect::CloseMic,
                Effect::Guard(if *invented {
                    Guard::Invented
                } else {
                    Guard::NoSpeech
                }),
                Effect::Finish,
            ],
        },
        (Phase::Thinking(_) | Phase::Speaking, _, RunEvent::Progress) => Step {
            phase,
            deadline: DeadlineRule::Round,
            effects: Vec::new(),
        },
        (Phase::Thinking(_), _, RunEvent::ReplyReady) => Step {
            phase,
            deadline: DeadlineRule::Reply,
            effects: vec![Effect::StartPlayer],
        },
        (phase, _, RunEvent::Done { spoke: false, .. }) if phase != Phase::Idle => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![Effect::Finish],
        },
        (phase, _, RunEvent::Done { spoke: true, .. }) => unchanged(phase),
        (phase, _, RunEvent::Failed(error)) if phase != Phase::Idle => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![Effect::Fail(error.clone())],
        },
        _ => unchanged(phase),
    }
}

#[allow(dead_code)]
pub(crate) trait RunSpawner: Send + Sync {
    fn spawn(&self, start: LocalStart) -> JoinHandle<()>;
}

pub(crate) struct NoopSpawner;

impl RunSpawner for NoopSpawner {
    fn spawn(&self, _start: LocalStart) -> JoinHandle<()> {
        tokio::spawn(std::future::pending())
    }
}

#[allow(dead_code)]
pub(crate) struct LocalStart {
    pub run: RunId,
    pub kind: RunKind,
    pub live: Arc<AssistLive>,
    pub history: Vec<Turn>,
    pub mic: Option<MicInput>,
    pub reply: ReplySink,
    pub device: DeviceFacts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceFacts {
    pub name: String,
    pub time_zone: Option<String>,
    pub device_id: String,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pinna_assist::error::{Reason, Slot};
    use pinna_assist::presets::PresetId;

    use super::*;

    fn kinds() -> [RunKind; 3] {
        [
            RunKind::Request,
            RunKind::FollowUp,
            RunKind::Typed("question".into()),
        ]
    }

    fn failure() -> ProviderError {
        ProviderError::new(Slot::Chat, PresetId::OpenAi, Reason::Timeout)
    }

    #[test]
    fn speech_ended_closes_the_microphone_and_starts_thinking() {
        let now = Instant::now();
        for kind in &kinds()[..2] {
            assert_eq!(
                step(Phase::Listening(now), kind, &RunEvent::SpeechEnded, now),
                Step {
                    phase: Phase::Thinking(now),
                    deadline: DeadlineRule::Think,
                    effects: vec![
                        Effect::CloseMic,
                        Effect::EndOfSpeech,
                        Effect::Event("thinking"),
                    ],
                }
            );
        }
    }

    #[test]
    fn no_speech_after_a_request_has_the_home_assistant_error_feedback() {
        let now = Instant::now();
        for (invented, guard) in [(false, Guard::NoSpeech), (true, Guard::Invented)] {
            assert_eq!(
                step(
                    Phase::Listening(now),
                    &RunKind::Request,
                    &RunEvent::NoSpeech { invented },
                    now,
                ),
                Step {
                    phase: Phase::Idle,
                    deadline: DeadlineRule::Keep,
                    effects: vec![
                        Effect::CloseMic,
                        Effect::Guard(guard),
                        Effect::Event("stopped"),
                        Effect::ErrorCue,
                    ],
                }
            );
        }
    }

    #[test]
    fn no_speech_after_a_follow_up_finishes_quietly() {
        let now = Instant::now();
        for (invented, guard) in [(false, Guard::NoSpeech), (true, Guard::Invented)] {
            assert_eq!(
                step(
                    Phase::Listening(now),
                    &RunKind::FollowUp,
                    &RunEvent::NoSpeech { invented },
                    now,
                ),
                Step {
                    phase: Phase::Idle,
                    deadline: DeadlineRule::Keep,
                    effects: vec![Effect::CloseMic, Effect::Guard(guard), Effect::Finish],
                }
            );
        }
    }

    #[test]
    fn progress_rearms_the_current_round_while_thinking_or_speaking() {
        let now = Instant::now();
        for kind in &kinds() {
            for phase in [Phase::Thinking(now), Phase::Speaking] {
                assert_eq!(
                    step(phase, kind, &RunEvent::Progress, now),
                    Step {
                        phase,
                        deadline: DeadlineRule::Round,
                        effects: Vec::new(),
                    }
                );
            }
        }
    }

    #[test]
    fn reply_ready_starts_the_player_without_announcing_speech() {
        let now = Instant::now();
        for kind in &kinds() {
            assert_eq!(
                step(Phase::Thinking(now), kind, &RunEvent::ReplyReady, now),
                Step {
                    phase: Phase::Thinking(now),
                    deadline: DeadlineRule::Reply,
                    effects: vec![Effect::StartPlayer],
                }
            );
        }
    }

    #[test]
    fn done_finishes_at_once_only_when_nothing_was_spoken() {
        let now = Instant::now();
        for kind in &kinds() {
            assert_eq!(
                step(
                    Phase::Thinking(now),
                    kind,
                    &RunEvent::Done {
                        turn: None,
                        spoke: false,
                    },
                    now,
                ),
                Step {
                    phase: Phase::Idle,
                    deadline: DeadlineRule::Keep,
                    effects: vec![Effect::Finish],
                }
            );
            assert_eq!(
                step(
                    Phase::Speaking,
                    kind,
                    &RunEvent::Done {
                        turn: None,
                        spoke: true,
                    },
                    now,
                ),
                unchanged(Phase::Speaking)
            );
        }
    }

    #[test]
    fn failure_ends_every_active_run_with_its_provider_error() {
        let now = Instant::now();
        for kind in &kinds() {
            for phase in [
                Phase::Starting(now),
                Phase::Listening(now),
                Phase::Thinking(now),
                Phase::Speaking,
            ] {
                assert_eq!(
                    step(phase, kind, &RunEvent::Failed(failure()), now),
                    Step {
                        phase: Phase::Idle,
                        deadline: DeadlineRule::Keep,
                        effects: vec![Effect::Fail(failure())],
                    }
                );
            }
        }
    }

    #[test]
    fn events_from_the_wrong_phase_or_a_typed_listen_are_ignored() {
        let now = Instant::now();
        let listening = Phase::Listening(now);
        assert_eq!(
            step(
                listening,
                &RunKind::Typed("question".into()),
                &RunEvent::SpeechEnded,
                now
            ),
            unchanged(listening)
        );
        assert_eq!(
            step(
                listening,
                &RunKind::Typed("question".into()),
                &RunEvent::NoSpeech { invented: false },
                now,
            ),
            unchanged(listening)
        );
        for kind in &kinds() {
            assert_eq!(
                step(Phase::Thinking(now), kind, &RunEvent::SpeechEnded, now),
                unchanged(Phase::Thinking(now))
            );
            assert_eq!(
                step(listening, kind, &RunEvent::ReplyReady, now),
                unchanged(listening)
            );
            assert_eq!(
                step(listening, kind, &RunEvent::Progress, now),
                unchanged(listening)
            );
            for event in [
                RunEvent::SpeechEnded,
                RunEvent::NoSpeech { invented: false },
                RunEvent::Progress,
                RunEvent::ReplyReady,
                RunEvent::Done {
                    turn: None,
                    spoke: false,
                },
                RunEvent::Failed(failure()),
            ] {
                assert_eq!(step(Phase::Idle, kind, &event, now), unchanged(Phase::Idle));
            }
        }
    }

    #[test]
    fn transition_tests_use_real_instants_not_tokio_time() {
        let start = Instant::now();
        let later = start + Duration::from_secs(2);
        let Step { phase, .. } = step(
            Phase::Listening(start),
            &RunKind::Request,
            &RunEvent::SpeechEnded,
            later,
        );
        assert_eq!(phase, Phase::Thinking(later));
    }
}
