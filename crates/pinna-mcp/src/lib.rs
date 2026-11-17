//! Shared MCP tool and JSON-RPC types for standalone mode (ADR 0017).

pub mod client;
pub mod jsonrpc;
pub mod server;
pub mod tool;

pub use tool::{MAX_TOOL_OUTPUT, ToolCall, ToolOutput, ToolSet, ToolSpec, namespaced};
