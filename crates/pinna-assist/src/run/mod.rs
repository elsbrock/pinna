//! The events exchanged by one standalone run and the Brain.

use futures_core::future::BoxFuture;

pub mod listen;
#[allow(dead_code)]
pub(crate) mod reply;

pub use crate::conversation::Turn;
use crate::error::ProviderError;

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
