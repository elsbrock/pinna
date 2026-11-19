//! The player's own count of the audio the server has outstanding, so a server that sends more
//! than `buffer_capacity` is closed rather than queued without bound (ADR 0027 §6).
//!
//! The server counts a chunk until its last sample is due; this count drops a chunk once its
//! first sample is due, because finding where a chunk ends would mean decoding it. So it never
//! counts more than the server may, apart from the clock filter's error, which the slack
//! covers. How long a chunk lasts is for `pinnad` to bound when it decodes it.
//!
//! A chunk is due when the clock filter placed it on the player's clock as it arrived. The
//! server's replies to `client/time` move the filter, so a count that converted again later
//! would let the server release audio it still has outstanding by moving its clock.

use std::collections::VecDeque;

use crate::noise::MAX_MESSAGE;

/// Allowed above `buffer_capacity` before the server is taken to exceed it: one largest Noise
/// message, so a chunk counted a little late by the clock estimate cannot close the connection.
const SLACK: usize = MAX_MESSAGE;

#[derive(Debug)]
pub(crate) struct Budget {
    capacity: usize,
    used: usize,
    /// Server timestamp, due time on the player's clock, and counted size of each chunk not
    /// yet due, in arrival order.
    chunks: VecDeque<(i64, i64, usize)>,
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
        self.chunks.retain(|&(ts, _, _)| ts < from);
        self.used = self.chunks.iter().map(|&(_, _, n)| n).sum();
    }

    /// Counts a chunk of `size` bytes (header included) with the server timestamp `timestamp`,
    /// due at `due` on the player's clock, arriving at the player's time `now`. Returns false
    /// when the server has exceeded the capacity.
    pub(crate) fn add(&mut self, timestamp: i64, due: i64, size: usize, now: i64) -> bool {
        // A server sends chunks in time order, apart from a restart after `stream/clear`, so
        // the oldest are at the front.
        while let Some(&(_, d, n)) = self.chunks.front() {
            if d > now {
                break;
            }
            self.chunks.pop_front();
            self.used -= n;
        }
        self.chunks.push_back((timestamp, due, size));
        self.used += size;
        self.used <= self.capacity.saturating_add(SLACK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's clock 7 s ahead of the player's.
    const OFFSET: i64 = 7_000_000;

    #[test]
    fn counts_until_a_chunk_is_due() {
        let mut b = Budget::new(100_000);
        // 33 chunks of 5000 bytes, 20 ms apart, all in the future: 165 000 bytes, within the
        // capacity and the slack.
        for i in 0..33 {
            let due = 1_000_000 + i * 20_000;
            assert!(b.add(due + OFFSET, due, 5000, 0), "chunk {i}");
        }
        assert_eq!(b.used, 165_000);
        assert!(!b.add(2_000_000 + OFFSET, 2_000_000, 5000, 0));
        // Once earlier chunks are due on the player's clock, there is room again.
        let mut b = Budget::new(100_000);
        for i in 0..20 {
            assert!(b.add(i * 20_000 + OFFSET, i * 20_000, 5000, 0));
        }
        assert!(b.add(400_000 + OFFSET, 400_000, 5000, 200_000));
        assert_eq!(b.used, 10 * 5000);
    }

    #[test]
    fn clears() {
        let mut b = Budget::new(1000);
        for i in 0..5 {
            b.add(i * 10 + OFFSET, i * 10, 100, -1);
        }
        b.clear_from(20 + OFFSET);
        assert_eq!(b.used, 200);
        assert_eq!(b.chunks.len(), 2);
        b.reset();
        assert_eq!(b.used, 0);
        assert!(b.chunks.is_empty());
    }
}
