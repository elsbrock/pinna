//! Streaming brain protocols and their shared message vocabulary.
//!
//! Each adapter has a pure JSON/SSE codec and a small HTTP shell. Tool-call
//! fragments are assembled here so the tool loop receives complete values.

use std::{collections::VecDeque, pin::Pin};

use bytes::Bytes;
use futures_core::{
    future::BoxFuture,
    stream::{BoxStream, Stream},
};
use futures_util::{StreamExt, stream};
use pinna_http::{NetError, SseDecoder, SseEvent, classify, reqwest};
use pinna_mcp::{ToolCall, ToolSpec};
use reqwest::Response;

use crate::{
    error::{ProviderError, Reason, Slot},
    presets::PresetId,
};

mod anthropic;
pub(crate) use anthropic::API_VERSION as ANTHROPIC_VERSION;
mod gemini;
mod openai;

pub use anthropic::AnthropicChat;
pub use gemini::GeminiChat;
pub use openai::OpenAiChat;

const SSE_LINE_MAX: usize = 256 * 1024;
/// One streamed reply, whole. The line limit alone doesn't bound an event:
/// `data:` lines without a blank line pile up in the decoder. Reasoning
/// tokens arrive and are dropped, so this leaves plenty of room.
const SSE_BODY_MAX: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum ChatDelta {
    Text(String),
    Call(ToolCall),
    /// An opaque token the provider wants back with the round's calls
    /// (Gemini's `thoughtSignature`).
    Signature(String),
    Usage {
        input: u64,
        output: u64,
    },
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    User(String),
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
        /// What [`ChatDelta::Signature`] carried for these calls; sent back
        /// unchanged by the adapter that asked for it.
        signature: Option<String>,
    },
    ToolResult {
        call_id: String,
        name: String,
        output: pinna_mcp::ToolOutput,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ChatRequest<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
    /// The brain may call `tools` this round. On the last round of a tool
    /// loop the tools are still sent, because the conversation already holds
    /// calls to them, but the provider is told not to call any.
    pub calls_allowed: bool,
    pub max_tokens: u32,
    pub user: Option<&'a str>,
}

pub trait Chat: Send + Sync {
    fn stream<'a>(
        &'a self,
        req: ChatRequest<'a>,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<ChatDelta, ProviderError>>, ProviderError>>;
}

type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

struct SseEvents {
    body: BodyStream,
    decoder: SseDecoder,
    pending: VecDeque<SseEvent>,
    eof: bool,
    /// Bytes read from the body so far.
    read: usize,
    preset: PresetId,
    clock_synced: fn() -> bool,
}

impl SseEvents {
    fn new(response: Response, preset: PresetId, clock_synced: fn() -> bool) -> Self {
        Self {
            body: Box::pin(response.bytes_stream()),
            decoder: SseDecoder::new(SSE_LINE_MAX),
            pending: VecDeque::new(),
            eof: false,
            read: 0,
            preset,
            clock_synced,
        }
    }

    async fn next(&mut self) -> Option<Result<SseEvent, ProviderError>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.eof {
                return None;
            }
            match self.body.next().await {
                Some(Ok(chunk)) => {
                    self.read = self.read.saturating_add(chunk.len());
                    if self.read > SSE_BODY_MAX {
                        self.eof = true;
                        return Some(Err(provider_error(self.preset, Reason::TooLarge)));
                    }
                    match self.decoder.push(&chunk) {
                        Ok(events) => self.pending.extend(events),
                        Err(_) => {
                            self.eof = true;
                            return Some(Err(provider_error(self.preset, Reason::TooLarge)));
                        }
                    }
                }
                Some(Err(error)) => {
                    self.eof = true;
                    return Some(Err(network_error(self.preset, &error, self.clock_synced)));
                }
                None => {
                    self.eof = true;
                    return self.decoder.finish().map(Ok);
                }
            }
        }
    }
}

/// Convert a response into deltas. The parser receives `None` at end of input
/// and decides whether that is a normal end or a cut stream for its protocol.
fn response_stream<F>(
    response: Response,
    preset: PresetId,
    clock_synced: fn() -> bool,
    parser: F,
) -> BoxStream<'static, Result<ChatDelta, ProviderError>>
where
    F: FnMut(Option<SseEvent>) -> Result<Vec<ChatDelta>, ProviderError> + Send + 'static,
{
    let events = SseEvents::new(response, preset, clock_synced);
    let state = (events, parser, VecDeque::new(), false);
    stream::unfold(
        state,
        |(mut events, mut parser, mut pending, mut done)| async move {
            loop {
                if let Some(delta) = pending.pop_front() {
                    return Some((Ok(delta), (events, parser, pending, done)));
                }
                if done {
                    return None;
                }
                let event = match events.next().await {
                    Some(Ok(event)) => Some(event),
                    Some(Err(error)) => {
                        return Some((Err(error), (events, parser, pending, true)));
                    }
                    None => None,
                };
                match parser(event) {
                    Ok(deltas) => {
                        done = deltas.iter().any(|delta| matches!(delta, ChatDelta::End));
                        pending.extend(deltas);
                    }
                    Err(error) => {
                        return Some((Err(error), (events, parser, pending, true)));
                    }
                }
            }
        },
    )
    .boxed()
}

fn provider_error(preset: PresetId, reason: Reason) -> ProviderError {
    ProviderError::new(Slot::Chat, preset, reason)
}

fn protocol_error(preset: PresetId) -> ProviderError {
    provider_error(preset, Reason::Protocol)
}

fn network_error(
    preset: PresetId,
    error: &reqwest::Error,
    clock_synced: fn() -> bool,
) -> ProviderError {
    let reason = match classify(error) {
        NetError::Tls if !clock_synced() => Reason::ClockNotSet,
        NetError::Timeout => Reason::Timeout,
        NetError::Unreachable => Reason::Unreachable,
        NetError::Tls => Reason::Tls,
        NetError::Protocol => Reason::Protocol,
    };
    provider_error(preset, reason)
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}{path}", base_url.trim_end_matches('/'))
}
