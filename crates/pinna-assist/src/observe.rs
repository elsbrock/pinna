//! The metrics port. Provider code reports values; pinnad owns the registry.

use crate::{
    error::{ProviderError, Slot},
    presets::PresetId,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spend {
    AudioSeconds(f32),
    Tokens { input: u64, output: u64 },
    Characters(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Guard {
    NoSpeech,
    Invented,
    FollowUp,
    Ack,
    StoppedByWake,
    StoppedByButton,
}

pub trait Observer: Send + Sync {
    fn stage(&self, slot: Slot, seconds: f64);
    fn spent(&self, preset: PresetId, spend: Spend);
    fn guard(&self, guard: Guard);
    fn failed(&self, error: &ProviderError);
}
