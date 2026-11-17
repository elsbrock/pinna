//! Built-in tools for the brain and `/mcp` (ADR 0017, architecture §8).

// The tool registry (T61) offers these; until then only tests call them.
#![cfg_attr(not(test), allow(dead_code))]

use std::time::SystemTime;

use pinna_mcp::{ToolOutput, ToolSpec};
use serde_json::json;

use crate::clock;

pub mod weather;

pub const TIME: &str = "time";

pub fn time_spec() -> ToolSpec {
    ToolSpec {
        name: TIME.into(),
        description: "The current local date and time where the satellite is.".into(),
        input_schema: json!({"type": "object", "properties": {}}),
    }
}

/// The `time` tool. `zone` is the time zone setting; unset is UTC.
pub fn time(zone: Option<&str>, now: SystemTime) -> ToolOutput {
    let (time, zone) = clock::local_now(zone, now);
    ToolOutput::ok(format!("{time} ({zone})"))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn the_time_tool_names_the_zone() {
        // 2026-11-17 14:03:00 UTC.
        let now = UNIX_EPOCH + Duration::from_secs(1_794_924_180);
        assert_eq!(
            time(None, now),
            ToolOutput::ok("Tuesday 17 November 2026, 14:03 (UTC)")
        );
        assert_eq!(time_spec().name, TIME);
    }
}
