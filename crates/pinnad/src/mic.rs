//! Microphone routing for Home Assistant and standalone runs (ADR 0017).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use tokio::sync::mpsc;

#[allow(dead_code)]
pub const MIC_QUEUE: usize = 64;
const PREROLL_FRAMES: usize = 10;

/// The Brain's microphone route. Opening or replacing it changes the generation,
/// so the capture loop starts each route with the current pre-roll.
#[derive(Default)]
pub struct MicTap {
    gen_: AtomicU64,
    slot: Mutex<Option<mpsc::Sender<Bytes>>>,
}

impl MicTap {
    #[allow(dead_code)]
    pub fn open(&self, tx: mpsc::Sender<Bytes>) {
        let mut slot = self.slot.lock().unwrap();
        *slot = Some(tx);
        self.gen_.fetch_add(1, Ordering::SeqCst);
    }

    pub fn close(&self) {
        let mut slot = self.slot.lock().unwrap();
        slot.take();
        self.gen_.fetch_add(1, Ordering::SeqCst);
    }

    /// Refresh a capture loop's cached sender. Returns true only at a route edge.
    pub fn current(&self, seen: &mut u64, cached: &mut Option<mpsc::Sender<Bytes>>) -> bool {
        if self.gen_.load(Ordering::Acquire) == *seen {
            return false;
        }
        let slot = self.slot.lock().unwrap();
        let generation = self.gen_.load(Ordering::Acquire);
        if generation == *seen {
            return false;
        }
        *cached = slot.clone();
        *seen = generation;
        true
    }
}

/// The capture work for one frame. Home Assistant events are returned rather
/// than sent so routing stays synchronous and testable without a server.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Routed {
    pub home_assistant: Vec<Bytes>,
    pub end_home_assistant: bool,
    pub detect_wake: bool,
    pub reset_detector: bool,
}

/// Post-gain microphone routing and the shared 200 ms pre-roll.
#[derive(Default)]
pub struct MicRouter {
    preroll: VecDeque<Bytes>,
    generation: u64,
    local: Option<mpsc::Sender<Bytes>>,
    was_local: bool,
    was_streaming: bool,
    dropped: u64,
}

impl MicRouter {
    pub fn route(&mut self, frame: Bytes, streaming: bool, tap: &MicTap) -> Routed {
        let route_changed = tap.current(&mut self.generation, &mut self.local);
        let local = !streaming && self.local.is_some();
        let new_local = local && (!self.was_local || route_changed);
        let mut routed = Routed {
            end_home_assistant: self.was_streaming && !streaming,
            reset_detector: (self.was_streaming && !streaming)
                || new_local
                || (self.was_local && !local),
            ..Routed::default()
        };

        // Home Assistant keeps priority if a stale local route overlaps its stream.
        if streaming {
            if !self.was_streaming {
                routed.home_assistant.extend(self.preroll.drain(..));
            }
            routed.home_assistant.push(frame);
        } else if local {
            if new_local {
                let preroll: Vec<_> = self.preroll.drain(..).collect();
                for frame in preroll {
                    self.send_local(frame);
                }
            }
            self.send_local(frame);
        } else {
            if self.preroll.len() == PREROLL_FRAMES {
                self.preroll.pop_front();
            }
            self.preroll.push_back(frame);
            routed.detect_wake = true;
        }

        self.was_streaming = streaming;
        self.was_local = local;
        routed
    }

    #[allow(dead_code)]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn clear_preroll(&mut self) {
        self.preroll.clear();
    }

    fn send_local(&mut self, frame: Bytes) {
        let Some(tx) = &self.local else {
            return;
        };
        if tx.try_send(frame).is_err() {
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: u8) -> Bytes {
        Bytes::from(vec![value])
    }

    fn values(rx: &mut mpsc::Receiver<Bytes>) -> Vec<u8> {
        let mut values = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            values.extend(frame);
        }
        values
    }

    #[test]
    fn a_new_local_route_flushes_preroll_without_home_assistant_audio() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();
        router.route(frame(1), false, &tap);
        router.route(frame(2), false, &tap);

        let (tx, mut rx) = mpsc::channel(MIC_QUEUE);
        tap.open(tx);
        let routed = router.route(frame(3), false, &tap);

        assert!(routed.home_assistant.is_empty());
        assert!(!routed.end_home_assistant);
        assert!(!routed.detect_wake);
        assert!(routed.reset_detector);
        assert_eq!(values(&mut rx), [1, 2, 3]);

        tap.close();
        let routed = router.route(frame(4), false, &tap);
        assert!(routed.home_assistant.is_empty());
        assert!(!routed.end_home_assistant);
        assert!(routed.detect_wake);
        assert!(routed.reset_detector);
    }

    #[test]
    fn a_full_local_queue_drops_and_counts_the_frame() {
        let tap = MicTap::default();
        let (tx, mut rx) = mpsc::channel(1);
        tap.open(tx);
        let mut router = MicRouter::default();

        router.route(frame(1), false, &tap);
        router.route(frame(2), false, &tap);

        assert_eq!(router.dropped(), 1);
        assert_eq!(values(&mut rx), [1]);
    }

    #[test]
    fn a_new_generation_uses_preroll_gathered_since_the_last_route() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();
        let (first, mut first_rx) = mpsc::channel(MIC_QUEUE);
        tap.open(first);
        router.route(frame(1), false, &tap);
        assert_eq!(values(&mut first_rx), [1]);

        tap.close();
        router.route(frame(2), false, &tap);
        router.route(frame(3), false, &tap);
        let (second, mut second_rx) = mpsc::channel(MIC_QUEUE);
        tap.open(second);
        router.route(frame(4), false, &tap);

        assert_eq!(values(&mut second_rx), [2, 3, 4]);
    }

    #[test]
    fn home_assistant_routing_keeps_its_preroll_and_end_event() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();
        router.route(frame(1), false, &tap);
        router.route(frame(2), false, &tap);

        let routed = router.route(frame(3), true, &tap);
        assert_eq!(routed.home_assistant, [frame(1), frame(2), frame(3)]);
        assert!(!routed.end_home_assistant);
        assert!(!routed.detect_wake);

        let routed = router.route(frame(4), false, &tap);
        assert!(routed.home_assistant.is_empty());
        assert!(routed.end_home_assistant);
        assert!(routed.detect_wake);
        assert!(routed.reset_detector);
    }
}
