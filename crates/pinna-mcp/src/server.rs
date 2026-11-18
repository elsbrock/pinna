//! Sans-IO dispatcher for Pinna's MCP server (ADR 0017 §"Pinna as an MCP server").
//!
//! It turns one HTTP body into one reply body. The transport (pinnad's `/mcp`
//! route) owns the Host, `Content-Type` and Origin checks; this module only
//! speaks JSON-RPC.

use serde_json::{Value, json};

use crate::jsonrpc::{Error, Id, Incoming, Payload, Request, Response, Version, parse};
use crate::tool::{ToolOutput, ToolSet};

/// A client name is for the dashboard; it never becomes a metric label.
const CLIENT_NAME_MAX: usize = 40;

/// Name and version reported by `initialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// One reply body, plus the MCP client that sent the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handled {
    /// `None` for a notification: the transport answers 202 with no body.
    pub reply: Option<Vec<u8>>,
    pub client_name: Option<String>,
}

/// Answer one MCP request. `body` is the raw request bytes.
pub async fn handle(tools: &dyn ToolSet, info: &ServerInfo, body: &[u8]) -> Handled {
    let reply = match parse(body) {
        Ok(Incoming::Single(req)) => return single(tools, info, req).await,
        Err(e) => Some(encode(Payload::Failure { error: e }, Id::Null)),
        Ok(Incoming::Batch(_)) => Some(error(Id::Null, -32600, "Batch requests are not supported")),
    };
    Handled {
        reply,
        client_name: None,
    }
}

/// A request with an id gets a reply; a notification gets none. Either way an
/// `initialize` reports the client name to the caller.
async fn single(tools: &dyn ToolSet, info: &ServerInfo, req: Request) -> Handled {
    let client_name = if req.method == "initialize" {
        client_name(&req)
    } else {
        None
    };
    let Some(id) = req.id.clone() else {
        return Handled {
            reply: None,
            client_name,
        };
    };
    Handled {
        reply: Some(encode(dispatch(tools, info, &req).await, id)),
        client_name,
    }
}

async fn dispatch(tools: &dyn ToolSet, info: &ServerInfo, req: &Request) -> Payload {
    match req.method.as_str() {
        "initialize" => Payload::Success {
            result: initialize(info, req),
        },
        "ping" => Payload::Success { result: json!({}) },
        "tools/list" => Payload::Success {
            result: json!({ "tools": tools.specs().await }),
        },
        "tools/call" => call(tools, req).await,
        _ => failure(-32601, "Method not found"),
    }
}

fn initialize(info: &ServerInfo, req: &Request) -> Value {
    // Echo a version we speak; otherwise answer with the newest one.
    let version = req
        .params
        .as_ref()
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .filter(|v| crate::PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(crate::PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": info.name, "version": info.version },
    })
}

async fn call(tools: &dyn ToolSet, req: &Request) -> Payload {
    let Some(name) = req
        .params
        .as_ref()
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
    else {
        return failure(-32602, "Invalid params");
    };
    let args = req
        .params
        .as_ref()
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let out = tools.call(name, args).await;
    Payload::Success {
        result: tool_result(&out),
    }
}

fn tool_result(out: &ToolOutput) -> Value {
    json!({
        "content": [{ "type": "text", "text": out.text }],
        "isError": out.is_error,
    })
}

fn client_name(req: &Request) -> Option<String> {
    let raw = req
        .params
        .as_ref()?
        .get("clientInfo")?
        .get("name")?
        .as_str()?;
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(CLIENT_NAME_MAX)
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn failure(code: i32, message: &str) -> Payload {
    Payload::Failure {
        error: Error {
            code,
            message: message.into(),
            data: None,
        },
    }
}

fn error(id: Id, code: i32, message: &str) -> Vec<u8> {
    encode(failure(code, message), id)
}

fn encode(payload: Payload, id: Id) -> Vec<u8> {
    serde_json::to_vec(&Response {
        jsonrpc: Version::V2,
        id,
        payload,
    })
    .expect("a JSON-RPC response always serializes")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_core::future::BoxFuture;

    use super::*;
    use crate::tool::ToolSpec;

    struct Fake {
        specs: Vec<ToolSpec>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                specs: vec![
                    ToolSpec {
                        name: "time".into(),
                        description: "Local time".into(),
                        input_schema: json!({"type": "object"}),
                    },
                    ToolSpec {
                        name: "set_timer".into(),
                        description: "Start a timer".into(),
                        input_schema: json!({"type": "object"}),
                    },
                ],
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ToolSet for Fake {
        fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
            Box::pin(async move { self.specs.clone() })
        }

        fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async move {
                self.calls.lock().unwrap().push((name.to_string(), args));
                match name {
                    "time" => ToolOutput::ok("noon"),
                    _ => ToolOutput::error(format!("Unknown tool {name}.")),
                }
            })
        }
    }

    fn info() -> ServerInfo {
        ServerInfo {
            name: "pinna".into(),
            version: "0.1.0".into(),
        }
    }

    fn request(method: &str, id: Value, params: Value) -> Vec<u8> {
        let mut wire = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if !id.is_null() {
            wire["id"] = id;
        }
        serde_json::to_vec(&wire).unwrap()
    }

    fn decoded(reply: Option<Vec<u8>>) -> Value {
        serde_json::from_slice(&reply.expect("expected a reply body")).unwrap()
    }

    #[tokio::test]
    async fn initialize_echoes_a_supported_version_and_names_the_server() {
        let tools = Fake::new();
        for (asked, expected) in [
            ("2025-06-18", "2025-06-18"),
            ("2025-03-26", "2025-03-26"),
            ("1999-01-01", "2025-06-18"),
        ] {
            let body = request(
                "initialize",
                json!(1),
                json!({"protocolVersion": asked, "clientInfo": {"name": "helper", "version": "2"}}),
            );
            let out = handle(&tools, &info(), &body).await;
            let reply = decoded(out.reply);
            assert_eq!(reply["id"], json!(1));
            assert_eq!(reply["result"]["protocolVersion"], json!(expected));
            assert_eq!(reply["result"]["capabilities"], json!({"tools": {}}));
            assert_eq!(
                reply["result"]["serverInfo"],
                json!({"name": "pinna", "version": "0.1.0"})
            );
            assert_eq!(out.client_name.as_deref(), Some("helper"));
        }
    }

    #[tokio::test]
    async fn client_name_is_capped_and_stripped_of_control_characters() {
        let tools = Fake::new();
        let long = format!("a\u{7}b{}", "c".repeat(60));
        let body = request(
            "initialize",
            json!(1),
            json!({"protocolVersion": "2025-06-18", "clientInfo": {"name": long}}),
        );
        let out = handle(&tools, &info(), &body).await;
        let name = out.client_name.unwrap();
        assert_eq!(name, format!("ab{}", "c".repeat(38)));
        assert!(name.chars().count() <= CLIENT_NAME_MAX);

        let blank = request(
            "initialize",
            json!(1),
            json!({"protocolVersion": "2025-06-18", "clientInfo": {"name": "  "}}),
        );
        assert_eq!(handle(&tools, &info(), &blank).await.client_name, None);
    }

    #[tokio::test]
    async fn ping_answers_with_an_empty_result() {
        let tools = Fake::new();
        let out = handle(&tools, &info(), &request("ping", json!("p"), json!({}))).await;
        assert_eq!(
            decoded(out.reply),
            json!({"jsonrpc":"2.0","id":"p","result":{}})
        );
    }

    #[tokio::test]
    async fn tools_list_returns_every_spec() {
        let tools = Fake::new();
        let out = handle(&tools, &info(), &request("tools/list", json!(2), json!({}))).await;
        let reply = decoded(out.reply);
        assert_eq!(
            reply["result"]["tools"],
            json!([
                {"name":"time","description":"Local time","inputSchema":{"type":"object"}},
                {"name":"set_timer","description":"Start a timer","inputSchema":{"type":"object"}},
            ])
        );
    }

    #[tokio::test]
    async fn tools_call_forwards_arguments_and_reports_the_result() {
        let tools = Fake::new();
        let args = json!({"seconds": 30});
        let body = request(
            "tools/call",
            json!(3),
            json!({"name": "time", "arguments": args}),
        );
        let reply = decoded(handle(&tools, &info(), &body).await.reply);
        assert_eq!(reply["id"], json!(3));
        assert_eq!(
            reply["result"],
            json!({"content":[{"type":"text","text":"noon"}],"isError":false})
        );
        assert_eq!(tools.calls(), vec![("time".to_string(), args)]);

        // Missing arguments reach the tool as an empty object.
        let missing = request("tools/call", json!(4), json!({"name": "time"}));
        let reply = decoded(handle(&tools, &info(), &missing).await.reply);
        assert_eq!(reply["result"]["isError"], json!(false));
        assert_eq!(tools.calls().last().unwrap().1, json!({}));
    }

    #[tokio::test]
    async fn a_failing_tool_is_an_error_result_not_a_protocol_error() {
        let tools = Fake::new();
        let body = request(
            "tools/call",
            json!(5),
            json!({"name": "nope", "arguments": {}}),
        );
        let reply = decoded(handle(&tools, &info(), &body).await.reply);
        assert_eq!(reply["result"]["isError"], json!(true));
        assert_eq!(reply["result"]["content"][0]["text"], "Unknown tool nope.");
    }

    #[tokio::test]
    async fn notifications_get_no_reply() {
        let tools = Fake::new();
        for method in ["notifications/initialized", "notifications/cancelled"] {
            let out = handle(&tools, &info(), &request(method, Value::Null, json!({}))).await;
            assert_eq!(out.reply, None, "{method}");
        }
        // An unknown notification is silent too.
        let out = handle(&tools, &info(), &request("nope", Value::Null, json!({}))).await;
        assert_eq!(out.reply, None);
    }

    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let tools = Fake::new();
        let reply = decoded(
            handle(
                &tools,
                &info(),
                &request("resources/list", json!(6), json!({})),
            )
            .await
            .reply,
        );
        assert_eq!(reply["error"]["code"], json!(-32601));
        assert_eq!(reply["error"]["message"], "Method not found");
    }

    #[tokio::test]
    async fn invalid_call_params_are_rejected() {
        let tools = Fake::new();
        for params in [json!({}), json!([1, 2]), json!({"name": 7})] {
            let reply = decoded(
                handle(&tools, &info(), &request("tools/call", json!(7), params))
                    .await
                    .reply,
            );
            assert_eq!(reply["error"]["code"], json!(-32602));
        }
        assert!(tools.calls().is_empty());
    }

    #[tokio::test]
    async fn batches_are_refused() {
        let tools = Fake::new();
        let body = br#"[{"jsonrpc":"2.0","method":"ping","id":1}]"#;
        let out = handle(&tools, &info(), body).await;
        assert_eq!(out.client_name, None);
        let reply = decoded(out.reply);
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["error"]["code"], json!(-32600));
    }

    #[tokio::test]
    async fn a_malformed_body_is_a_parse_error() {
        let tools = Fake::new();
        let reply = decoded(handle(&tools, &info(), b"{").await.reply);
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["error"]["code"], json!(-32700));
    }
}
