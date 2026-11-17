//! Standalone run state owned by the Brain (ADR 0017).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use pinna_assist::audio::MicInput;
use pinna_assist::conversation::{Conversation, Turn};
use pinna_assist::limits::RunLimits;
use pinna_assist::presets::HistoryPolicy;
use pinna_assist::reply::{ReplySink, ReplySource};
use pinna_assist::run::RunKind;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::{AssistStatus, RunId};
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
