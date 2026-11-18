import assert from "node:assert/strict";
import { test } from "node:test";

import type { Assistant, ProviderConnection, SlotChoice } from "../gen/pinna/v1/assistant_pb.ts";
import type { AssistantStatus } from "../gen/pinna/v1/device_pb.ts";
import { isStandalone, MODE, presetName, slotLines, slotName } from "./assistant.ts";

// node's type stripping can't run the generated module (it holds a TS enum),
// so the tests build message shapes by hand.
const status = (over: Partial<AssistantStatus> = {}): AssistantStatus => ({
  $typeName: "pinna.v1.AssistantStatus",
  ready: false,
  sttReady: false,
  brainReady: false,
  ttsReady: false,
  label: "",
  lastError: "",
  ...over,
});
const connection = (id: string, preset: string): ProviderConnection => ({
  $typeName: "pinna.v1.ProviderConnection",
  id,
  preset,
  baseUrl: "https://api.example.com/v1",
  revision: 1n,
  keySet: true,
  keyLast4: "abcd",
});
const slot = (conn: string, model: string, ready = true): SlotChoice => ({
  $typeName: "pinna.v1.SlotChoice",
  connection: conn,
  model,
  ready,
});
const view = (over: Partial<Assistant> = {}): Assistant =>
  ({
    $typeName: "pinna.v1.Assistant",
    ready: false,
    connections: [],
    language: "",
    followUps: true,
    tools: {},
    stations: [],
    mcpServers: [],
    agents: [],
    presets: [],
    ackSpoken: "One moment.",
    voiceRules: "",
    deviceTools: [],
    ...over,
  }) as Assistant;

test("the mode comes from Status", () => {
  assert.equal(isStandalone({ mode: MODE.standalone }), true);
  assert.equal(isStandalone({ mode: MODE.homeAssistant }), false);
  assert.equal(isStandalone(null), false);
});

test("services by the names people know them", () => {
  assert.equal(presetName("openai"), "OpenAI");
  assert.equal(presetName("lmstudio"), "LM Studio");
  assert.equal(presetName("custom"), "Another server");
  assert.equal(presetName("martian"), "martian");
  assert.equal(slotName("gpt-4o-mini", "openai"), "gpt-4o-mini via OpenAI");
  // OpenClaw chooses its own model.
  assert.equal(slotName("", "openclaw"), "OpenClaw");
});

test("the checklist: ready, not checked, failed on this page, not set up", () => {
  const v = view({
    connections: [connection("openai-1", "openai"), connection("anthropic-1", "anthropic")],
    stt: slot("openai-1", "whisper-1"),
    brain: slot("anthropic-1", "claude-x", false),
  });
  const lines = slotLines(status({ sttReady: true }), v);

  assert.deepEqual(
    lines.map((l) => [l.key, l.word, l.tone]),
    [
      ["stt", "Ready", "ok"],
      ["brain", "Not checked", "caution"],
      ["tts", "Not set up", "caution"],
    ],
  );
  assert.equal(lines[0].detail, "whisper-1 via OpenAI");
  assert.equal(lines[1].detail, "claude-x via Anthropic: its connection changed since the last check. Check it again.");
  assert.equal(lines[2].detail, "Choose a connection and a voice.");

  const failed = slotLines(status({ sttReady: true }), v, { brain: "The key is wrong. Enter it again." });
  assert.deepEqual([failed[1].word, failed[1].tone, failed[1].detail], [
    "Check failed",
    "danger",
    "The key is wrong. Enter it again.",
  ]);
});

test("Status decides readiness; without the view the slots are only ready or not", () => {
  const stale = view({ connections: [connection("openai-1", "openai")], tts: slot("openai-1", "nova", true) });
  assert.equal(slotLines(status(), stale)[2].word, "Not checked");

  const lines = slotLines(status({ ready: true, sttReady: true, brainReady: true, ttsReady: true }), null);
  assert.ok(lines.every((l) => l.ready && l.detail === "Passed its check."));
  assert.deepEqual(
    slotLines(undefined, stale).map((l) => l.ready),
    [false, false, true],
  );
});
