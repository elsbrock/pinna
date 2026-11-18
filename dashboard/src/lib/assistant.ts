// Standalone mode in words (ADR 0017): the mode, the three slots and what
// stops standalone from running. From Status and AssistantService's view; no
// Svelte and no runtime imports, so `node --test` runs it.
import type { Assistant, Preset, ProviderConnection } from "../gen/pinna/v1/assistant_pb.ts";
import type { AssistantStatus, Status } from "../gen/pinna/v1/device_pb.ts";
import type { Tone } from "./view.ts";

// pinna.v1.Mode values (a generated TS enum, which `node --test` cannot load).
export const MODE = { homeAssistant: 0, standalone: 1 } as const;

/** The satellite answers requests itself. */
export function isStandalone(s: Pick<Status, "mode"> | null | undefined): boolean {
  return s?.mode === MODE.standalone;
}

export type SlotKey = "stt" | "brain" | "tts";

/** The slots of a standalone request, in the order a request uses them. */
export const SLOTS: readonly { key: SlotKey; name: string; holds: "model" | "voice" }[] = [
  { key: "stt", name: "Speech-to-text", holds: "model" },
  { key: "brain", name: "Brain", holds: "model" },
  { key: "tts", name: "Text-to-speech", holds: "voice" },
];

const PRESET_NAMES: Record<string, string> = {
  openai: "OpenAI",
  openrouter: "OpenRouter",
  anthropic: "Anthropic",
  gemini: "Google Gemini",
  groq: "Groq",
  mistral: "Mistral",
  elevenlabs: "ElevenLabs",
  ollama: "Ollama",
  lmstudio: "LM Studio",
  hermes: "Hermes",
  openclaw: "OpenClaw",
  custom: "Other server",
};

/** A preset as people know the service; an id this page doesn't know as it is. */
export function presetName(id: string): string {
  return PRESET_NAMES[id] ?? id;
}

/** `gpt-4o-mini via OpenAI`; the service alone when it chooses the model itself. */
export function slotName(model: string, preset: string): string {
  return model ? `${model} via ${presetName(preset)}` : presetName(preset);
}

export type SlotLine = {
  key: SlotKey;
  name: string;
  ready: boolean;
  /** The state in a word or two, for a pn-health. */
  word: string;
  tone: Tone;
  /** What it runs on, or what to do about it. */
  detail: string;
};

/**
 * Each slot's state for the checklist that gates standalone mode. Readiness
 * comes from Status, which follows every change; the view adds what each slot
 * runs on. `failed` holds the sentence of a check that failed on this page.
 */
export function slotLines(
  status: AssistantStatus | undefined,
  view: Assistant | null,
  failed: Partial<Record<SlotKey, string>> = {},
): SlotLine[] {
  const live = status ? { stt: status.sttReady, brain: status.brainReady, tts: status.ttsReady } : null;
  return SLOTS.map(({ key, name, holds }) => {
    const choice = view?.[key];
    const ready = live ? live[key] : !!choice?.ready;
    const connection = view?.connections.find((c) => c.id === choice?.connection);
    const runsOn = choice && connection ? slotName(choice.model, connection.preset) : "";
    if (ready) return { key, name, ready, word: "Ready", tone: "ok", detail: runsOn || "Passed its check." };
    const why = failed[key];
    if (why) return { key, name, ready, word: "Check failed", tone: "danger", detail: why };
    if (choice)
      return {
        key,
        name,
        ready,
        word: "Not checked",
        tone: "caution",
        detail: `${runsOn ? `${runsOn}: its` : "Its"} connection changed since the last check. Check it again.`,
      };
    return { key, name, ready, word: "Not set up", tone: "caution", detail: `Choose a connection and a ${holds}.` };
  });
}

/** The slot a standalone failure names, as in "brain via openrouter: HTTP 402". */
export function errorSlot(error: string): SlotKey | "tool" | null {
  const m = /^(stt|brain|tts|tool) via /.exec(error);
  return m ? (m[1] as SlotKey | "tool") : null;
}

/** A server as the page shows it: its host and port, never a path or query. */
export function hostOf(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

/** A stored key: set, with its last four characters, which tell keys apart. */
export function keyShown(c: Pick<ProviderConnection, "keySet" | "keyLast4">): string {
  return c.keySet ? `set ••••${c.keyLast4}` : "no key";
}

/**
 * What a preset's address asks the owner for: nothing (a hosted service),
 * the computer a local server runs on, that and its port, or the whole
 * address. Follows the placeholders in the preset's address.
 */
export type AddressKind = "fixed" | "host" | "host-port" | "url";

export function addressKind(presetUrl: string): AddressKind {
  if (!presetUrl) return "url";
  if (presetUrl.includes("<host>:<port>")) return "host-port";
  if (presetUrl.includes("<host>")) return "host";
  return "fixed";
}

type Serves = Pick<Preset, "stt" | "brain" | "tts">;

export function servesSlot(p: Serves, key: SlotKey): boolean {
  return key === "stt" ? p.stt : key === "brain" ? p.brain : p.tts;
}

/** "a, b and c" */
function list(words: string[]): string {
  return words.length > 1 ? `${words.slice(0, -1).join(", ")} and ${words.at(-1)}` : (words[0] ?? "");
}

/** The slots a preset can serve, as a sentence's start: "Speech-to-text and text-to-speech". */
export function servesWords(p: Serves): string {
  const names = SLOTS.filter((s) => servesSlot(p, s.key)).map((s, i) => (i ? s.name.toLowerCase() : s.name));
  return list(names);
}

/** The slots a connection is chosen for. */
export function usedBy(view: Assistant, id: string): SlotKey[] {
  return SLOTS.filter((s) => view[s.key]?.connection === id).map((s) => s.key);
}

/** "the brain", "speech-to-text and the brain": slots inside a sentence. */
export function slotWords(keys: SlotKey[]): string {
  return list(keys.map((k) => (k === "brain" ? "the brain" : SLOTS.find((s) => s.key === k)!.name.toLowerCase())));
}

// pinna.v1.Delegation values (a generated TS enum, which `node --test` cannot load).
const DELEGATION = { toolLoop: 1, handOff: 2 } as const;

/**
 * Who runs the tools for the brain: the satellite (a tool loop, which can
 * offer the owner's MCP servers too) or the brain itself, an agent. The
 * Advanced setting wins over the preset; null while no brain is chosen.
 */
export function delegation(view: Assistant): "tool-loop" | "hand-off" | null {
  const choice = view.brain;
  const connection = view.connections.find((c) => c.id === choice?.connection);
  const preset = view.presets.find((p) => p.id === connection?.preset);
  if (!choice || !preset) return null;
  const set = view.brainOverride?.delegation;
  if (set === DELEGATION.toolLoop) return "tool-loop";
  if (set === DELEGATION.handOff) return "hand-off";
  return preset.handOff ? "hand-off" : "tool-loop";
}

/** A key typed, a key to remove, or the stored one kept. */
export type SecretEdit = { kind: "keep" } | { kind: "set"; value: string } | { kind: "clear" };

/** A SetConnection request's key fields for an edit. */
export function keyFields(e: SecretEdit): { key?: string; clearKey: boolean } {
  if (e.kind === "set") return { key: e.value, clearKey: false };
  return { clearKey: e.kind === "clear" };
}
