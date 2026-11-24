//! Shared MCP tool and JSON-RPC types for standalone mode (ADR 0017).

pub mod client;
pub mod credential;
/// The MCP protocol versions both the server and the client speak, newest
/// first.
pub(crate) const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2025-03-26"];

pub mod jsonrpc;
pub mod server;
pub mod tool;

pub use credential::{CredentialError, CredentialKind, McpCredential};
pub use tool::{
    MAX_TOOL_OUTPUT, TOOL_NAME_MAX, ToolCall, ToolOutput, ToolSet, ToolSpec, in_namespace,
    namespaced, tool_name,
};
