//! Microphone routing for Home Assistant and standalone runs (ADR 0017).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use pinna_vad::{Endpoint, EndpointConfig, Endpointer};
use tokio::sync::mpsc;

pub const MIC_QUEUE: usize = 64;
const PREROLL_FRAMES: usize = 10;

/// The Brain's microphone route. Opening or replacing it changes the generation,
/// so the capture loop starts each route with the current pre-roll.
#[derive(Default)]
pub struct MicTap {
    gen_: AtomicU64,
    slot: Mutex<Option<mpsc::Sender<Bytes>>>,
    /// The shadow endpointer on the current Home Assistant stream, shared so the
    /// Brain can log it beside Home Assistant's own end of speech.
    shadow: Mutex<ShadowReport>,
}

/// How far the shadow endpointer got on the current Home Assistant stream.
/// Times count audio, not wall clock: milliseconds of microphone audio sent to
/// Home Assistant since the stream began, the pre-roll included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShadowReport {
    pub streamed_ms: u64,
    /// The shadow's decision, and how much audio it had seen when it made it.
    pub end: Option<(Endpoint, u64)>,
}

impl MicTap {
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

    /// Publish the shadow endpointer's progress. It is only ever read for a
    /// log line.
    fn report_shadow(&self, report: ShadowReport) {
        *self.shadow.lock().unwrap() = report;
    }

    /// The shadow endpointer's progress on the current Home Assistant stream.
    pub fn shadow_report(&self) -> ShadowReport {
        *self.shadow.lock().unwrap()
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
    /// A private endpointer run beside Home Assistant's own VAD (ADR 0017).
    /// Its decision is logged, never sent to Home Assistant.
    shadow: Option<Endpointer>,
    shadow_report: ShadowReport,
}

impl MicRouter {
    pub fn route(
        &mut self,
        frame: Bytes,
        streaming: bool,
        tap: &MicTap,
        noise_seed: Option<u64>,
    ) -> Routed {
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
                self.shadow = Some(Endpointer::new(EndpointConfig::REQUEST, noise_seed));
                self.shadow_report = ShadowReport::default();
                tap.report_shadow(self.shadow_report);
                let preroll: Vec<_> = self.preroll.drain(..).collect();
                for frame in preroll {
                    self.feed_shadow(&frame, tap);
                    routed.home_assistant.push(frame);
                }
            }
            self.feed_shadow(&frame, tap);
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

    /// Run the shadow endpointer over a frame that is going to Home Assistant.
    /// Its decision is stored on the tap; nothing here changes what Home
    /// Assistant receives.
    fn feed_shadow(&mut self, frame: &Bytes, tap: &MicTap) {
        let Some(shadow) = self.shadow.as_mut() else {
            return;
        };
        let pcm: Vec<i16> = frame
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let state = shadow.push(&pcm);
        // 16 kHz mono: 16 samples per millisecond.
        self.shadow_report.streamed_ms += pcm.len() as u64 / 16;
        if state.is_terminal() && self.shadow_report.end.is_none() {
            self.shadow_report.end = Some((state, self.shadow_report.streamed_ms));
        }
        tap.report_shadow(self.shadow_report);
    }

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
        router.route(frame(1), false, &tap, None);
        router.route(frame(2), false, &tap, None);

        let (tx, mut rx) = mpsc::channel(MIC_QUEUE);
        tap.open(tx);
        let routed = router.route(frame(3), false, &tap, None);

        assert!(routed.home_assistant.is_empty());
        assert!(!routed.end_home_assistant);
        assert!(!routed.detect_wake);
        assert!(routed.reset_detector);
        assert_eq!(values(&mut rx), [1, 2, 3]);

        tap.close();
        let routed = router.route(frame(4), false, &tap, None);
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

        router.route(frame(1), false, &tap, None);
        router.route(frame(2), false, &tap, None);

        assert_eq!(router.dropped(), 1);
        assert_eq!(values(&mut rx), [1]);
    }

    #[test]
    fn a_new_generation_uses_preroll_gathered_since_the_last_route() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();
        let (first, mut first_rx) = mpsc::channel(MIC_QUEUE);
        tap.open(first);
        router.route(frame(1), false, &tap, None);
        assert_eq!(values(&mut first_rx), [1]);

        tap.close();
        router.route(frame(2), false, &tap, None);
        router.route(frame(3), false, &tap, None);
        let (second, mut second_rx) = mpsc::channel(MIC_QUEUE);
        tap.open(second);
        router.route(frame(4), false, &tap, None);

        assert_eq!(values(&mut second_rx), [2, 3, 4]);
    }

    #[test]
    fn home_assistant_routing_keeps_its_preroll_and_end_event() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();
        router.route(frame(1), false, &tap, None);
        router.route(frame(2), false, &tap, None);

        let routed = router.route(frame(3), true, &tap, None);
        assert_eq!(routed.home_assistant, [frame(1), frame(2), frame(3)]);
        assert!(!routed.end_home_assistant);
        assert!(!routed.detect_wake);

        let routed = router.route(frame(4), false, &tap, None);
        assert!(routed.home_assistant.is_empty());
        assert!(routed.end_home_assistant);
        assert!(routed.detect_wake);
        assert!(routed.reset_detector);
    }

    /// One endpointer frame: 160 samples of 16-bit PCM, as the capture loop
    /// hands Home Assistant.
    fn pcm(amp: i16) -> Bytes {
        let mut out = Vec::with_capacity(320);
        for _ in 0..160 {
            out.extend_from_slice(&amp.to_le_bytes());
        }
        Bytes::from(out)
    }

    #[test]
    fn the_shadow_endpointer_runs_only_for_home_assistant_streams() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();

        // Idle frames feed the wake word, never the shadow endpointer.
        for _ in 0..600 {
            router.route(pcm(10), false, &tap, Some(256));
        }
        assert_eq!(tap.shadow_report(), ShadowReport::default());

        // A standalone route is not Home Assistant either.
        let (tx, _rx) = mpsc::channel(MIC_QUEUE);
        tap.open(tx);
        for _ in 0..100 {
            router.route(pcm(3000), false, &tap, Some(256));
        }
        assert_eq!(tap.shadow_report(), ShadowReport::default());
        tap.close();
    }

    #[test]
    fn the_shadow_endpointer_reports_the_end_of_speech_on_the_home_assistant_stream() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();
        for _ in 0..100 {
            router.route(pcm(3000), true, &tap, Some(256));
        }
        for _ in 0..120 {
            router.route(pcm(10), true, &tap, Some(256));
        }
        let report = tap.shadow_report();
        // 220 frames of 10 ms were streamed; speech stopped after 1000 ms.
        assert_eq!(report.streamed_ms, 2200);
        let (end, at) = report.end.unwrap();
        assert_eq!(end, Endpoint::Ended);
        assert!((1000..=2200).contains(&at), "ended after {at} ms");
    }

    #[test]
    fn the_shadow_endpointer_reports_no_speech_on_a_quiet_stream() {
        let tap = MicTap::default();
        let mut router = MicRouter::default();
        for _ in 0..520 {
            router.route(pcm(10), true, &tap, Some(256));
        }
        let (end, at) = tap.shadow_report().end.unwrap();
        assert_eq!(end, Endpoint::NoSpeech);
        // NoSpeech comes 5 s after the start, and the decision keeps its time.
        assert_eq!(at, 5000);
    }
}
