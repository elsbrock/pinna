//! Provider failures, reduced to bounded metric labels and owner-safe text.

use std::fmt;

use crate::presets::PresetId;

/// The pipeline stage that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Stt,
    Chat,
    Tts,
    Tool,
}

impl Slot {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Chat => "brain",
            Self::Tts => "tts",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A provider failure without a URL, response body or transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Http(u16),
    Timeout,
    Unreachable,
    Tls,
    ClockNotSet,
    Protocol,
    TooLarge,
    Internal,
}

impl Reason {
    pub fn label(&self) -> &'static str {
        match *self {
            Self::Http(401) => "http_401",
            Self::Http(402) => "http_402",
            Self::Http(403) => "http_403",
            Self::Http(404) => "http_404",
            Self::Http(429) => "http_429",
            Self::Http(500..=599) => "http_5xx",
            Self::Http(_) => "http_4xx",
            Self::Timeout => "timeout",
            Self::Unreachable => "unreachable",
            Self::Tls => "tls",
            Self::ClockNotSet => "clock",
            Self::Protocol => "protocol",
            Self::TooLarge => "too_large",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(status) => write!(f, "HTTP {status}"),
            Self::Timeout => f.write_str("request timed out"),
            Self::Unreachable => f.write_str("service unreachable"),
            Self::Tls => f.write_str("TLS failed"),
            Self::ClockNotSet => f.write_str("clock not set"),
            Self::Protocol => f.write_str("invalid provider response"),
            Self::TooLarge => f.write_str("provider response too large"),
            Self::Internal => f.write_str("internal error"),
        }
    }
}

/// A failure at one provider slot. Its display never contains request data.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderError {
    pub slot: Slot,
    pub preset: PresetId,
    pub reason: Reason,
}

impl ProviderError {
    pub fn new(slot: Slot, preset: PresetId, reason: Reason) -> Self {
        Self {
            slot,
            preset,
            reason,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} via {}: {}",
            self.slot.label(),
            self.preset.label(),
            self.reason
        )
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_slot_preset_and_reason_only() {
        let error = ProviderError::new(Slot::Chat, PresetId::OpenRouter, Reason::Http(402));
        assert_eq!(error.to_string(), "brain via openrouter: HTTP 402");
        assert!(!error.to_string().contains("https://"));
        assert!(!error.to_string().contains("sk-secret"));
    }

    #[test]
    fn labels_stay_in_the_closed_sets() {
        assert_eq!(Slot::Chat.label(), "brain");
        assert_eq!(Reason::Http(401).label(), "http_401");
        assert_eq!(Reason::Http(418).label(), "http_4xx");
        assert_eq!(Reason::Http(503).label(), "http_5xx");
        assert_eq!(Reason::ClockNotSet.label(), "clock");
        assert_eq!(Reason::TooLarge.label(), "too_large");
    }
}
