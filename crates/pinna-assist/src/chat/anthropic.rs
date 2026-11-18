//! Anthropic's streaming Messages API.

use std::collections::BTreeMap;

use futures_core::{future::BoxFuture, stream::BoxStream};
use pinna_http::{Secret, SseEvent, reqwest};
use pinna_mcp::{ToolCall, ToolOutput};
use serde_json::{Value, json};

use super::{Chat, ChatDelta, ChatRequest, Message, endpoint, protocol_error, response_stream};
use crate::{
    error::{ProviderError, Slot},
    http::{Auth, send},
    presets::PresetId,
};

/// The Messages API version every Anthropic request names.
pub(crate) const API_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct AnthropicChat {
    client: reqwest::Client,
    base_url: String,
    model: String,
    auth: Auth,
    clock_synced: fn() -> bool,
}

impl AnthropicChat {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        model: impl Into<String>,
        key: Secret,
        clock_synced: fn() -> bool,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            model: model.into(),
            auth: Auth::ApiKey("x-api-key", key),
            clock_synced,
        }
    }

    fn request_json(&self, req: &ChatRequest<'_>) -> Value {
        let mut messages = Vec::new();
        let mut tool_results = Vec::new();
        for message in req.messages {
            match message {
                Message::ToolResult {
                    call_id, output, ..
                } => tool_results.push(tool_result(call_id, output)),
                Message::User(text) => {
                    flush_tool_results(&mut messages, &mut tool_results);
                    messages.push(json!({
                        "role": "user",
                        "content": [{"type": "text", "text": text}],
                    }));
                }
                Message::Assistant { text, calls, .. } => {
                    flush_tool_results(&mut messages, &mut tool_results);
                    let mut content = Vec::new();
                    if !text.is_empty() {
                        content.push(json!({"type": "text", "text": text}));
                    }
                    content.extend(calls.iter().map(|call| {
                        json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.args,
                        })
                    }));
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
        }
        flush_tool_results(&mut messages, &mut tool_results);

        let mut body = json!({
            "model": self.model,
            "system": req.system,
            "messages": messages,
            "tools": req.tools.iter().map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })).collect::<Vec<_>>(),
            "max_tokens": req.max_tokens,
            "stream": true,
        });
        if let Some(user) = req.user {
            body["metadata"] = json!({"user_id": user});
        }
        body
    }
}

fn tool_result(call_id: &str, output: &ToolOutput) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": output.text,
        "is_error": output.is_error,
    })
}

fn flush_tool_results(messages: &mut Vec<Value>, results: &mut Vec<Value>) {
    if !results.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": std::mem::take(results),
        }));
    }
}

impl Chat for AnthropicChat {
    fn stream<'a>(
        &'a self,
        req: ChatRequest<'a>,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<ChatDelta, ProviderError>>, ProviderError>> {
        Box::pin(async move {
            let response = send(
                self.client
                    .post(endpoint(&self.base_url, "/v1/messages"))
                    .header("anthropic-version", API_VERSION)
                    .json(&self.request_json(&req)),
                &self.auth,
                Slot::Chat,
                PresetId::Anthropic,
                self.clock_synced,
            )
            .await?;
            let mut decoder = Decoder::default();
            Ok(response_stream(
                response,
                PresetId::Anthropic,
                self.clock_synced,
                move |event| decoder.parse(event),
            ))
        })
    }
}

#[derive(Default)]
struct Decoder {
    tools: BTreeMap<usize, PendingTool>,
    input: u64,
    output: u64,
    usage_seen: bool,
}

impl Decoder {
    fn parse(&mut self, event: Option<SseEvent>) -> Result<Vec<ChatDelta>, ProviderError> {
        let Some(event) = event else {
            return Err(protocol_error(PresetId::Anthropic));
        };
        let value: Value =
            serde_json::from_str(&event.data).map_err(|_| protocol_error(PresetId::Anthropic))?;
        let kind = event
            .event
            .as_deref()
            .or_else(|| value["type"].as_str())
            .unwrap_or("");
        match kind {
            "message_start" => {
                self.record_usage(&value["message"]["usage"]);
                Ok(Vec::new())
            }
            "content_block_start" => self.start_block(&value),
            "content_block_delta" => self.delta(&value),
            "content_block_stop" => {
                let index = index(&value)?;
                Ok(self
                    .tools
                    .remove(&index)
                    .map(|tool| ChatDelta::Call(finish_tool(tool)))
                    .into_iter()
                    .collect())
            }
            "message_delta" => {
                self.record_usage(&value["usage"]);
                Ok(Vec::new())
            }
            "message_stop" => {
                let mut deltas = std::mem::take(&mut self.tools)
                    .into_values()
                    .map(|tool| ChatDelta::Call(finish_tool(tool)))
                    .collect::<Vec<_>>();
                if self.usage_seen {
                    deltas.push(ChatDelta::Usage {
                        input: self.input,
                        output: self.output,
                    });
                }
                deltas.push(ChatDelta::End);
                Ok(deltas)
            }
            "error" => Err(protocol_error(PresetId::Anthropic)),
            // Pings, signatures and new non-content events carry nothing to say.
            _ => Ok(Vec::new()),
        }
    }

    fn start_block(&mut self, value: &Value) -> Result<Vec<ChatDelta>, ProviderError> {
        let block = &value["content_block"];
        match block["type"].as_str() {
            Some("text") => Ok(block["text"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(|text| vec![ChatDelta::Text(text.to_owned())])
                .unwrap_or_default()),
            Some("tool_use") => {
                let id = required_string(block, "id")?;
                let name = required_string(block, "name")?;
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                let arguments = if input == json!({}) {
                    String::new()
                } else {
                    input.to_string()
                };
                self.tools.insert(
                    index(value)?,
                    PendingTool {
                        id,
                        name,
                        arguments,
                    },
                );
                Ok(Vec::new())
            }
            // Thinking and redacted thinking are never part of the spoken reply.
            _ => Ok(Vec::new()),
        }
    }

    fn delta(&mut self, value: &Value) -> Result<Vec<ChatDelta>, ProviderError> {
        let delta = &value["delta"];
        match delta["type"].as_str() {
            Some("text_delta") => {
                let text = required_string(delta, "text")?;
                Ok(if text.is_empty() {
                    Vec::new()
                } else {
                    vec![ChatDelta::Text(text)]
                })
            }
            Some("input_json_delta") => {
                let fragment = required_string(delta, "partial_json")?;
                self.tools
                    .get_mut(&index(value)?)
                    .ok_or_else(|| protocol_error(PresetId::Anthropic))?
                    .arguments
                    .push_str(&fragment);
                Ok(Vec::new())
            }
            // Thinking deltas and signatures are deliberately dropped.
            _ => Ok(Vec::new()),
        }
    }

    fn record_usage(&mut self, usage: &Value) {
        if let Some(input) = usage["input_tokens"].as_u64() {
            self.input = self.input.max(input);
            self.usage_seen = true;
        }
        if let Some(output) = usage["output_tokens"].as_u64() {
            // Anthropic reports a cumulative output count in each message delta.
            self.output = self.output.max(output);
            self.usage_seen = true;
        }
    }
}

struct PendingTool {
    id: String,
    name: String,
    arguments: String,
}

fn finish_tool(tool: PendingTool) -> ToolCall {
    let raw = if tool.arguments.trim().is_empty() {
        "{}"
    } else {
        &tool.arguments
    };
    let args = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
    ToolCall {
        id: tool.id,
        name: tool.name,
        args,
    }
}

fn index(value: &Value) -> Result<usize, ProviderError> {
    value["index"]
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| protocol_error(PresetId::Anthropic))
}

fn required_string(value: &Value, field: &str) -> Result<String, ProviderError> {
    value[field]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| protocol_error(PresetId::Anthropic))
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;
    use pinna_http::{ClientKind, client, reqwest::Method};
    use pinna_mcp::ToolSpec;

    use super::*;
    use crate::testing::{FakeResponse, FakeServer};

    fn adapter(server: &FakeServer) -> AnthropicChat {
        AnthropicChat::new(
            client(ClientKind::Provider { read: None }),
            server.url(""),
            "claude-test",
            Secret::new("anthropic-test-key"),
            || true,
        )
    }

    async fn collect(
        chat: &AnthropicChat,
        request: ChatRequest<'_>,
    ) -> Vec<Result<ChatDelta, ProviderError>> {
        chat.stream(request).await.unwrap().collect().await
    }

    fn event(kind: &str, value: Value) -> String {
        format!("event: {kind}\ndata: {value}\n\n")
    }

    #[tokio::test]
    async fn text_is_streamed_thinking_is_dropped_and_usage_is_summed() {
        let server = FakeServer::start().await;
        let chunks = [
            event(
                "message_start",
                json!({"type": "message_start", "message": {"usage": {
                    "input_tokens": 17,
                    "output_tokens": 1
                }}}),
            ),
            event(
                "content_block_start",
                json!({"index": 0, "content_block": {"type": "thinking", "thinking": "hidden"}}),
            ),
            event(
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "thinking_delta", "thinking": "still hidden"}}),
            ),
            event("content_block_stop", json!({"index": 0})),
            event(
                "content_block_start",
                json!({"index": 1, "content_block": {"type": "text", "text": ""}}),
            ),
            event(
                "content_block_delta",
                json!({"index": 1, "delta": {"type": "text_delta", "text": "It is sunny."}}),
            ),
            event("content_block_stop", json!({"index": 1})),
            event(
                "message_delta",
                json!({"delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
            ),
            event("message_stop", json!({"type": "message_stop"})),
        ];
        server.script(
            Method::POST,
            "/v1/messages",
            FakeResponse::sse(chunks, Duration::from_millis(1)),
        );

        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Be brief.",
                messages: &[],
                tools: &[],
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(
            deltas,
            vec![
                Ok(ChatDelta::Text("It is sunny.".into())),
                Ok(ChatDelta::Usage {
                    input: 17,
                    output: 5,
                }),
                Ok(ChatDelta::End),
            ]
        );
    }

    #[tokio::test]
    async fn a_tool_call_assembles_input_json_deltas() {
        let server = FakeServer::start().await;
        let chunks = [
            event(
                "content_block_start",
                json!({"index": 2, "content_block": {
                    "type": "tool_use",
                    "id": "toolu-1",
                    "name": "weather",
                    "input": {}
                }}),
            ),
            event(
                "content_block_delta",
                json!({"index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"}}),
            ),
            event(
                "content_block_delta",
                json!({"index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"Paris\"}"}}),
            ),
            event("content_block_stop", json!({"index": 2})),
            event("message_stop", json!({})),
        ];
        server.script(
            Method::POST,
            "/v1/messages",
            FakeResponse::sse(chunks, Duration::ZERO),
        );

        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Be brief.",
                messages: &[],
                tools: &[],
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(
            deltas,
            vec![
                Ok(ChatDelta::Call(ToolCall {
                    id: "toolu-1".into(),
                    name: "weather".into(),
                    args: json!({"city": "Paris"}),
                })),
                Ok(ChatDelta::End),
            ]
        );
    }

    #[tokio::test]
    async fn tool_results_round_trip_in_one_user_message() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1/messages",
            FakeResponse::sse([event("message_stop", json!({}))], Duration::ZERO),
        );
        let calls = vec![
            ToolCall {
                id: "toolu-1".into(),
                name: "weather".into(),
                args: json!({"city": "Paris"}),
            },
            ToolCall {
                id: "toolu-2".into(),
                name: "time".into(),
                args: json!({"zone": "UTC"}),
            },
        ];
        let messages = [
            Message::User("Check both.".into()),
            Message::Assistant {
                text: "I will check.".into(),
                calls,
                signature: None,
            },
            Message::ToolResult {
                call_id: "toolu-1".into(),
                name: "weather".into(),
                output: ToolOutput::ok("Sunny"),
            },
            Message::ToolResult {
                call_id: "toolu-2".into(),
                name: "time".into(),
                output: ToolOutput::error("Unknown zone"),
            },
        ];
        let tools = [ToolSpec {
            name: "weather".into(),
            description: "Current weather".into(),
            input_schema: json!({"type": "object"}),
        }];

        let deltas = collect(
            &adapter(&server),
            ChatRequest {
                system: "Speak plainly.",
                messages: &messages,
                tools: &tools,
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(deltas, vec![Ok(ChatDelta::End)]);

        let requests = server.requests();
        let request = &requests[0];
        assert_eq!(request.header("x-api-key"), Some("anthropic-test-key"));
        assert_eq!(request.header("anthropic-version"), Some(API_VERSION));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["system"], "Speak plainly.");
        assert_eq!(body["tools"][0]["input_schema"], json!({"type": "object"}));
        assert_eq!(body["messages"][1]["content"][0]["type"], "text");
        assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(
            body["messages"][1]["content"][1]["input"],
            json!({"city": "Paris"})
        );
        assert_eq!(body["messages"][2]["role"], "user");
        let results = body["messages"][2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "toolu-1");
        assert_eq!(results[0]["content"], "Sunny");
        assert_eq!(results[0]["is_error"], false);
        assert_eq!(results[1]["tool_use_id"], "toolu-2");
        assert_eq!(results[1]["content"], "Unknown zone");
        assert_eq!(results[1]["is_error"], true);
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    }
}
