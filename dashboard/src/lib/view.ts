// What the page shows, derived from Status: words, tones, disabled reasons and
// ticking numbers. No Svelte and no runtime imports, so `node --test` runs it.
import type { Effect } from "../../../design/ring.ts";
import type { Status, Timer } from "../gen/pinna/v1/device_pb.ts";

/** The connection to the satellite's WatchStatus stream. */
export type Link = "connecting" | "live" | "offline";

// pinna.v1.Phase values (generated as a TS enum, which `node --test` cannot load).
const PHASE_WORDS: Record<number, "listening" | "thinking" | "speaking"> = {
  2: "listening",
  3: "thinking",
  4: "speaking",
};

/** The ring effect, in the device's own order: muted, a ringing alarm, the phase. */
export function ringEffect(s: Status | null, link: Link): Effect {
  if (!s || link !== "live") return "idle";
  if (s.muted) return "muted";
  if (s.alarmRinging) return "timer-ringing";
  return PHASE_WORDS[s.phase] ?? "idle";
}

export type Badge = { phase: string; word: string };

/** The PhaseBadge, in the ring's order: Muted, a ringing alarm, the proto's phase words. */
export function badge(s: Status): Badge {
  if (s.muted) return { phase: "muted", word: "Muted" };
  if (s.alarmRinging) return { phase: "timer", word: "Ringing" };
  const phase = PHASE_WORDS[s.phase];
  if (phase) return { phase, word: phase[0].toUpperCase() + phase.slice(1) };
  return { phase: "idle", word: "Idle" };
}

export type Tone = "ok" | "caution" | "danger";
export type Health = { tone: Tone; text: string };

/** The Home Assistant health line. */
export function health(s: Status | null, link: Link, offlineSince: Date | null): Health {
  if (link === "offline" || !s) {
    const since = offlineSince ? ` since ${hhmm(offlineSince)}` : "";
    return { tone: "danger", text: `Not reachable${since}` };
  }
  if (!s.haConnected) return { tone: "caution", text: "Not connected to Home Assistant" };
  if (!s.voiceSubscribed) return { tone: "caution", text: "Connected, no voice pipeline" };
  return { tone: "ok", text: "Connected to Home Assistant" };
}

/** Why Start conversation is disabled, or null when it can be pressed. */
export function startBlocked(s: Status | null, link: Link): string | null {
  if (!s || link !== "live") return "The satellite is not reachable.";
  if (!s.haConnected) return "Connect Home Assistant to start a conversation.";
  if (!s.voiceSubscribed) return "Home Assistant has no voice pipeline for this satellite yet.";
  if (s.muted) return "Turn the microphones on to start a conversation.";
  if (s.alarmRinging) return "Stop the timer alarm first.";
  if (s.phase !== 1) return "A conversation is already running.";
  return null;
}

/** Stop applies to a conversation, an announcement or a ringing alarm. */
export function canStop(s: Status | null, link: Link): boolean {
  return !!s && link === "live" && (s.alarmRinging || (s.phase !== 1 && s.phase !== 0));
}

/** Seconds left on a timer, `elapsedMs` after its Status arrived. */
export function timerLeft(t: Timer, elapsedMs: number): number {
  if (!t.active) return t.secondsLeft;
  return Math.max(0, t.secondsLeft - Math.floor(elapsedMs / 1000));
}

/** `04:32`, or `1:04:32` from an hour up. */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s % 60)}` : `${pad(m)}:${pad(s % 60)}`;
}

/** `3d 4h`, `4h 12m`, `12m`, `45s`. */
export function uptime(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return `${s}s`;
}

/** Local time as `14:02`. */
export function hhmm(d: Date): string {
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** The wake word line: what the detector is doing, without guessing why. */
export function wakeLine(s: Status): string {
  const phrase = s.wakeWord?.phrase;
  if (s.wakeWord?.enabled && phrase) return `Listening for "${phrase}"`;
  return "Not listening for the wake word";
}
