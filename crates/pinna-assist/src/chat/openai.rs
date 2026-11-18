//! OpenAI-compatible streaming chat completions.

use std::collections::BTreeMap;

use futures_core::{future::BoxFuture, stream::BoxStream};
use pinna_http::{Secret, SseEvent, reqwest};
use pinna_mcp::ToolCall;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Chat, ChatDelta, ChatRequest, Message, endpoint, protocol_error, response_stream};
use crate::{
    error::ProviderError,
    http::{Auth, send},
    presets::PresetId,
};

#[derive(Clone)]
pub struct OpenAiChat {
    client: reqwest::Client,
    base_url: String,
    model: String,
    auth: Auth,
    preset: PresetId,
    clock_synced: fn() -> bool,
}

impl OpenAiChat {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        model: impl Into<String>,
        key: Option<Secret>,
        preset: PresetId,
        clock_synced: fn() -> bool,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            model: model.into(),
            auth: key.map(Auth::Bearer).unwrap_or_default(),
            preset,
            clock_synced,
        }
    }

    fn request_json(&self, req: &ChatRequest<'_>) -> Value {
        let mut messages = vec![json!({"role": "system", "content": req.system})];
        messages.extend(req.messages.iter().map(|message| match message {
            Message::User(text) => json!({"role": "user", "content": text}),
            Message::Assistant { text, calls, .. } => {
                let tool_calls: Vec<_> = calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.args.to_string(),
                            },
                        })
                    })
                    .collect();
                let mut value = json!({"role": "assistant", "content": text});
                if !tool_calls.is_empty() {
                    value["tool_calls"] = Value::Array(tool_calls);
                }
                value
            }
            Message::ToolResult {
                call_id,
                name: _,
                output,
            } => json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output.text,
            }),
        }));

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        // OpenAI's own API refuses `max_tokens` for its reasoning models and
        // takes the newer name for every model; compatible servers know only
        // the older one.
        let limit = if self.preset == PresetId::OpenAi {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[limit] = req.max_tokens.into();
        if !req.tools.is_empty() {
            body["tools"] = req
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        },
                    })
                })
                .collect();
            if !req.calls_allowed {
                body["tool_choice"] = "none".into();
            }
        }
        if let Some(user) = req.user {
            body["user"] = user.into();
        }
        body
    }
}

impl Chat for OpenAiChat {
    fn stream<'a>(
        &'a self,
        req: ChatRequest<'a>,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<ChatDelta, ProviderError>>, ProviderError>> {
        Box::pin(async move {
            let response = send(
                self.client
                    .post(endpoint(&self.base_url, "/chat/completions"))
                    .json(&self.request_json(&req)),
                &self.auth,
                crate::error::Slot::Chat,
                self.preset,
                self.clock_synced,
            )
            .await?;
            let mut decoder = Decoder::new(self.preset);
            Ok(response_stream(
                response,
                self.preset,
                self.clock_synced,
                move |event| decoder.parse(event),
            ))
        })
    }
}

struct Decoder {
    preset: PresetId,
    calls: BTreeMap<usize, PendingCall>,
    input: u64,
    output: u64,
    usage_seen: bool,
}

impl Decoder {
    fn new(preset: PresetId) -> Self {
        Self {
            preset,
            calls: BTreeMap::new(),
            input: 0,
            output: 0,
            usage_seen: false,
        }
    }

    fn parse(&mut self, event: Option<SseEvent>) -> Result<Vec<ChatDelta>, ProviderError> {
        let preset = self.preset;
        let Some(event) = event else {
            return Err(protocol_error(preset));
        };
        if event.data.trim() == "[DONE]" {
            let mut deltas = self.take_calls(preset)?;
            if self.usage_seen {
                deltas.push(ChatDelta::Usage {
                    input: self.input,
                    output: self.output,
                });
            }
            deltas.push(ChatDelta::End);
            return Ok(deltas);
        }

        let chunk: Chunk = serde_json::from_str(&event.data).map_err(|_| protocol_error(preset))?;
        let mut deltas = Vec::new();
        for choice in chunk.choices {
            if let Some(text) = choice.delta.content
                && !text.is_empty()
            {
                deltas.push(ChatDelta::Text(text));
            }
            for call in choice.delta.tool_calls {
                let pending = self.calls.entry(call.index).or_default();
                if let Some(id) = call.id {
                    pending.id = id;
                }
                if let Some(function) = call.function {
                    if let Some(name) = function.name {
                        pending.name = name;
                    }
                    if let Some(arguments) = function.arguments {
                        pending.arguments.push_str(&arguments);
                    }
                }
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage_seen = true;
            self.input = self.input.saturating_add(usage.prompt_tokens);
            self.output = self.output.saturating_add(usage.completion_tokens);
        }
        Ok(deltas)
    }

    fn take_calls(&mut self, preset: PresetId) -> Result<Vec<ChatDelta>, ProviderError> {
        std::mem::take(&mut self.calls)
            .into_values()
            .map(|call| {
                if call.id.is_empty() || call.name.is_empty() {
                    return Err(protocol_error(preset));
                }
                let raw = if call.arguments.trim().is_empty() {
                    "{}"
                } else {
                    &call.arguments
                };
                let args =
                    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
                Ok(ChatDelta::Call(ToolCall {
                    id: call.id,
                    name: call.name,
                    args,
                }))
            })
            .collect()
    }
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolDelta>,
}

#[derive(Deserialize)]
struct ToolDelta {
    #[serde(default)]
    index: usize,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::StreamExt;
    use pinna_http::{ClientKind, client, reqwest::Method};
    use pinna_mcp::{ToolOutput, ToolSpec};
    use serde_json::json;

    use super::*;
    use crate::{
        error::{Reason, Slot},
        testing::{FakeResponse, FakeServer},
    };

    fn adapter(server: &FakeServer) -> OpenAiChat {
        adapter_for(server, PresetId::OpenRouter)
    }

    fn adapter_for(server: &FakeServer, preset: PresetId) -> OpenAiChat {
        OpenAiChat::new(
            client(ClientKind::Provider { read: None }),
            server.url("/v1"),
            "test-model",
            Some(Secret::new("sk-test")),
            preset,
            || true,
        )
    }

    async fn collect(
        chat: &OpenAiChat,
        request: ChatRequest<'_>,
    ) -> Vec<Result<ChatDelta, ProviderError>> {
        chat.stream(request).await.unwrap().collect().await
    }

    fn event(value: Value) -> String {
        format!("data: {value}\n\n")
    }

    #[tokio::test]
    async fn utf8_can_split_mid_character_and_reasoning_is_dropped() {
        let server = FakeServer::start().await;
        let fixture = format!(
            "{}data: [DONE]\n\n",
            event(json!({"choices": [{"delta": {
                "content": "héllo 👋",
                "reasoning": "not spoken",
                "reasoning_content": "not spoken either"
            }}]}))
        );
        let split = fixture.find('é').unwrap() + 1;
        server.script(
            Method::POST,
            "/v1/chat/completions",
            FakeResponse::sse(
                [
                    Bytes::copy_from_slice(&fixture.as_bytes()[..split]),
                    Bytes::copy_from_slice(&fixture.as_bytes()[split..]),
                ],
                Duration::ZERO,
            ),
        );

        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Be brief.",
                messages: &[],
                tools: &[],
                calls_allowed: true,
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(
            deltas,
            vec![Ok(ChatDelta::Text("héllo 👋".into())), Ok(ChatDelta::End),]
        );
    }

    #[tokio::test]
    async fn parallel_tool_calls_are_assembled_and_usage_comes_from_the_final_chunk() {
        let server = FakeServer::start().await;
        let chunks = [
            event(json!({"choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "call-weather",
                "type": "function",
                "function": {"name": "weather", "arguments": "{\"city\":"}
            }]}}]})),
            event(json!({"choices": [{"delta": {"tool_calls": [{
                "index": 1,
                "id": "call-time",
                "type": "function",
                "function": {"name": "time", "arguments": "{\"zone\":"}
            }]}}]})),
            event(json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "\"Paris\"}"}},
                {"index": 1, "function": {"arguments": "\"UTC\"}"}}
            ]}}]})),
            event(json!({
                "choices": [],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}
            })),
            "data: [DONE]\n\n".into(),
        ];
        server.script(
            Method::POST,
            "/v1/chat/completions",
            FakeResponse::sse(chunks, Duration::from_millis(1)),
        );

        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Be brief.",
                messages: &[],
                tools: &[],
                calls_allowed: true,
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(
            deltas,
            vec![
                Ok(ChatDelta::Call(ToolCall {
                    id: "call-weather".into(),
                    name: "weather".into(),
                    args: json!({"city": "Paris"}),
                })),
                Ok(ChatDelta::Call(ToolCall {
                    id: "call-time".into(),
                    name: "time".into(),
                    args: json!({"zone": "UTC"}),
                })),
                Ok(ChatDelta::Usage {
                    input: 11,
                    output: 7,
                }),
                Ok(ChatDelta::End),
            ]
        );
    }

    #[tokio::test]
    async fn a_cut_stream_is_a_protocol_error() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/chat/completions",
            FakeResponse::sse(
                [event(
                    json!({"choices": [{"delta": {"content": "partial"}}]}),
                )],
                Duration::ZERO,
            ),
        );

        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Be brief.",
                messages: &[],
                tools: &[],
                calls_allowed: true,
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(
            deltas,
            vec![
                Ok(ChatDelta::Text("partial".into())),
                Err(ProviderError::new(
                    Slot::Chat,
                    PresetId::OpenRouter,
                    Reason::Protocol,
                )),
            ]
        );
    }

    #[tokio::test]
    async fn an_endless_event_stops_at_the_stream_cap() {
        let server = FakeServer::start().await;
        // `data:` lines with no blank line between them never end an event;
        // 5 MiB of them must not pile up in memory.
        let line = format!("data: {}\n", "x".repeat(1023));
        let chunks: Vec<String> = (0..5 * 1024).map(|_| line.clone()).collect();
        server.script(
            Method::POST,
            "/v1/chat/completions",
            FakeResponse::sse(chunks, Duration::ZERO),
        );
        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Be brief.",
                messages: &[],
                tools: &[],
                calls_allowed: true,
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(
            deltas,
            vec![Err(ProviderError::new(
                Slot::Chat,
                PresetId::OpenRouter,
                Reason::TooLarge,
            ))]
        );
    }

    #[tokio::test]
    async fn the_request_sends_messages_tools_and_the_stable_user() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/chat/completions",
            FakeResponse::sse(["data: [DONE]\n\n"], Duration::ZERO),
        );
        let call = ToolCall {
            id: "call-1".into(),
            name: "weather".into(),
            args: json!({"city": "Paris"}),
        };
        let messages = [
            Message::User("What is it like?".into()),
            Message::Assistant {
                text: "I will check.".into(),
                calls: vec![call],
                signature: None,
            },
            Message::ToolResult {
                call_id: "call-1".into(),
                name: "weather".into(),
                output: ToolOutput::ok("Sunny"),
            },
        ];
        let tools = [ToolSpec {
            name: "weather".into(),
            description: "Current weather".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }];

        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Speak plainly.",
                messages: &messages,
                tools: &tools,
                calls_allowed: true,
                max_tokens: 500,
                user: Some("pinna-device"),
            },
        )
        .await;
        assert_eq!(deltas, vec![Ok(ChatDelta::End)]);

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 500);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["user"], "pinna-device");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "What is it like?");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["messages"][3]["tool_call_id"], "call-1");
        assert_eq!(body["messages"][3]["content"], "Sunny");
        assert_eq!(body["tools"][0]["function"]["name"], "weather");
        assert_eq!(
            body["tools"][0]["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[tokio::test]
    async fn openai_itself_gets_the_newer_token_limit() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/chat/completions",
            FakeResponse::sse(["data: [DONE]\n\n".to_owned()], Duration::ZERO),
        );
        collect(
            &adapter_for(&server, PresetId::OpenAi),
            ChatRequest {
                system: "Be brief.",
                messages: &[Message::User("Hi.".into())],
                tools: &[],
                calls_allowed: true,
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        let body: Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
        assert_eq!(body["max_completion_tokens"], 500);
        assert!(body.get("max_tokens").is_none());
    }

    #[tokio::test]
    async fn the_last_round_forbids_calls_but_keeps_the_tools() {
        let server = FakeServer::start().await;
        let tools = [ToolSpec {
            name: "weather".into(),
            description: "Weather".into(),
            input_schema: json!({"type": "object"}),
        }];
        let messages = [Message::User("Hi.".into())];
        let last = ChatRequest {
            system: "Be brief.",
            messages: &messages,
            tools: &tools,
            calls_allowed: false,
            max_tokens: 10,
            user: None,
        };
        let body = adapter(&server).request_json(&last);
        assert_eq!(body["tool_choice"], json!("none"));
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
        let open = ChatRequest {
            calls_allowed: true,
            ..last
        };
        assert!(
            adapter(&server)
                .request_json(&open)
                .get("tool_choice")
                .is_none()
        );
        let none = ChatRequest { tools: &[], ..last };
        let body = adapter(&server).request_json(&none);
        assert!(body.get("tools").is_none() && body.get("tool_choice").is_none());
    }
}
