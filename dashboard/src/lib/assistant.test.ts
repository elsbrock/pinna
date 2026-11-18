import assert from "node:assert/strict";
import { test } from "node:test";

import type { Assistant, BrainOverride, Preset, ProviderConnection, SlotChoice } from "../gen/pinna/v1/assistant_pb.ts";
import type { AssistantStatus } from "../gen/pinna/v1/device_pb.ts";
import {
  addressKind,
  delegation,
  errorSlot,
  hostOf,
  isStandalone,
  keyFields,
  keyShown,
  MODE,
  presetName,
  servesWords,
  slotLines,
  slotName,
  slotWords,
  usedBy,
} from "./assistant.ts";

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
const preset = (id: string, over: Partial<Preset> = {}): Preset => ({
  $typeName: "pinna.v1.Preset",
  id,
  baseUrl: "https://api.example.com/v1",
  stt: false,
  brain: true,
  tts: false,
  model: "",
  handOff: false,
  ...over,
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
  assert.equal(presetName("custom"), "Other server");
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

test("a failure names its slot", () => {
  assert.equal(errorSlot("brain via openrouter: HTTP 402"), "brain");
  assert.equal(errorSlot("stt via groq: timeout"), "stt");
  assert.equal(errorSlot("tool via openai: protocol"), "tool");
  assert.equal(errorSlot("something else"), null);
  assert.equal(errorSlot(""), null);
});

test("a server shows by host and port, a key by its last four characters", () => {
  assert.equal(hostOf("https://api.openai.com/v1"), "api.openai.com");
  assert.equal(hostOf("http://192.168.1.20:11434/v1"), "192.168.1.20:11434");
  assert.equal(hostOf("not a url"), "not a url");
  assert.equal(keyShown({ keySet: true, keyLast4: "abcd" }), "set ••••abcd");
  assert.equal(keyShown({ keySet: false, keyLast4: "" }), "no key");
});

test("the address a preset asks for follows its placeholders", () => {
  assert.equal(addressKind("https://api.openai.com/v1"), "fixed");
  assert.equal(addressKind("http://<host>:11434/v1"), "host");
  assert.equal(addressKind("http://<host>:<port>/v1"), "host-port");
  assert.equal(addressKind(""), "url");
});

test("slots in words", () => {
  assert.equal(servesWords({ stt: true, brain: true, tts: true }), "Speech-to-text, brain and text-to-speech");
  assert.equal(servesWords({ stt: true, brain: false, tts: true }), "Speech-to-text and text-to-speech");
  assert.equal(slotWords(["brain"]), "the brain");
  assert.equal(slotWords(["stt", "brain", "tts"]), "speech-to-text, the brain and text-to-speech");
  const v = view({ stt: slot("openai-1", "whisper-1"), tts: slot("openai-1", "nova"), brain: slot("x-1", "m") });
  assert.deepEqual(usedBy(v, "openai-1"), ["stt", "tts"]);
  assert.deepEqual(usedBy(v, "gone"), []);
});

test("who runs the tools: the Advanced setting, else the brain's preset", () => {
  const over = (delegation: number): BrainOverride => ({
    $typeName: "pinna.v1.BrainOverride",
    delegation,
    history: 0,
    historyLimit: 0,
  });
  const base = view({
    connections: [connection("openai-1", "openai"), connection("hermes-1", "hermes")],
    presets: [preset("openai"), preset("hermes", { handOff: true })],
  });
  assert.equal(delegation(base), null);
  assert.equal(delegation({ ...base, brain: slot("openai-1", "gpt") }), "tool-loop");
  assert.equal(delegation({ ...base, brain: slot("hermes-1", "hermes") }), "hand-off");
  assert.equal(delegation({ ...base, brain: slot("hermes-1", "hermes"), brainOverride: over(1) }), "tool-loop");
  assert.equal(delegation({ ...base, brain: slot("openai-1", "gpt"), brainOverride: over(2) }), "hand-off");
  assert.equal(delegation({ ...base, brain: slot("openai-1", "gpt"), brainOverride: over(0) }), "tool-loop");
});

test("a key edit becomes the request's key fields", () => {
  assert.deepEqual(keyFields({ kind: "set", value: "sk-1" }), { key: "sk-1", clearKey: false });
  assert.deepEqual(keyFields({ kind: "clear" }), { clearKey: true });
  assert.deepEqual(keyFields({ kind: "keep" }), { clearKey: false });
});
