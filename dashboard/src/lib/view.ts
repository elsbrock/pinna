// What the page shows, derived from Status: words, tones, disabled reasons and
// ticking numbers. No Svelte and no runtime imports, so `node --test` runs it.
import type { Effect } from "../../../design/ring.ts";
import type { CalibrationEvent, Status, Timer, WakeEvent } from "../gen/pinna/v1/device_pb.ts";

/** The connection to the satellite's WatchStatus stream. */
export type Link = "connecting" | "live" | "offline";

// pinna.v1.Phase values (generated as a TS enum, which `node --test` cannot load).
const PHASE_WORDS: Record<number, "listening" | "thinking" | "speaking"> = {
  2: "listening",
  3: "thinking",
  4: "speaking",
};

/** Identify's test pattern, one effect per step, as pinnad's LED_TEST_EFFECTS. */
export const LED_TEST: readonly Effect[] = ["listening", "thinking", "speaking", "muted"];
export const LED_TEST_STEP_MS = 1_000;

/**
 * The ring effect, in the device's own order: the test pattern at `testStep`,
 * muted, a ringing alarm, the phase, a running timer's arc, then a waiting
 * notification.
 */
/** The satellite's name for people: the one Home Assistant shows (ADR 0007). */
export function displayName(s: Status): string {
  return s.settings?.friendlyName || s.name;
}

export function ringEffect(s: Status | null, link: Link, testStep = 0): Effect {
  if (!s || link !== "live") return "idle";
  if (s.ledTest) return LED_TEST[Math.min(testStep, LED_TEST.length - 1)];
  if (s.muted) return "muted";
  if (s.alarmRinging) return "timer-ringing";
  const phase = PHASE_WORDS[s.phase];
  if (phase) return phase;
  if (pairingOpen(s)) return "pairing";
  // The soonest running timer's arc shows above a waiting notification.
  if (s.timers.some((t) => t.active)) return "timer";
  if (s.notification) return "notification";
  return "idle";
}

/**
 * The Bluetooth pairing window is open (ADR 0011). The satellite sends a new
 * Status when it closes, so the seconds left when this one arrived will do.
 */
export function pairingOpen(s: Status): boolean {
  return (s.bluetooth?.pairingSecondsLeft ?? 0) > 0;
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

/** Why Calibrate is disabled, or null when it can be pressed. Mirrors pinnad. */
export function calibrationBlocked(s: Status | null, link: Link): string | null {
  if (!s || link !== "live") return "The satellite is not reachable.";
  if (s.muted) return "Turn the microphones on to calibrate.";
  if (!s.settings?.wakeWord) return "Turn the wake word on to calibrate.";
  if (s.alarmRinging) return "Stop the timer alarm first.";
  if (s.phase !== 1) return "A conversation is already running.";
  return null;
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
 * why. The reasons mirror pinnad's own gate (unmuted; idle or speaking with a
 * voice pipeline, or an alarm ringing), so they are facts, not guesses.
 */
export function wakeHint(s: Status): string {
  if (!s.settings?.wakeWord) return "Off. Press the action button to talk.";
  const phrase = s.wakeWord?.phrase;
  if (!phrase) return "No wake word is chosen.";
  const word = `"${phrase}"`;
  if (s.wakeWord?.enabled) return `Listening for ${word}`;
  if (s.muted) return `Not listening for ${word} while the microphones are off`;
  if (!s.haConnected || !s.voiceSubscribed)
    return `Not listening for ${word} until Home Assistant has a voice pipeline for this satellite`;
  if (s.phase !== 1 && s.phase !== 0) return `Not listening for ${word} during the conversation`;
  return `Not listening for ${word} yet`;
}

/**
 * The active wake words as they are said, each quoted: `“Alexa”` or
 * `“Alexa” or “Hey Jarvis”`; `the wake word` when none is known. For a
 * sentence, since Home Assistant shows the words as one phrase
 * ("Alexa or Hey Jarvis"), which reads wrong inside one.
 */
export function spokenWakeWords(s: Status): string {
  const words = (s.settings?.wakeWords ?? [])
    .map((id) => s.wakeWord?.available.find((w) => w.id === id)?.phrase)
    .filter((p): p is string => !!p);
  if (words.length > 0) return words.map((p) => `“${p}”`).join(" or ");
  return s.wakeWord?.phrase ? `“${s.wakeWord.phrase}”` : "the wake word";
}

/**
 * The wake word pickers' hint: who chooses, or why there is nothing to choose.
 * `undefined` when the dashboard may choose and the words are installed.
 */
export function wakeWordsHint(s: Status): string | undefined {
  if ((s.wakeWord?.available.length ?? 0) === 0)
    return "No wake word models are installed on this satellite.";
  if (s.haConnected) return "Home Assistant chooses the wake words while it is connected.";
  return undefined;
}

/** A near miss's suggestion sits this far above the lowest setting that catches it. */
const CATCH_MARGIN = 5;
/** A wake's suggestion sits this far below the highest setting that ignores it. */
const IGNORE_MARGIN = 3;

/** The score a sensitivity needs: pinna-wakeword's `threshold_for_sensitivity`, keep in step. */
export function thresholdFor(sensitivity: number): number {
  return Math.min(0.85, Math.max(0.3, 0.85 - sensitivity * 0.0044));
}

/**
 * A score at two decimals, rounded away from the threshold: up for a score
 * that reached it, down for one that didn't. So `0.5508` needed and a near
 * miss at `0.5499` read 0.56 and 0.54, not 0.55 and 0.55.
 */
export function scoreText(score: number, reached: boolean): string {
  const hundredths = reached ? Math.ceil(score * 100 - 1e-4) : Math.floor(score * 100 + 1e-4);
  return (hundredths / 100).toFixed(2);
}

export type WakeAdvice = {
  /** What the score means at `sensitivity`. */
  text: string;
  /** The sensitivity that fixes it, when one does and it isn't set yet. */
  set?: number;
};

type WakeLike = Pick<WakeEvent, "woke" | "score" | "wakesFrom" | "reachedThreshold">;

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/**
 * What a wake or near miss says about the sensitivity: the setting that would
 * have ignored a wake or caught a near miss, with a small margin, or why no
 * setting would. `all`: the whole list, to say what else that setting would
 * ignore or catch.
 */
export function wakeAdvice(e: WakeLike, sensitivity: number, all: readonly WakeLike[] = []): WakeAdvice {
  const score = `Score ${scoreText(e.score, e.woke || e.reachedThreshold)}`;
  const from = e.wakesFrom;
  const others = all.filter((o) => o !== e && o.wakesFrom !== undefined);
  if (e.woke) {
    if (!from)
      return {
        text: `${score}: every setting wakes on this. If it wasn't you, move the satellite away from the TV or the radio.`,
      };
    if (sensitivity < from) return { text: `${score}. Ignored at this setting.` };
    const set = Math.max(0, from - 1 - IGNORE_MARGIN);
    // Other wakes that still wake now and would not at `set`.
    const lost = others.filter((o) => o.woke && o.wakesFrom! > set && o.wakesFrom! <= sensitivity).length;
    const cost = lost ? ` But it also ignores ${plural(lost, "other wake", "other wakes")} listed here.` : "";
    return { text: `${score}. If it wasn't you, sensitivity ${set} ignores it.${cost}`, set };
  }
  if (e.reachedThreshold)
    return {
      text: `${score}: high enough at the time, but too quiet against the background or too short to count. Speak closer or louder, with less noise around.`,
    };
  if (from === undefined) return { text: `${score}: too faint for any setting. Speak closer, or with less noise around.` };
  if (sensitivity >= from) return { text: `${score}. Wakes at this setting.` };
  const set = Math.min(100, from + CATCH_MARGIN);
  // Other near misses that miss now and would wake at `set`.
  const gained = others.filter(
    (o) => !o.woke && !o.reachedThreshold && o.wakesFrom! <= set && o.wakesFrom! > sensitivity,
  ).length;
  const also = gained ? ` It also catches ${plural(gained, "other near miss", "other near misses")} listed here.` : "";
  return { text: `${score}. If it was you, sensitivity ${set} catches it.${also}`, set };
}

/** pinna.v1.CalibrationEventKind values (a generated TS enum, which `node --test` cannot load). */
export const CAL_KIND = { woke: 1, raised: 2, lowered: 3, nothingHeard: 4, noSettingHelps: 5, wouldUndo: 6 } as const;

type CalibrationLike = Pick<CalibrationEvent, "kind" | "raise" | "score" | "from" | "to">;

/** How long a + or − press looks back at what was heard (pinnad's own window). */
const FEEDBACK_SECONDS = 15;

/**
 * One event during calibration, in words: what the wake or the + / − press did,
 * or why the device changed nothing.
 */
export function calibrationAdvice(e: CalibrationLike): { label: string; text: string } {
  const nothing = `${e.raise ? "+" : "−"} did nothing`;
  const score = e.score ?? 0;
  switch (e.kind) {
    case CAL_KIND.woke:
      return {
        label: "Heard the wake word",
        text: `Score ${scoreText(score, true)}. No request started.`,
      };
    case CAL_KIND.raised:
      return {
        label: "+ raised the sensitivity",
        text: `${e.from} → ${e.to}, to catch a near miss at ${scoreText(score, false)}.`,
      };
    case CAL_KIND.lowered:
      return {
        label: "− lowered the sensitivity",
        text: `${e.from} → ${e.to}, to ignore a wake at ${scoreText(score, true)}.`,
      };
    case CAL_KIND.nothingHeard:
      return { label: nothing, text: `Nothing was heard in the last ${FEEDBACK_SECONDS} seconds.` };
    case CAL_KIND.noSettingHelps:
      if (!e.raise)
        return {
          label: nothing,
          text: `Every setting wakes on a score of ${scoreText(score, true)}. Move the satellite away from the sound, or turn off the wake word while it plays.`,
        };
      if (score < thresholdFor(e.from))
        return {
          label: nothing,
          text: `Score ${scoreText(score, false)} is too faint for any setting. Speak closer, or with less noise around.`,
        };
      return {
        label: nothing,
        text: `Score ${scoreText(score, true)} was enough at the time, but the device rejected it for being quiet against the background or too short. Speak closer or louder, with less noise around.`,
      };
    case CAL_KIND.wouldUndo:
      return {
        label: nothing,
        text: e.raise
          ? "It would let back the false wake you ignored. Your wake and that one score about the same; no setting settles both. Move the satellite away from the TV or the radio."
          : "It would lose the wake you caught. That wake and the false one score about the same; no setting settles both. Move the satellite away from the TV or the radio.",
      };
    default:
      return { label: "Calibration", text: "An event the dashboard doesn't know." };
  }
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** Local time as `14:02` today, `Yesterday 14:02`, or `12 Nov 14:02`. */
export function when(unixMs: number, now: Date): string {
  const d = new Date(unixMs);
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(now) - day(d)) / 86_400_000);
  if (days === 0) return hhmm(d);
  if (days === 1) return `Yesterday ${hhmm(d)}`;
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${hhmm(d)}`;
}

/** The project's documentation (the dashboard works offline; these are plain links). */
export const DOCS = "https://elsbrock.github.io/pinna/docs/";

// pinna.v1.WifiSecurity and JoinStage values (TS enums node --test can't load).
const SECURITY_WORDS: Record<number, string> = {
  1: "Open",
  2: "WPA2",
  3: "WPA3 only",
  4: "Enterprise",
  5: "WEP",
};
const JOIN_TRIAL = 3;
const JOIN_JOINED = 4;
const JOIN_FAILED = 5;

/** A network's security as people know it. */
export function securityWord(security: number): string {
  return SECURITY_WORDS[security] ?? "Unknown";
}

/** Why the satellite can't join a network with this security; null when it can. */
export function unjoinable(security: number): string | null {
  switch (security) {
    case 1:
    case 2:
      return null;
    case 3:
      return "WPA3 only: this satellite's Wi-Fi can't join it.";
    case 4:
      return "Enterprise networks aren't supported.";
    case 5:
      return "WEP isn't supported: it isn't safe.";
    default:
      return "Unknown security.";
  }
}

/** The last step a JoinWifi stream sends. */
export function joinDone(step: { stage: number }): boolean {
  return step.stage === JOIN_TRIAL || step.stage === JOIN_JOINED || step.stage === JOIN_FAILED;
}

/** How a join ended, for a notice: its tone, or null while it runs. */
export function joinTone(step: { stage: number } | null | undefined): "ok" | "caution" | null {
  if (!step) return null;
  if (step.stage === JOIN_JOINED) return "ok";
  if (step.stage === JOIN_FAILED) return "caution";
  return null;
}

/** Minutes and seconds, "4:05". */
export function mmss(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

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
      body: "Choose your network under Join a network, below.",
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
  // Doesn't boot next: an installed update was kept back.
  if (s.state === "trial" && !s.active && !s.booted) return "Not started yet";
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

/** The slot a switch restarts into: the other one, while nothing else waits,
 *  when its signed image is good or hasn't been started yet. */
export function switchTarget(u: UpdatesLike): SlotLike | undefined {
  if (updatePhase(u) !== "ready") return undefined;
  const s = targetSlot(u);
  return s?.version && s.state !== "unbootable" ? s : undefined;
}

/** The dashboard's sections and pages: hash paths, as the router shows them. */
export const PATHS = [
  "/",
  "/voice",
  "/voice/wake-word",
  "/voice/media-sources",
  "/home-assistant",
  "/network",
  "/system",
  "/system/logs",
] as const;
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
