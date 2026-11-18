// Standalone mode in words (ADR 0017): the mode, the three slots and what
// stops standalone from running. From Status and AssistantService's view; no
// Svelte and no runtime imports, so `node --test` runs it.
import type { Assistant } from "../gen/pinna/v1/assistant_pb.ts";
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
  custom: "Another server",
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
