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

/**
 * The ring effect, in the device's own order: muted, a ringing alarm, the phase,
 * then a waiting notification unless a running timer's arc shows above it.
 */
export function ringEffect(s: Status | null, link: Link): Effect {
  if (!s || link !== "live") return "idle";
  if (s.muted) return "muted";
  if (s.alarmRinging) return "timer-ringing";
  const phase = PHASE_WORDS[s.phase];
  if (phase) return phase;
  if (s.notification && !s.timers.some((t) => t.active)) return "notification";
  return "idle";
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
export function health(s: Status | null, link: Link, offlineSince: Date | null, restarting = false): Health {
  if (restarting && link !== "live") return { tone: "caution", text: "Restarting" };
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

/**
 * The Wake word switch's hint: what the detector does and, when it is paused,
 * why. The reasons mirror pinnad's own gate (idle, unmuted, a voice pipeline,
 * no alarm), so they are facts, not guesses.
 */
export function wakeHint(s: Status): string {
  const phrase = s.wakeWord?.phrase ? `"${s.wakeWord.phrase}"` : "the wake word";
  if (!s.settings?.wakeWord) return "Off. Press the action button to talk.";
  if (s.wakeWord?.enabled) return `Listening for ${phrase}`;
  if (s.muted) return `Not listening for ${phrase} while the microphones are off`;
  if (!s.haConnected || !s.voiceSubscribed)
    return `Not listening for ${phrase} until Home Assistant has a voice pipeline for this satellite`;
  if (s.alarmRinging) return `Not listening for ${phrase} while the alarm rings`;
  if (s.phase !== 1 && s.phase !== 0) return `Not listening for ${phrase} during the conversation`;
  return `Not listening for ${phrase} yet`;
}

/** The project's documentation (the dashboard works offline; these are plain links). */
export const DOCS = "https://elsbrock.github.io/pinna/docs/";

/** Wi-Fi signal as a word people use: thresholds as common for dBm. */
export function signalWord(dbm: number): string {
  if (dbm >= -55) return "Excellent";
  if (dbm >= -67) return "Good";
  if (dbm >= -75) return "Fair";
  return "Weak";
}

export function band(mhz: number): string {
  if (!mhz) return "";
  if (mhz < 3000) return "2.4 GHz";
  if (mhz < 5925) return "5 GHz";
  return "6 GHz";
}

/** What an interface is, in words: the satellite has Wi-Fi and the USB console. */
export function interfaceLabel(name: string, wireless: boolean): string {
  if (wireless) return "Wi-Fi";
  if (name.startsWith("usb")) return "USB (console)";
  if (/^(eth|en)/.test(name)) return "Ethernet";
  return name;
}

export type NetIssue = { title: string; body: string; href: string };

/**
 * What is wrong with the Wi-Fi, in the cases the troubleshooting page names.
 * `wifi` is the wireless interface, if there is one.
 */
export function wifiIssue(
  wifi: { wifi?: { ssid: string }; addresses: string[] } | undefined,
): NetIssue | null {
  const doc = `${DOCS}troubleshooting/#the-device-has-no-wi-fi`;
  if (!wifi)
    return {
      title: "No Wi-Fi interface",
      body: "The Wi-Fi firmware is probably missing from the image.",
      href: doc,
    };
  const v4 = wifi.addresses.some((a) => !a.includes(":"));
  // No link details (iw missing) but an address: it is connected.
  if (!wifi.wifi && v4) return null;
  if (!wifi.wifi)
    return {
      title: "Not connected to a Wi-Fi network",
      body: "Check the network name and password in /var/lib/pinna/wifi/wpa_supplicant-wlan0.conf.",
      href: doc,
    };
  if (!v4)
    return {
      title: `Connected to ${wifi.wifi?.ssid}, but no IPv4 address yet`,
      body: "The network's DHCP server hasn't answered. This usually sorts itself out within a minute.",
      href: doc,
    };
  return null;
}

type SlotLike = { name: string; version: string; state: string; triesLeft: number; active: boolean; booted: boolean };
type UpdatesLike = { available: boolean; installing: boolean; slots: SlotLike[] };

/** A slot's state in words. */
export function slotWord(s: SlotLike): string {
  if (s.state === "good") return "Good";
  if (s.state === "trial") return `On trial, ${s.triesLeft} ${s.triesLeft === 1 ? "try" : "tries"} left`;
  return "Can't boot";
}

/**
 * Where an update stands, from the A/B slots alone (so a reloaded page, or one
 * opened after a restart, knows too).
 *   ready     install is possible
 *   trial     this version runs on trial: installing waits until it is marked good
 *   pending   another slot boots next: restart to try it
 *   installing
 *   unavailable
 */
export type UpdatePhase = "unavailable" | "installing" | "trial" | "pending" | "ready";

export function updatePhase(u: UpdatesLike): UpdatePhase {
  if (!u.available) return "unavailable";
  if (u.installing) return "installing";
  const booted = u.slots.find((s) => s.booted);
  const active = u.slots.find((s) => s.active);
  if (booted?.state === "trial") return "trial";
  if (active && booted && active.name !== booted.name && active.state !== "unbootable") return "pending";
  return "ready";
}

/** The slot an install writes: the one that is not running. */
export function targetSlot(u: UpdatesLike): SlotLike | undefined {
  return u.slots.find((s) => !s.booted);
}

/** The dashboard's sections and pages: hash paths, as the router shows them. */
export const PATHS = ["/", "/voice", "/home-assistant", "/network", "/system", "/system/logs"] as const;
export type Path = (typeof PATHS)[number];

// v1 addresses that people may have bookmarked.
const MOVED: Record<string, Path> = { "/settings": "/voice", "/updates": "/system", "/logs": "/system/logs" };

/** The page a location hash opens: itself, where it moved, or null for none. */
export function routeOf(hash: string): Path | null {
  const p = hash.replace(/^#/, "") || "/";
  if ((PATHS as readonly string[]).includes(p)) return p as Path;
  return MOVED[p] ?? null;
}

/** pinna.v1.InstallStage values (a generated TS enum, which `node --test` cannot load). */
export const STAGE = { check: 1, download: 2, write: 3, verify: 4, installed: 5 } as const;

export type StepState = "done" | "current" | "pending" | "failed";
export type Step = { label: string; state: StepState };

const STEP_LABELS = ["Check the release", "Download and write", "Verify", "Restart into the new version"];

/**
 * The install as steps, from the furthest stage its lines reached and how it
 * ended (null while it runs). A failed install fails the step it was on.
 */
export function installSteps(stage: number, finished: { ok: boolean } | null): Step[] {
  let at: number;
  if (finished?.ok || stage >= STAGE.installed) at = 3;
  else if (stage >= STAGE.write) at = 2;
  else if (stage >= STAGE.check) at = 1;
  else at = 0;
  const failed = finished !== null && !finished.ok;
  return STEP_LABELS.map((label, i) => ({
    label,
    state: i < at ? "done" : i > at ? "pending" : failed ? "failed" : "current",
  }));
}

/** `  84.2 MiB of 212.4 MiB` from pinnactl update, as text and a fraction. */
export function downloadProgress(line: string): { text: string; fraction: number } | null {
  const m = /^\s+([\d.]+) MiB of ([\d.]+) MiB\s*$/.exec(line);
  if (!m) return null;
  const done = Number(m[1]);
  const total = Number(m[2]);
  return { text: `${m[1]} of ${m[2]} MiB`, fraction: total > 0 ? Math.min(1, done / total) : 0 };
}

/** When the satellite started, as `Thu 09:14`, from now and its uptime. */
export function since(now: Date, uptimeSeconds: number): string {
  const d = new Date(now.getTime() - uptimeSeconds * 1000);
  return `${d.toLocaleDateString("en", { weekday: "short" })} ${hhmm(d)}`;
}

/** The Microphones switch's hint when the hardware button is involved. */
export function micHint(s: Status): string | undefined {
  if (!s.hardwareMuted) return undefined;
  return s.softwareMuted
    ? "Off with the microphone button on the satellite, and in software. Press the button, then turn them on here."
    : "Off with the microphone button on the satellite. Press it there to turn them on.";
}

/** The health line in a word or two, for the phone's app bar. */
export function healthWord(h: Health): string {
  if (h.tone === "ok") return "Connected";
  if (h.tone === "danger") return "Not reachable";
  return h.text === "Restarting" ? "Restarting" : "Check Home Assistant";
}

/** `433.3 MBit/s VHT-MCS 9 80MHz short GI` from iw, as `433 Mbit/s`. */
export function bitrate(tx: string): string {
  const m = /^([\d.]+)\s*MBit\/s/i.exec(tx);
  return m ? `${Math.round(Number(m[1]))} Mbit/s` : tx;
}

/** Connected to Wi-Fi: link details, or (without them) an IPv4 address. wifiIssue() agrees. */
export function wifiConnected(w: { wifi?: unknown; addresses: string[] } | undefined): boolean {
  return !!w && (!!w.wifi || w.addresses.some((a) => !a.includes(":")));
}
