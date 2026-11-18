//! Tool vocabulary shared by callers and implementations (ADR 0017).

use futures_core::future::BoxFuture;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

pub const MAX_TOOL_OUTPUT: usize = 8 * 1024;

fn capped(mut text: String) -> String {
    text.truncate(text.floor_char_boundary(MAX_TOOL_OUTPUT));
    text
}

fn read_text<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    String::deserialize(de).map(capped)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    #[serde(deserialize_with = "read_text")]
    pub text: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(t: impl Into<String>) -> Self {
        Self {
            text: capped(t.into()),
            is_error: false,
        }
    }

    pub fn error(t: impl Into<String>) -> Self {
        Self {
            text: capped(t.into()),
            is_error: true,
        }
    }
}

pub trait ToolSet: Send + Sync {
    fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>>;
    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, ToolOutput>;
}

/// The name a model sees for a server's tool: `<server>_<tool>`, within the
/// alphabet every provider accepts for tool names (`[A-Za-z0-9_-]`, at most
/// 64 bytes). The server part is lowercase with its hyphens turned into
/// underscores, so the `<server>_` prefix reads one way and names the server.
/// `None` when the tool's name can't be offered.
pub fn namespaced(server: &str, tool: &str) -> Option<String> {
    let server_byte =
        |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-';
    let tool_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    if server.is_empty()
        || tool.is_empty()
        || server.len().saturating_add(tool.len()) >= 64
        || !server.bytes().all(server_byte)
        || !tool.bytes().all(tool_byte)
    {
        return None;
    }
    Some(format!("{}_{}", server.replace('-', "_"), tool))
}

/// Whether `name` is one of `server`'s tools: `<server>_<tool>`, with the
/// server's hyphens written as underscores, as [`namespaced`] names them.
pub fn in_namespace(server: &str, name: &str) -> bool {
    let (server, name) = (server.as_bytes(), name.as_bytes());
    !server.is_empty()
        && name.len() > server.len() + 1
        && name[server.len()] == b'_'
        && server
            .iter()
            .zip(name)
            .all(|(s, n)| if *s == b'-' { *n == b'_' } else { s == n })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_in_the_namespace_its_server_gave_it() {
        let named = namespaced("my-ha", "lights_on").unwrap();
        assert!(in_namespace("my-ha", &named));
        assert!(in_namespace("home", "home_lab_reboot"));
        assert!(!in_namespace("home", "home_"));
        assert!(!in_namespace("home", "homework"));
        assert!(!in_namespace("", "_x"));
        assert!(!in_namespace("my-ha", "my-ha_lights"));
    }
    use serde_json::json;

    #[test]
    fn tool_values_round_trip() {
        let spec =
            json!({"name":"time", "description":"Local time", "inputSchema":{"type":"object"}});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<ToolSpec>(spec.clone()).unwrap())
                .unwrap(),
            spec
        );
        let call = ToolCall {
            id: "1".into(),
            name: "time".into(),
            args: json!({"zone":"UTC"}),
        };
        assert_eq!(
            serde_json::from_value::<ToolCall>(serde_json::to_value(&call).unwrap()).unwrap(),
            call
        );
        for output in [ToolOutput::ok("noon"), ToolOutput::error("Unknown zone.")] {
            assert_eq!(
                serde_json::from_value::<ToolOutput>(serde_json::to_value(&output).unwrap())
                    .unwrap(),
                output
            );
        }
    }

    #[test]
    fn output_limit_counts_bytes_without_splitting_characters() {
        let prefix = "a".repeat(MAX_TOOL_OUTPUT - 1);
        let long = format!("{prefix}é");
        assert_eq!(ToolOutput::ok(&long).text, prefix);
        assert_eq!(ToolOutput::error(&long).text, prefix);
        let output: ToolOutput =
            serde_json::from_value(json!({"text": long, "isError": true})).unwrap();
        assert_eq!(output.text, prefix);
        assert_eq!(
            ToolOutput::ok(format!("{prefix}!")).text.len(),
            MAX_TOOL_OUTPUT
        );
    }

    #[test]
    fn names_must_fit_the_model_alphabet_and_length() {
        assert_eq!(
            namespaced("home-2", "set_timer").as_deref(),
            Some("home_2_set_timer")
        );
        // Names as real servers spell them: Home Assistant's intents, Context7.
        assert_eq!(
            namespaced("home", "HassTurnOn").as_deref(),
            Some("home_HassTurnOn")
        );
        assert_eq!(
            namespaced("ctx", "resolve-library-id").as_deref(),
            Some("ctx_resolve-library-id")
        );
        assert_eq!(namespaced("a", &"x".repeat(62)).unwrap().len(), 64);
        assert!(namespaced("a", &"x".repeat(63)).is_none());
        for (server, tool) in [("", "t"), ("s", ""), ("S", "t"), ("s", "a.b"), ("s", "é")] {
            assert!(namespaced(server, tool).is_none());
        }
    }
}
