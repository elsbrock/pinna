//! Bounded, in-memory text turns shared with the brain.
//!
//! The Brain owns the conversation. A run receives [`Conversation::history`]
//! and returns one new [`Turn`] when it finishes.

use std::{collections::VecDeque, time::Duration, time::Instant};

use crate::presets::HistoryPolicy;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub user: String,
    pub assistant: String,
}

impl Turn {
    fn bytes(&self) -> usize {
        self.user.len().saturating_add(self.assistant.len())
    }
}

/// A conversation whose stored history always fits its policy.
pub struct Conversation {
    policy: HistoryPolicy,
    turns: VecDeque<Turn>,
    bytes: usize,
    last_turn: Option<Instant>,
}

impl Conversation {
    pub const IDLE: Duration = Duration::from_secs(300);

    pub fn new(policy: HistoryPolicy) -> Self {
        Self {
            policy,
            turns: VecDeque::new(),
            bytes: 0,
            last_turn: None,
        }
    }

    /// Return the history for a new request, forgetting an idle conversation.
    pub fn history(&mut self, now: Instant) -> Vec<Turn> {
        self.forget_expired(now);
        self.turns.iter().cloned().collect()
    }

    /// Commit a completed turn and drop the oldest history that no longer fits.
    pub fn push(&mut self, turn: Turn, now: Instant) {
        self.forget_expired(now);
        self.last_turn = Some(now);
        if self.policy == HistoryPolicy::NewTurnOnly {
            return;
        }

        self.bytes = self.bytes.saturating_add(turn.bytes());
        self.turns.push_back(turn);
        match self.policy {
            HistoryPolicy::Turns(max) => {
                while self.turns.len() > max {
                    self.drop_oldest();
                }
            }
            HistoryPolicy::Bytes(max) => {
                while self.bytes > max {
                    self.drop_oldest();
                }
            }
            HistoryPolicy::NewTurnOnly => unreachable!(),
        }
    }

    /// Clear all turns and apply a new policy.
    pub fn reset(&mut self, policy: HistoryPolicy) {
        self.policy = policy;
        self.turns.clear();
        self.bytes = 0;
        self.last_turn = None;
    }

    fn forget_expired(&mut self, now: Instant) {
        if self
            .last_turn
            .is_some_and(|last| now.saturating_duration_since(last) > Self::IDLE)
        {
            self.turns.clear();
            self.bytes = 0;
            self.last_turn = None;
        }
    }

    fn drop_oldest(&mut self) {
        if let Some(turn) = self.turns.pop_front() {
            self.bytes = self.bytes.saturating_sub(turn.bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(user: &str, assistant: &str) -> Turn {
        Turn {
            user: user.into(),
            assistant: assistant.into(),
        }
    }

    #[test]
    fn turn_policy_keeps_the_newest_turns() {
        let now = Instant::now();
        let mut conversation = Conversation::new(HistoryPolicy::Turns(2));
        conversation.push(turn("one", "first"), now);
        conversation.push(turn("two", "second"), now);
        conversation.push(turn("three", "third"), now);
        assert_eq!(
            conversation.history(now),
            [turn("two", "second"), turn("three", "third")]
        );

        conversation.reset(HistoryPolicy::Turns(0));
        conversation.push(turn("four", "fourth"), now);
        assert!(conversation.history(now).is_empty());
    }

    #[test]
    fn byte_policy_drops_oldest_turns_first() {
        let now = Instant::now();
        let mut conversation = Conversation::new(HistoryPolicy::Bytes(10));
        conversation.push(turn("ab", "cd"), now);
        conversation.push(turn("efgh", "ij"), now);
        assert_eq!(conversation.history(now).len(), 2);

        conversation.push(turn("k", "l"), now);
        assert_eq!(
            conversation.history(now),
            [turn("efgh", "ij"), turn("k", "l")]
        );

        conversation.push(turn("too long", "for the cap"), now);
        assert!(conversation.history(now).is_empty());
    }

    #[test]
    fn byte_policy_counts_utf8_bytes() {
        let now = Instant::now();
        let mut conversation = Conversation::new(HistoryPolicy::Bytes(5));
        conversation.push(turn("é", "🙂"), now);
        assert!(conversation.history(now).is_empty());

        conversation.reset(HistoryPolicy::Bytes(6));
        conversation.push(turn("é", "🙂"), now);
        assert_eq!(conversation.history(now), [turn("é", "🙂")]);
    }

    #[test]
    fn new_turn_only_sends_no_earlier_turns() {
        let now = Instant::now();
        let mut conversation = Conversation::new(HistoryPolicy::NewTurnOnly);
        conversation.push(turn("one", "first"), now);
        conversation.push(turn("two", "second"), now);
        assert!(conversation.history(now).is_empty());
    }

    #[test]
    fn history_expires_after_five_idle_minutes() {
        let start = Instant::now();
        let mut conversation = Conversation::new(HistoryPolicy::Turns(10));
        conversation.push(turn("one", "first"), start);
        assert_eq!(
            conversation.history(start + Duration::from_secs(299)),
            [turn("one", "first")]
        );
        assert_eq!(
            conversation.history(start + Duration::from_secs(300)),
            [turn("one", "first")]
        );
        assert!(
            conversation
                .history(start + Duration::from_secs(301))
                .is_empty()
        );

        conversation.push(turn("two", "second"), start + Duration::from_secs(302));
        assert_eq!(
            conversation.history(start + Duration::from_secs(302)),
            [turn("two", "second")]
        );
    }

    #[test]
    fn push_forgets_an_expired_conversation_and_reset_clears_it() {
        let start = Instant::now();
        let mut conversation = Conversation::new(HistoryPolicy::Turns(10));
        conversation.push(turn("old", "turn"), start);
        conversation.push(turn("new", "turn"), start + Duration::from_secs(301));
        assert_eq!(
            conversation.history(start + Duration::from_secs(301)),
            [turn("new", "turn")]
        );

        conversation.reset(HistoryPolicy::Bytes(64));
        assert!(
            conversation
                .history(start + Duration::from_secs(301))
                .is_empty()
        );
    }
}
