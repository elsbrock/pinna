//! Shared MCP tool and JSON-RPC types for standalone mode (ADR 0017).

pub mod client;
/// The MCP protocol versions both the server and the client speak, newest
/// first.
pub(crate) const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2025-03-26"];

pub mod jsonrpc;
pub mod server;
pub mod tool;

pub use tool::{
    MAX_TOOL_OUTPUT, ToolCall, ToolOutput, ToolSet, ToolSpec, in_namespace, namespaced,
};
