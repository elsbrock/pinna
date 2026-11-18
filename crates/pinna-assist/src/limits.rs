//! Per-stage time limits and the Brain's backstops (ADR 0017 §"Each stage has
//! a time limit").
//!
//! A stage limit is what a request waits on the provider; a backstop is the
//! later wall-clock bound the Brain re-arms so a stalled stage still fails.

use std::time::Duration;

/// The limits of one run. The constants are fixed, not settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// The listening timings (nothing heard, the longest recording) are the
/// endpointer's: [`pinna_vad::EndpointConfig`].
pub struct RunLimits {
    /// Speech-to-text.
    pub stt: Duration,
    /// The brain's first text in one round.
    pub first_text: Duration,
    /// One tool call.
    pub tool: Duration,
    /// Tool rounds.
    pub rounds: u8,
    /// Text-to-speech, request to headers.
    pub tts_first_audio: Duration,
    /// An acknowledgment after this long without text; `None` for none.
    pub ack_after: Option<Duration>,
    pub max_tokens: u32,
    /// Spoken characters in a reply.
    pub spoken_chars: usize,
    /// Speaker backstop, including anything still queued.
    pub speaking: Duration,
}

impl RunLimits {
    /// A model the tool loop runs against.
    pub const MODEL: Self = Self {
        stt: Duration::from_secs(15),
        first_text: Duration::from_secs(30),
        tool: Duration::from_secs(10),
        rounds: 5,
        tts_first_audio: Duration::from_secs(10),
        ack_after: None,
        max_tokens: 500,
        spoken_chars: 1_500,
        speaking: Duration::from_secs(180),
    };

    /// An agent (Hermes, OpenClaw) that owns its tools and may run them before
    /// its first text.
    pub const AGENT: Self = Self {
        first_text: Duration::from_secs(120),
        rounds: 1,
        ack_after: Some(Duration::from_secs(3)),
        max_tokens: 1_000,
        ..Self::MODEL
    };

    /// STT, the first text and a margin.
    pub fn think_backstop(&self) -> Duration {
        self.stt + self.first_text + Duration::from_secs(5)
    }

    /// One tool round's first text, its call and a margin.
    pub fn round_backstop(&self) -> Duration {
        self.first_text + self.tool + Duration::from_secs(5)
    }

    /// The reply player's own bound.
    pub fn reply_backstop(&self) -> Duration {
        Duration::from_secs(12)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backstops_match_the_table() {
        let m = RunLimits::MODEL;
        assert_eq!(m.think_backstop(), Duration::from_secs(50));
        assert_eq!(m.round_backstop(), Duration::from_secs(45));
        assert_eq!(m.reply_backstop(), Duration::from_secs(12));

        let a = RunLimits::AGENT;
        assert_eq!(a.think_backstop(), Duration::from_secs(140));
        assert_eq!(a.round_backstop(), Duration::from_secs(135));
        assert_eq!(a.reply_backstop(), Duration::from_secs(12));
    }

    #[test]
    fn agent_differs_only_where_intended() {
        assert_eq!(RunLimits::AGENT.first_text, Duration::from_secs(120));
        assert_eq!(RunLimits::AGENT.rounds, 1);
        assert_eq!(RunLimits::AGENT.ack_after, Some(Duration::from_secs(3)));
        assert_eq!(RunLimits::AGENT.max_tokens, 1_000);
        assert_eq!(RunLimits::AGENT.spoken_chars, 1_500);
        assert_eq!(RunLimits::AGENT.stt, RunLimits::MODEL.stt);
        assert_eq!(RunLimits::AGENT.tool, RunLimits::MODEL.tool);
        assert_eq!(RunLimits::MODEL.max_tokens, 500);
        assert_eq!(RunLimits::MODEL.rounds, 5);
    }
}
