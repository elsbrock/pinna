//! The tool loop's protocol-independent state (architecture §7).
//!
//! The harness owns the messages for the current turn and turns streamed text
//! into spoken sentences. Network and tool IO stay in `run::reply`.

use pinna_mcp::{ToolCall, ToolOutput};
use serde_json::Value;

use crate::{
    chat::{ChatDelta, Message},
    conversation::Turn,
    error::Reason,
    limits::RunLimits,
    sentences::Sentences,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Sentence(String),
    Usage { input: u64, output: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    CallTools(Vec<ToolCall>),
    Done { reply: String },
    Fail(Reason),
}

pub struct Harness {
    user: String,
    limits: RunLimits,
    tools_offered: bool,
    messages: Vec<Message>,
    sentences: Sentences,
    round: u8,
    round_text: String,
    calls: Vec<ToolCall>,
    /// The provider token for this round's calls (Gemini).
    signature: Option<String>,
    reply: String,
    pending: Vec<Out>,
}

impl Harness {
    pub fn new(user: String, limits: &RunLimits, tools_offered: bool) -> Self {
        Self {
            messages: vec![Message::User(user.clone())],
            sentences: Sentences::new(limits.spoken_chars),
            user,
            limits: *limits,
            tools_offered,
            round: 1,
            round_text: String::new(),
            calls: Vec::new(),
            signature: None,
            reply: String::new(),
            pending: Vec::new(),
        }
    }

    pub fn on_delta(&mut self, delta: ChatDelta) -> Vec<Out> {
        let mut out = std::mem::take(&mut self.pending);
        match delta {
            ChatDelta::Text(text) => {
                self.round_text.push_str(&text);
                out.extend(self.sentences.push(&text).into_iter().map(Out::Sentence));
            }
            ChatDelta::Call(call) => {
                if self.tools_offered {
                    self.calls.push(call);
                }
            }
            ChatDelta::Signature(signature) => {
                // A parallel batch carries one, on its first call.
                if self.tools_offered && self.signature.is_none() {
                    self.signature = Some(signature);
                }
            }
            ChatDelta::Usage { input, output } => out.push(Out::Usage { input, output }),
            ChatDelta::End => self.flush(&mut out),
        }
        out
    }

    pub fn on_stream_end(&mut self) -> Step {
        let mut pending = std::mem::take(&mut self.pending);
        self.flush(&mut pending);
        self.pending = pending;
        self.reply.push_str(&self.round_text);

        let calls = std::mem::take(&mut self.calls);
        let signature = self.signature.take();
        if !self.round_text.is_empty() || !calls.is_empty() {
            self.messages.push(Message::Assistant {
                text: std::mem::take(&mut self.round_text),
                calls: calls.clone(),
                signature,
            });
        } else {
            self.round_text.clear();
        }

        if calls.is_empty() || !self.tools_offered {
            return Step::Done {
                reply: self.reply.clone(),
            };
        }
        if self.round >= self.limits.rounds {
            return Step::Fail(Reason::Protocol);
        }

        let mut valid = Vec::new();
        for call in calls {
            if call.args.is_object() {
                valid.push(call);
            } else {
                self.messages.push(Message::ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    output: invalid_arguments(&call.args),
                });
            }
        }
        Step::CallTools(valid)
    }

    pub fn on_results(&mut self, results: Vec<(ToolCall, ToolOutput)>) {
        for (call, output) in results {
            self.messages.push(Message::ToolResult {
                call_id: call.id,
                name: call.name,
                output,
            });
        }
        self.round = self.round.saturating_add(1);
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn turn(&self) -> Turn {
        Turn {
            user: self.user.clone(),
            assistant: self.reply.clone(),
        }
    }

    /// `run::reply` drains a fragment flushed by `on_stream_end`. Normally the
    /// adapter's `End` delta has already returned it from `on_delta`.
    pub(crate) fn drain(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.pending)
    }

    fn flush(&mut self, out: &mut Vec<Out>) {
        if let Some(sentence) = self.sentences.finish() {
            out.push(Out::Sentence(sentence));
        }
    }
}

fn invalid_arguments(args: &Value) -> ToolOutput {
    let _ = args;
    ToolOutput::error("The brain supplied invalid tool arguments.")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn call(id: &str, args: Value) -> ChatDelta {
        ChatDelta::Call(ToolCall {
            id: id.into(),
            name: "weather".into(),
            args,
        })
    }

    #[test]
    fn text_only_becomes_sentences_and_a_turn() {
        let mut harness = Harness::new("How are you?".into(), &RunLimits::MODEL, true);
        assert!(
            harness
                .on_delta(ChatDelta::Text("I am well. ".into()))
                .is_empty()
        );
        assert_eq!(
            harness.on_delta(ChatDelta::Text("Thanks for asking.".into())),
            [Out::Sentence("I am well.".into())]
        );
        assert_eq!(
            harness.on_delta(ChatDelta::End),
            [Out::Sentence("Thanks for asking.".into())]
        );
        assert_eq!(
            harness.on_stream_end(),
            Step::Done {
                reply: "I am well. Thanks for asking.".into()
            }
        );
        assert_eq!(
            harness.turn(),
            Turn {
                user: "How are you?".into(),
                assistant: "I am well. Thanks for asking.".into(),
            }
        );
    }

    #[test]
    fn text_and_interleaved_calls_keep_protocol_messages() {
        let mut harness = Harness::new("Weather?".into(), &RunLimits::MODEL, true);
        harness.on_delta(ChatDelta::Text("I will check. ".into()));
        harness.on_delta(call("one", json!({"place": "Berlin"})));
        harness.on_delta(call("two", json!({"place": "Munich"})));
        harness.on_delta(ChatDelta::End);
        let Step::CallTools(calls) = harness.on_stream_end() else {
            panic!("tools should run");
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "one");
        assert_eq!(calls[1].id, "two");

        harness.on_results(vec![
            (calls[0].clone(), ToolOutput::ok("18 degrees")),
            (calls[1].clone(), ToolOutput::error("Unknown place.")),
        ]);
        assert_eq!(harness.messages().len(), 4);
        assert!(matches!(
            &harness.messages()[1],
            Message::Assistant { calls, .. } if calls.len() == 2
        ));
        assert!(matches!(
            &harness.messages()[3],
            Message::ToolResult { output, .. } if output.is_error
        ));

        harness.on_delta(ChatDelta::Text("Both are mild.".into()));
        harness.on_delta(ChatDelta::End);
        assert_eq!(
            harness.on_stream_end(),
            Step::Done {
                reply: "I will check. Both are mild.".into()
            }
        );
    }

    #[test]
    fn a_whole_gemini_call_is_one_tool_round() {
        let mut harness = Harness::new("Time?".into(), &RunLimits::MODEL, true);
        harness.on_delta(ChatDelta::Signature("sig".into()));
        harness.on_delta(call("gemini-call-1", json!({"zone": "Europe/Berlin"})));
        let Step::CallTools(calls) = harness.on_stream_end() else {
            panic!("tools should run");
        };
        assert_eq!(calls[0].args, json!({"zone": "Europe/Berlin"}));
        // The round's signature stays with its calls for the next request.
        assert!(matches!(
            &harness.messages()[1],
            Message::Assistant { signature: Some(s), .. } if s == "sig"
        ));
        harness.on_results(vec![(calls[0].clone(), ToolOutput::ok("14:03"))]);
        harness.on_delta(ChatDelta::Text("It is 14:03.".into()));
        assert!(matches!(harness.on_stream_end(), Step::Done { .. }));
    }

    #[test]
    fn a_sixth_tool_round_is_never_started() {
        let mut harness = Harness::new("Keep going".into(), &RunLimits::MODEL, true);
        for round in 1..RunLimits::MODEL.rounds {
            harness.on_delta(call(&format!("call-{round}"), json!({})));
            let Step::CallTools(calls) = harness.on_stream_end() else {
                panic!("round {round} should call a tool");
            };
            harness.on_results(vec![(calls[0].clone(), ToolOutput::ok("done"))]);
        }
        harness.on_delta(call("call-5", json!({})));
        assert_eq!(harness.on_stream_end(), Step::Fail(Reason::Protocol));
        assert_eq!(harness.round, RunLimits::MODEL.rounds);
    }

    #[test]
    fn invented_json_arguments_become_an_error_without_a_tool_call() {
        let mut harness = Harness::new("Weather?".into(), &RunLimits::MODEL, true);
        harness.on_delta(call("bad", Value::String("{\"place\":".into())));
        assert_eq!(harness.on_stream_end(), Step::CallTools(Vec::new()));
        assert!(matches!(
            &harness.messages()[2],
            Message::ToolResult { call_id, output, .. }
                if call_id == "bad" && output.is_error
        ));
        harness.on_results(Vec::new());
        harness.on_delta(ChatDelta::Text("I could not check.".into()));
        assert!(matches!(harness.on_stream_end(), Step::Done { .. }));
    }

    #[test]
    fn the_spoken_cap_spans_tool_rounds() {
        let limits = RunLimits {
            spoken_chars: 12,
            ..RunLimits::MODEL
        };
        let mut harness = Harness::new("Long reply".into(), &limits, true);
        assert_eq!(
            harness.on_delta(ChatDelta::Text("First sentence.".into())),
            Vec::<Out>::new()
        );
        harness.on_delta(call("one", json!({})));
        let Step::CallTools(calls) = harness.on_stream_end() else {
            panic!("tools should run");
        };
        assert_eq!(harness.drain(), [Out::Sentence("First senten".into())]);
        harness.on_results(vec![(calls[0].clone(), ToolOutput::ok("done"))]);
        assert!(
            harness
                .on_delta(ChatDelta::Text("More text.".into()))
                .is_empty()
        );
        assert!(matches!(harness.on_stream_end(), Step::Done { .. }));
        assert!(harness.drain().is_empty());
    }

    #[test]
    fn hand_off_ignores_calls() {
        let mut harness = Harness::new("Hello".into(), &RunLimits::AGENT, false);
        harness.on_delta(call("agent-owned", json!({})));
        harness.on_delta(ChatDelta::Text("Done.".into()));
        assert_eq!(
            harness.on_stream_end(),
            Step::Done {
                reply: "Done.".into()
            }
        );
        assert!(matches!(
            &harness.messages()[1],
            Message::Assistant { calls, .. } if calls.is_empty()
        ));
    }
}
