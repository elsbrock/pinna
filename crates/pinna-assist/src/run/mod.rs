//! The events exchanged by one standalone run and the Brain.

use futures_core::future::BoxFuture;

use crate::error::ProviderError;

/// One text exchange. The conversation module owns the history policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub user: String,
    pub assistant: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunKind {
    Request,
    FollowUp,
    Typed(String),
}

#[derive(Clone, Debug)]
pub enum RunEvent {
    SpeechEnded,
    NoSpeech { invented: bool },
    Progress,
    ReplyReady,
    Done { turn: Option<Turn>, spoke: bool },
    Failed(ProviderError),
}

pub trait RunEvents: Send + Sync {
    fn emit(&self, ev: RunEvent) -> BoxFuture<'_, ()>;
}
