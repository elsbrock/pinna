//! Gemini's streaming GenerateContent API.

use futures_core::{future::BoxFuture, stream::BoxStream};
use pinna_http::{Secret, SseEvent, reqwest};
use pinna_mcp::{ToolCall, ToolOutput};
use serde_json::{Map, Value, json};
use url::Url;

use super::{Chat, ChatDelta, ChatRequest, Message, protocol_error, response_stream};
use crate::{
    error::{ProviderError, Slot},
    http::{Auth, send},
    presets::PresetId,
};

#[derive(Clone)]
pub struct GeminiChat {
    client: reqwest::Client,
    base_url: String,
    model: String,
    auth: Auth,
    clock_synced: fn() -> bool,
}

impl GeminiChat {
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
            auth: Auth::ApiKey("x-goog-api-key", key),
            clock_synced,
        }
    }

    fn endpoint(&self) -> Result<Url, ProviderError> {
        let mut url = Url::parse(self.base_url.trim_end_matches('/'))
            .map_err(|_| protocol_error(PresetId::Gemini))?;
        url.set_query(None);
        url.set_fragment(None);
        let mut model_parts = self
            .model
            .split('/')
            .filter(|part| !part.is_empty())
            .peekable();
        if model_parts.peek().is_none() {
            return Err(protocol_error(PresetId::Gemini));
        }
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| protocol_error(PresetId::Gemini))?;
            path.pop_if_empty().push("v1beta").push("models");
            while let Some(part) = model_parts.next() {
                if model_parts.peek().is_none() {
                    path.push(&format!("{part}:streamGenerateContent"));
                } else {
                    path.push(part);
                }
            }
        }
        url.query_pairs_mut().append_pair("alt", "sse");
        Ok(url)
    }

    fn request_json(&self, req: &ChatRequest<'_>) -> Value {
        let mut contents = Vec::new();
        let mut function_responses = Vec::new();
        for message in req.messages {
            match message {
                Message::ToolResult {
                    call_id,
                    name,
                    output,
                } => function_responses.push(function_response(call_id, name, output)),
                Message::User(text) => {
                    flush_function_responses(&mut contents, &mut function_responses);
                    contents.push(json!({
                        "role": "user",
                        "parts": [{"text": text}],
                    }));
                }
                Message::Assistant {
                    text,
                    calls,
                    signature,
                } => {
                    flush_function_responses(&mut contents, &mut function_responses);
                    let mut parts = Vec::new();
                    if !text.is_empty() {
                        parts.push(json!({"text": text}));
                    }
                    parts.extend(calls.iter().enumerate().map(|(i, call)| {
                        let mut part = json!({"functionCall": {
                            "id": call.id,
                            "name": call.name,
                            "args": call.args,
                        }});
                        // Gemini wants its signature back on the round's
                        // first call, or refuses the request.
                        if i == 0
                            && let Some(signature) = signature
                        {
                            part["thoughtSignature"] = json!(signature);
                        }
                        part
                    }));
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
        }
        flush_function_responses(&mut contents, &mut function_responses);

        let mut body = json!({
            "systemInstruction": {"parts": [{"text": req.system}]},
            "contents": contents,
            "generationConfig": {"maxOutputTokens": req.max_tokens},
        });
        if !req.tools.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": req.tools.iter().map(|tool| json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": gemini_schema(&tool.input_schema),
                })).collect::<Vec<_>>(),
            }]);
        }
        body
    }
}

fn function_response(call_id: &str, name: &str, output: &ToolOutput) -> Value {
    let response = if output.is_error {
        json!({"error": output.text})
    } else {
        json!({"result": output.text})
    };
    json!({"functionResponse": {
        "id": call_id,
        "name": name,
        "response": response,
    }})
}

fn flush_function_responses(contents: &mut Vec<Value>, responses: &mut Vec<Value>) {
    if !responses.is_empty() {
        contents.push(json!({
            "role": "user",
            "parts": std::mem::take(responses),
        }));
    }
}

impl Chat for GeminiChat {
    fn stream<'a>(
        &'a self,
        req: ChatRequest<'a>,
    ) -> BoxFuture<'a, Result<BoxStream<'a, Result<ChatDelta, ProviderError>>, ProviderError>> {
        Box::pin(async move {
            let response = send(
                self.client
                    .post(self.endpoint()?)
                    .json(&self.request_json(&req)),
                &self.auth,
                Slot::Chat,
                PresetId::Gemini,
                self.clock_synced,
            )
            .await?;
            let mut decoder = Decoder::default();
            Ok(response_stream(
                response,
                PresetId::Gemini,
                self.clock_synced,
                move |event| decoder.parse(event),
            ))
        })
    }
}

#[derive(Default)]
struct Decoder {
    input: u64,
    output: u64,
    usage_seen: bool,
    saw_chunk: bool,
    generated_calls: u64,
}

impl Decoder {
    fn parse(&mut self, event: Option<SseEvent>) -> Result<Vec<ChatDelta>, ProviderError> {
        let Some(event) = event else {
            if !self.saw_chunk {
                return Err(protocol_error(PresetId::Gemini));
            }
            let mut deltas = Vec::new();
            if self.usage_seen {
                deltas.push(ChatDelta::Usage {
                    input: self.input,
                    output: self.output,
                });
            }
            deltas.push(ChatDelta::End);
            return Ok(deltas);
        };
        self.saw_chunk = true;
        let value: Value =
            serde_json::from_str(&event.data).map_err(|_| protocol_error(PresetId::Gemini))?;
        let mut deltas = Vec::new();
        if let Some(parts) = value["candidates"][0]["content"]["parts"].as_array() {
            for part in parts {
                if part["thought"].as_bool() == Some(true) {
                    continue;
                }
                if let Some(text) = part["text"].as_str()
                    && !text.is_empty()
                {
                    deltas.push(ChatDelta::Text(text.to_owned()));
                }
                if let Some(call) = part.get("functionCall") {
                    if let Some(signature) = part["thoughtSignature"].as_str() {
                        deltas.push(ChatDelta::Signature(signature.to_owned()));
                    }
                    let name = call["name"]
                        .as_str()
                        .ok_or_else(|| protocol_error(PresetId::Gemini))?
                        .to_owned();
                    let id = call["id"].as_str().map_or_else(
                        || {
                            self.generated_calls = self.generated_calls.saturating_add(1);
                            format!("gemini-call-{}", self.generated_calls)
                        },
                        str::to_owned,
                    );
                    deltas.push(ChatDelta::Call(ToolCall {
                        id,
                        name,
                        args: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    }));
                }
            }
        }
        self.record_usage(&value["usageMetadata"]);
        Ok(deltas)
    }

    fn record_usage(&mut self, usage: &Value) {
        if let Some(input) = usage["promptTokenCount"].as_u64() {
            self.input = self.input.max(input);
            self.usage_seen = true;
        }
        let candidates = usage["candidatesTokenCount"].as_u64();
        let thoughts = usage["thoughtsTokenCount"].as_u64();
        if candidates.is_some() || thoughts.is_some() {
            let output = candidates
                .unwrap_or(0)
                .saturating_add(thoughts.unwrap_or(0));
            self.output = self.output.max(output);
            self.usage_seen = true;
        }
    }
}

/// Keep only Gemini's OpenAPI subset and use its uppercase type names.
fn gemini_schema(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return Value::Null;
    };
    let mut reduced = Map::new();
    for (key, value) in object {
        match key.as_str() {
            "type" => {
                if let Some(kind) = value.as_str() {
                    reduced.insert(key.clone(), kind.to_uppercase().into());
                }
            }
            "format" | "description" | "nullable" | "enum" | "required" | "propertyOrdering"
            | "minItems" | "maxItems" => {
                reduced.insert(key.clone(), value.clone());
            }
            "items" => {
                reduced.insert(key.clone(), gemini_schema(value));
            }
            "properties" => {
                let properties = value
                    .as_object()
                    .map(|properties| {
                        properties
                            .iter()
                            .map(|(name, schema)| (name.clone(), gemini_schema(schema)))
                            .collect::<Map<_, _>>()
                    })
                    .unwrap_or_default();
                reduced.insert(key.clone(), Value::Object(properties));
            }
            "anyOf" => {
                let alternatives = value
                    .as_array()
                    .map(|alternatives| alternatives.iter().map(gemini_schema).collect())
                    .unwrap_or_default();
                reduced.insert(key.clone(), Value::Array(alternatives));
            }
            _ => {}
        }
    }
    Value::Object(reduced)
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;
    use pinna_http::{ClientKind, client, reqwest::Method};
    use pinna_mcp::ToolSpec;

    use super::*;
    use crate::testing::{FakeResponse, FakeServer};

    fn adapter(server: &FakeServer, model: &str) -> GeminiChat {
        GeminiChat::new(
            client(ClientKind::Provider { read: None }),
            server.url(""),
            model,
            Secret::new("gemini-secret-key"),
            || true,
        )
    }

    async fn collect(
        chat: &GeminiChat,
        request: ChatRequest<'_>,
    ) -> Vec<Result<ChatDelta, ProviderError>> {
        chat.stream(request).await.unwrap().collect().await
    }

    fn event(value: Value) -> String {
        format!("data: {value}\n\n")
    }

    #[tokio::test]
    async fn text_and_function_calls_stream_while_thought_parts_are_dropped() {
        let server = FakeServer::start().await;
        let chunks = [
            event(
                json!({"candidates": [{"content": {"role": "model", "parts": [
                    {"text": "hidden reasoning", "thought": true},
                    {"text": "I will check."},
                    {"functionCall": {
                        "id": "gemini-tool-1",
                        "name": "weather",
                        "args": {"city": "Paris"}
                    }, "thoughtSignature": "c2lnbmF0dXJl"}
                ]}}]}),
            ),
            event(json!({
                "candidates": [],
                "usageMetadata": {
                    "promptTokenCount": 13,
                    "candidatesTokenCount": 7,
                    "thoughtsTokenCount": 2,
                    "totalTokenCount": 22
                }
            })),
        ];
        server.script(
            Method::POST,
            "/v1beta/models/gemini-test:streamGenerateContent?alt=sse",
            FakeResponse::sse(chunks, Duration::from_millis(1)),
        );

        let deltas = collect(
            &adapter(&server, "gemini-test"),
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
                Ok(ChatDelta::Text("I will check.".into())),
                Ok(ChatDelta::Signature("c2lnbmF0dXJl".into())),
                Ok(ChatDelta::Call(ToolCall {
                    id: "gemini-tool-1".into(),
                    name: "weather".into(),
                    args: json!({"city": "Paris"}),
                })),
                Ok(ChatDelta::Usage {
                    input: 13,
                    output: 9,
                }),
                Ok(ChatDelta::End),
            ]
        );
    }

    #[tokio::test]
    async fn a_model_name_with_slashes_is_escaped_per_segment_and_the_key_is_a_header() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1beta/models/publishers/google/models/gemini%20test:streamGenerateContent?alt=sse",
            FakeResponse::sse([event(json!({"candidates": []}))], Duration::ZERO),
        );

        let deltas = collect(
            &adapter(&server, "publishers/google/models/gemini test"),
            ChatRequest {
                system: "Be brief.",
                messages: &[],
                tools: &[],
                max_tokens: 500,
                user: None,
            },
        )
        .await;
        assert_eq!(deltas, vec![Ok(ChatDelta::End)]);

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.header("x-goog-api-key"), Some("gemini-secret-key"));
        assert!(!request.path.contains("gemini-secret-key"));
        assert!(!request.path.contains("key="));
        assert!(request.path.ends_with("?alt=sse"));
    }

    #[tokio::test]
    async fn requests_reduce_tool_schemas_and_group_function_responses() {
        let server = FakeServer::start().await;
        server.script(
            Method::POST,
            "/v1beta/models/gemini-test:streamGenerateContent?alt=sse",
            FakeResponse::sse([event(json!({"candidates": []}))], Duration::ZERO),
        );
        let calls = vec![
            ToolCall {
                id: "call-1".into(),
                name: "weather".into(),
                args: json!({"city": "Paris"}),
            },
            ToolCall {
                id: "call-2".into(),
                name: "time".into(),
                args: json!({"zone": "UTC"}),
            },
        ];
        let messages = [
            Message::User("Check both.".into()),
            Message::Assistant {
                text: "I will check.".into(),
                calls,
                signature: Some("sig-1".into()),
            },
            Message::ToolResult {
                call_id: "call-1".into(),
                name: "weather".into(),
                output: ToolOutput::ok("Sunny"),
            },
            Message::ToolResult {
                call_id: "call-2".into(),
                name: "time".into(),
                output: ToolOutput::error("Unknown zone"),
            },
        ];
        let tools = [ToolSpec {
            name: "weather".into(),
            description: "Current weather".into(),
            input_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "city": {"type": "string", "description": "City", "default": "Paris"},
                    "days": {"type": "integer", "minimum": 1}
                },
                "required": ["city"]
            }),
        }];

        let deltas = collect(
            &adapter(&server, "gemini-test"),
            ChatRequest {
                system: "Speak plainly.",
                messages: &messages,
                tools: &tools,
                max_tokens: 500,
                user: Some("ignored-by-gemini"),
            },
        )
        .await;
        assert_eq!(deltas, vec![Ok(ChatDelta::End)]);

        let requests = server.requests();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "Speak plainly."
        );
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 500);
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(
            body["contents"][1]["parts"][1]["functionCall"]["args"],
            json!({"city": "Paris"})
        );
        // The signature goes back unchanged, on the round's first call only.
        assert_eq!(body["contents"][1]["parts"][1]["thoughtSignature"], "sig-1");
        assert!(
            body["contents"][1]["parts"][2]
                .get("thoughtSignature")
                .is_none()
        );
        let responses = body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0]["functionResponse"]["response"],
            json!({"result": "Sunny"})
        );
        assert_eq!(
            responses[1]["functionResponse"]["response"],
            json!({"error": "Unknown zone"})
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parameters"],
            json!({
                "type": "OBJECT",
                "properties": {
                    "city": {"type": "STRING", "description": "City"},
                    "days": {"type": "INTEGER"}
                },
                "required": ["city"]
            })
        );
    }
}
