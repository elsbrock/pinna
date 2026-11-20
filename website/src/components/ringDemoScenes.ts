// The requests the hero demo (RingDemo.astro) plays, in order. The page draws
// the scene row from this list and the script plays it, so both agree.
//
// A device is an index into the drawings: 0 the Echo (2nd gen), named Hall,
// 1 the Echo Dot (2nd gen), named Kitchen. Each listens for its own wake
// word. A beat lights the devices it names and leaves the others dark; its
// first part sets the phase badge. The phase lengths are the demo's, not
// measured response times.
import type { Effect } from "../../../design/ring";

export type Part = { device: number; effect: Effect; request?: string; reply?: string };
export type Beat = { ms: number; parts: Part[] };
export type Scene = { name: string; mode: "home-assistant" | "standalone"; beats: Beat[] };

export const ECHO = 0;
export const DOT = 1;

/** The quiet between two requests. */
export const REST_MS = 3200;

/** Who answers, as the reply bubbles name it. */
export const MODE_NAME: Record<Scene["mode"], string> = {
  "home-assistant": "Home Assistant",
  standalone: "Standalone mode",
};

/** A request and its reply on one device. */
const ask = (device: number, request: string, reply: string): Beat[] => [
  { ms: 2800, parts: [{ device, effect: "listening", request }] },
  { ms: 1600, parts: [{ device, effect: "thinking", request }] },
  { ms: 3400, parts: [{ device, effect: "speaking", reply }] },
];

// The devices take turns: the message ends on the Dot, so the Echo answers
// next, and the list wraps round to the Dot.
export const SCENES: Scene[] = [
  {
    name: "Lights",
    mode: "home-assistant",
    beats: ask(DOT, "Okay Nabu, turn on the kitchen lights.", "The kitchen lights are on."),
  },
  {
    name: "Weather",
    mode: "standalone",
    beats: ask(ECHO, "Hey Jarvis, will it rain tomorrow?", "Light rain from noon, up to 12 degrees."),
  },
  {
    name: "Shopping list",
    mode: "home-assistant",
    beats: ask(DOT, "Okay Nabu, add milk to the shopping list.", "Milk is on the shopping list."),
  },
  {
    // The brain sends the message with its send_message tool while it thinks;
    // the Kitchen says it at once, then Hall answers.
    name: "Message to another satellite",
    mode: "standalone",
    beats: [
      { ms: 2800, parts: [{ device: ECHO, effect: "listening", request: "Hey Jarvis, tell the kitchen that dinner is ready." }] },
      { ms: 1200, parts: [{ device: ECHO, effect: "thinking", request: "Hey Jarvis, tell the kitchen that dinner is ready." }] },
      {
        ms: 3400,
        parts: [
          { device: DOT, effect: "speaking", reply: "Message from Hall: dinner is ready." },
          { device: ECHO, effect: "thinking" },
        ],
      },
      { ms: 2600, parts: [{ device: ECHO, effect: "speaking", reply: "I told the kitchen." }] },
    ],
  },
  {
    name: "Question",
    mode: "standalone",
    beats: ask(ECHO, "Hey Jarvis, how do I say thank you in Japanese?", "Arigatō gozaimasu."),
  },
];

/** "Lights, with Home Assistant", for the line under the scene row. */
export const sceneTitle = (s: Scene) =>
  s.mode === "home-assistant" ? `${s.name}, with Home Assistant` : `${s.name}, in standalone mode`;
