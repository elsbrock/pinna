//! The player's own count of the audio the server has outstanding, so a server that sends more
//! than `buffer_capacity` is closed rather than queued without bound (ADR 0027 §6).
//!
//! The server counts a chunk until its last sample is due; this count drops a chunk once its
//! first sample is due, because finding where a chunk ends would mean decoding it. So it never
//! counts more than the server may, apart from the clock filter's error, which the slack
//! covers.

use std::collections::VecDeque;

use crate::noise::MAX_MESSAGE;

/// Allowed above `buffer_capacity` before the server is taken to exceed it: one largest Noise
/// message, so a chunk counted a little late by the clock estimate cannot close the connection.
const SLACK: usize = MAX_MESSAGE;

#[derive(Debug)]
pub(crate) struct Budget {
    capacity: usize,
    used: usize,
    /// Timestamp and counted size of each chunk still due, in arrival order.
    chunks: VecDeque<(i64, usize)>,
}

impl Budget {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            used: 0,
            chunks: VecDeque::new(),
        }
    }

    /// `stream/end`, or `stream/clear` without `from_timestamp`.
    pub(crate) fn reset(&mut self) {
        self.used = 0;
        self.chunks.clear();
    }

    /// `stream/clear` with `from_timestamp`: the chunks it discards count as never sent.
    pub(crate) fn clear_from(&mut self, from: i64) {
        self.chunks.retain(|&(ts, _)| ts < from);
        self.used = self.chunks.iter().map(|&(_, n)| n).sum();
    }

    /// Counts a chunk of `size` bytes (header included) that arrived when the server's clock
    /// read `server_now`. Returns false when the server has exceeded the capacity.
    pub(crate) fn add(&mut self, timestamp: i64, size: usize, server_now: i64) -> bool {
        // A server sends chunks in time order, apart from a restart after `stream/clear`, so
        // the oldest are at the front.
        while let Some(&(ts, n)) = self.chunks.front() {
            if ts > server_now {
                break;
            }
            self.chunks.pop_front();
            self.used -= n;
        }
        self.chunks.push_back((timestamp, size));
        self.used += size;
        self.used <= self.capacity.saturating_add(SLACK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_until_a_chunk_is_due() {
        let mut b = Budget::new(100_000);
        // 33 chunks of 5000 bytes, 20 ms apart, all in the future: 165 000 bytes, within the
        // capacity and the slack.
        for i in 0..33 {
            assert!(b.add(1_000_000 + i * 20_000, 5000, 0), "chunk {i}");
        }
        assert_eq!(b.used, 165_000);
        assert!(!b.add(2_000_000, 5000, 0));
        // Once earlier chunks are due, there is room again.
        let mut b = Budget::new(100_000);
        for i in 0..20 {
            assert!(b.add(i * 20_000, 5000, 0));
        }
        assert!(b.add(400_000, 5000, 200_000));
        assert_eq!(b.used, 10 * 5000);
    }

    #[test]
    fn clears() {
        let mut b = Budget::new(1000);
        for i in 0..5 {
            b.add(i * 10, 100, -1);
        }
        b.clear_from(20);
        assert_eq!(b.used, 200);
        assert_eq!(b.chunks.len(), 2);
        b.reset();
        assert_eq!(b.used, 0);
        assert!(b.chunks.is_empty());
    }
}
