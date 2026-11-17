//! Timer state shared by Home Assistant mirrors and device timers (ADR 0017).

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    Ha,
    #[expect(dead_code)]
    Local,
}

pub(super) struct Timer {
    pub(super) name: String,
    pub(super) total: u32,
    pub(super) left: u32,
    pub(super) active: bool,
    pub(super) at: Instant,
    #[expect(dead_code)]
    pub(super) origin: Origin,
}

impl Timer {
    pub(super) fn remaining(&self, now: Instant) -> u32 {
        if self.active {
            self.left
                .saturating_sub(now.saturating_duration_since(self.at).as_secs() as u32)
        } else {
            self.left
        }
    }
}
