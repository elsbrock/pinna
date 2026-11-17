//! In-memory run, observer and tool fakes for tests that use no sockets.

use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};

use futures_core::future::BoxFuture;
use pinna_mcp::{ToolCall, ToolOutput, ToolSet, ToolSpec};
use serde_json::Value;

use crate::{
    error::{ProviderError, Slot},
    observe::{Guard, Observer, Spend},
    presets::PresetId,
    run::{RunEvent, RunEvents},
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("fake mutex")
}

#[derive(Default)]
pub struct FakeRunEvents {
    events: Mutex<Vec<RunEvent>>,
}

impl FakeRunEvents {
    pub fn events(&self) -> Vec<RunEvent> {
        lock(&self.events).clone()
    }

    pub fn take(&self) -> Vec<RunEvent> {
        std::mem::take(&mut *lock(&self.events))
    }
}

impl RunEvents for FakeRunEvents {
    fn emit(&self, event: RunEvent) -> BoxFuture<'_, ()> {
        lock(&self.events).push(event);
        Box::pin(async {})
    }
}

#[derive(Default)]
pub struct FakeObserver {
    pub stages: Mutex<Vec<(Slot, f64)>>,
    pub spends: Mutex<Vec<(PresetId, Spend)>>,
    pub guards: Mutex<Vec<Guard>>,
    pub failures: Mutex<Vec<ProviderError>>,
}

impl Observer for FakeObserver {
    fn stage(&self, slot: Slot, seconds: f64) {
        lock(&self.stages).push((slot, seconds));
    }

    fn spent(&self, preset: PresetId, spend: Spend) {
        lock(&self.spends).push((preset, spend));
    }

    fn guard(&self, guard: Guard) {
        lock(&self.guards).push(guard);
    }

    fn failed(&self, error: &ProviderError) {
        lock(&self.failures).push(error.clone());
    }
}

/// A tool set with fixed specifications and outputs returned in call order.
pub struct FakeToolSet {
    specs: Vec<ToolSpec>,
    outputs: Mutex<VecDeque<ToolOutput>>,
    calls: Mutex<Vec<ToolCall>>,
}

impl FakeToolSet {
    pub fn new(specs: Vec<ToolSpec>, outputs: impl IntoIterator<Item = ToolOutput>) -> Self {
        Self {
            specs,
            outputs: Mutex::new(outputs.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<ToolCall> {
        lock(&self.calls).clone()
    }
}

impl ToolSet for FakeToolSet {
    fn specs(&self) -> BoxFuture<'_, Vec<ToolSpec>> {
        let specs = self.specs.clone();
        Box::pin(async move { specs })
    }

    fn call<'a>(&'a self, name: &'a str, args: Value) -> BoxFuture<'a, ToolOutput> {
        lock(&self.calls).push(ToolCall {
            id: String::new(),
            name: name.to_owned(),
            args,
        });
        let output = lock(&self.outputs)
            .pop_front()
            .unwrap_or_else(|| ToolOutput::error("No fake tool output remains."));
        Box::pin(async move { output })
    }
}
