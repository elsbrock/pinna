// What the page shows, derived from Status: words, tones, disabled reasons and
// ticking numbers. No Svelte and no runtime imports, so `node --test` runs it.
import type { Effect } from "../../../design/ring.ts";
import type {
  CalibrationEvent,
  Satellite,
  Security,
  Status,
  Timer,
  WakeEvent,
  WakeWordOption,
} from "../gen/pinna/v1/device_pb.ts";
import { isStandalone } from "./assistant.ts";

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
/** `word`: the line in a word or two, where healthWord's wouldn't fit. */
export type Health = { tone: Tone; text: string; word?: string };

/** The health line: of Home Assistant, or in standalone mode of its services. */
export function health(s: Status | null, link: Link, offlineSince: Date | null, restarting = false): Health {
  if (restarting && link !== "live") return { tone: "caution", text: "Restarting" };
  if (link === "offline" || !s) {
    const since = offlineSince ? ` since ${hhmm(offlineSince)}` : "";
    return { tone: "danger", text: `Not reachable${since}` };
  }
  if (isStandalone(s)) {
    if (!s.assistant?.ready) return { tone: "caution", text: "Standalone, services not ready", word: "Not ready" };
    return { tone: "ok", text: "Answers in standalone mode", word: "Standalone" };
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
  if (isStandalone(s)) {
    if (!s.assistant?.ready) return "Set up the standalone services under Assistant to start a conversation.";
  } else if (!s.haConnected) return "Connect Home Assistant, or switch to standalone mode, to start a conversation.";
  else if (!s.voiceSubscribed) return "Home Assistant has no voice pipeline for this satellite yet.";
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
 * why. The reasons mirror pinnad's own gate (unmuted; not listening to a
 * request; with a voice pipeline, or in standalone mode with its services
 * ready; or an alarm ringing), so they are facts, not guesses.
 */
export function wakeHint(s: Status): string {
  if (!s.settings?.wakeWord) return "Off. Press the action button to talk.";
  const phrase = s.wakeWord?.phrase;
  if (!phrase) return "No wake word is chosen.";
  const word = `"${phrase}"`;
  if (s.wakeWord?.enabled) return `Listening for ${word}`;
  if (s.muted) return `Not listening for ${word} while the microphones are off`;
  if (isStandalone(s)) {
    if (!s.assistant?.ready) return `Not listening for ${word} until the standalone services pass their check`;
  } else if (!s.haConnected || !s.voiceSubscribed)
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
 * `undefined` when the dashboard may choose and the words are installed: in
 * standalone mode it always may (ADR 0013, ADR 0017).
 */
export function wakeWordsHint(s: Status): string | undefined {
  if ((s.wakeWord?.available.length ?? 0) === 0)
    return "No wake word models are installed on this satellite.";
  if (s.haConnected && !isStandalone(s)) return "Home Assistant chooses the wake words while it is connected.";
  return undefined;
}

/** Home Assistant, in Home Assistant mode and connected, chooses the wake words. */
export function haChooses(s: Status): boolean {
  return s.haConnected && !isStandalone(s);
}

/** Custom wake words: at most this many, each model at most this large (ADR 0013). */
export const MAX_CUSTOM_WORDS = 8;
export const MAX_MODEL_BYTES = 2 * 1024 * 1024;
export const PHRASE_MIN = 2;
export const PHRASE_MAX = 40;

export function customWordCount(s: Status): number {
  return (s.wakeWord?.available ?? []).filter((w) => w.custom).length;
}

/** `1.4 MiB`, `120 KiB`, `800 bytes`. */
export function fileSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KiB`;
  return `${bytes} bytes`;
}

/** Why a phrase can't be added, or null. Counts characters, not UTF-16 units. */
export function phraseProblem(phrase: string): string | null {
  const n = [...phrase.trim()].length;
  if (n < PHRASE_MIN || n > PHRASE_MAX) return `The phrase must be ${PHRASE_MIN} to ${PHRASE_MAX} characters.`;
  return null;
}

/** Why a model file can't be added, or null. The satellite checks again. */
export function modelFileProblem(name: string, size: number): string | null {
  if (!/\.(onnx|tflite)$/i.test(name)) return "The model must be an openWakeWord file, .onnx or .tflite.";
  if (size === 0) return "The file is empty.";
  if (size > MAX_MODEL_BYTES) return `The file is ${fileSize(size)}. A model can be at most 2 MiB.`;
  return null;
}

/** Whether the satellite listens for this word now. */
export function wordChosen(s: Status, id: string): boolean {
  return (s.settings?.wakeWords ?? []).includes(id);
}

/** A word's hint on the Wake words page: its error, or where it came from and whether it listens. */
export function wordHint(s: Status, w: Pick<WakeWordOption, "id" | "custom" | "error">): string {
  if (w.error) return w.error;
  const origin = w.custom ? "Added on this satellite" : "Built in";
  // Chosen, not "listening": whether the satellite listens right now is the
  // Wake word switch's hint (wakeHint), and depends on more than the choice.
  return wordChosen(s, w.id) ? `${origin}. Chosen.` : `${origin}.`;
}

/** What Remove does, one sentence each, for the Confirm. */
export function removeWordEffects(s: Status, id: string): string[] {
  const out = ["It deletes the model from this satellite."];
  if (!wordChosen(s, id)) return out;
  out.push("It is chosen, so the satellite stops listening for it.");
  if ((s.settings?.wakeWords ?? []).length === 1) out.push("The satellite then has no wake word.");
  // Only Home Assistant can choose the replacement then (ADR 0013).
  if (haChooses(s))
    out.push("Home Assistant is connected, so it chooses the wake words: choose another one there.");
  return out;
}

/** What to do after a word was added. */
export function addedNextStep(s: Status | null): string {
  const then = "Then say it and watch the recent wakes under Sensitivity.";
  if (s && haChooses(s))
    return `Choose it on this satellite's device page in Home Assistant. It appears there once the satellite is idle. ${then}`;
  return `Choose it under Wake word on the Voice & sound page. ${then}`;
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
export const DOCS = "https://getpinna.dev/docs/";

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

/** What a window has left (ADR 0029), "14 min left" or "40 s left"; empty once it ended. */
export function windowLeft(seconds: number): string {
  const s = Math.max(0, Math.ceil(seconds));
  if (s === 0) return "";
  return s >= 60 ? `${Math.ceil(s / 60)} min left` : `${s} s left`;
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
  // The setup hotspot's interface (ADR 0028).
  if (name === "ap0") return "Setup hotspot";
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

type SlotLike = {
  name: string;
  version: string;
  state: string;
  triesLeft: number;
  active: boolean;
  booted: boolean;
  olderThanRunning?: boolean;
};
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

/** Said before the satellite starts an older version while a password is set (ADR 0029):
 *  an image from before the password doesn't check it, so the dashboard is open until the next update. */
export const OLDER_SLOT_WARNING =
  "The version in the other slot is older, and may not ask for a password: until the next update, anyone on your network could use the dashboard, /mcp and the metrics.";

/** The warning for starting `slot`, or null: only with a password set, and only for an older version. */
export function olderSlotWarning(passwordSet: boolean, slot: Pick<SlotLike, "olderThanRunning"> | undefined): string | null {
  return passwordSet && slot?.olderThanRunning ? OLDER_SLOT_WARNING : null;
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
  "/voice/wake-words",
  "/voice/media-sources",
  "/voice/ring",
  "/assistant",
  "/assistant/services",
  "/assistant/conversation",
  "/assistant/tools",
  "/network",
  "/system",
  "/system/logs",
] as const;
export type Path = (typeof PATHS)[number];

// Addresses that people may have bookmarked.
const MOVED: Record<string, Path> = {
  "/settings": "/voice",
  "/updates": "/system",
  "/logs": "/system/logs",
  "/home-assistant": "/assistant",
};

/** The page a location hash opens: itself, where it moved, or null for none. */
export function routeOf(hash: string): Path | null {
  const p = hash.replace(/^#/, "") || "/";
  if ((PATHS as readonly string[]).includes(p)) return p as Path;
  return MOVED[p] ?? null;
}

/** pinna.v1.ReleaseAction values: what Install does with the release a check found (ADR 0022). */
export const ACTION = {
  install: 1,
  upToDate: 2,
  older: 3,
  start: 4,
  restart: 5,
  failed: 6,
  blocked: 7,
  otherModel: 8,
} as const;

/** pinna.v1.UpdateChannel values, by the setting's names. */
export const CHANNEL = { stable: 1, beta: 2, off: 3 } as const;
export type ChannelName = keyof typeof CHANNEL;

/** The channel's name for a pinna.v1.UpdateChannel value. */
export function channelName(value: number | undefined): ChannelName {
  return (Object.keys(CHANNEL) as ChannelName[]).find((k) => CHANNEL[k] === value) ?? "off";
}

/** pinna.v1.InstallUpdates values, by the setting's names. */
export const INSTALL = { manually: 1, automatically: 2 } as const;
export type InstallName = keyof typeof INSTALL;

/** The "Install updates" choice for a pinna.v1.InstallUpdates value; manually until set. */
export function installName(value: number | undefined): InstallName {
  return (Object.keys(INSTALL) as InstallName[]).find((k) => INSTALL[k] === value) ?? "manually";
}

type CheckLike = { checking: boolean; finishedUnixMs: bigint; error: string; noRelease: boolean };

/** Where the update checks stand, in one sentence. */
export function checkStatus(check: CheckLike | undefined, off: boolean, now: Date): string {
  if (off) return "Off: the satellite doesn't look for releases.";
  if (check?.checking) return "Checking…";
  if (!check || check.finishedUnixMs === 0n) return "Not checked yet since the satellite started.";
  const at = new Date(Number(check.finishedUnixMs));
  const day = at.toDateString() === now.toDateString() ? "at" : at.toLocaleDateString("en", { weekday: "short" });
  return `${check.error ? "Last check failed" : "Checked"} ${day} ${hhmm(at)}.`;
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

/** The Overview's count of the satellite group's held messages (ADR 0026);
 *  null when none waits. They play once the satellite is idle and its
 *  microphones are on: only the microphones are the owner's to change, so
 *  only they are named, and only while they are off. */
export function heldText(count: number, muted: boolean): string | null {
  if (count <= 0) return null;
  const held = count === 1 ? "1 message held" : `${count} messages held`;
  if (!muted) return held;
  return `${held} · ${count === 1 ? "it plays" : "they play"} once the microphones are on`;
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
  if (h.word) return h.word;
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

/** Where a satellite's dashboard opens: the link and the host to show with it. */
export type SatLink = { href: string; host: string };
export type SatelliteLike = Pick<Satellite, "name" | "host" | "board" | "webPort" | "url" | "ipv4" | "ipv6">;

const port = (sat: SatelliteLike) => (sat.webPort && sat.webPort !== 80 ? `:${sat.webPort}` : "");
const link = (host: string): SatLink => ({ href: `http://${host}/`, host });
// The host its SRV record names: after a name conflict the instance name may not resolve (ADR 0024).
const localLink = (sat: SatelliteLike) => link(`${encodeURIComponent(sat.host || sat.name)}.local${port(sat)}`);

/** The address another satellite announces for its dashboard, when it is a plain
 *  http(s) one: the record comes from the network, so nothing else becomes a link. */
function announced(url: string): SatLink | null {
  try {
    const u = new URL(url);
    if ((u.protocol !== "http:" && u.protocol !== "https:") || u.username || u.password) return null;
    return { href: u.href, host: u.host };
  } catch {
    return null;
  }
}

/**
 * The link to another satellite's dashboard (ADR 0021): its own address when
 * it has one; else its `.local` name when this page was opened by one; else
 * its address of the family this page was opened by (IPv6 literal or IPv4),
 * falling back to the `.local` name when it has none of that family.
 */
export function satelliteLink(sat: SatelliteLike, here: { protocol: string; hostname: string }): SatLink {
  const own = announced(sat.url);
  if (own) return own;
  const name = here.hostname.toLowerCase().replace(/\.$/, "");
  if (name.endsWith(".local")) return localLink(sat);
  if (name.startsWith("[") || name.includes(":")) {
    // A global address before a unique-local one (fc00::/7); never link-local.
    const v6 = sat.ipv6.find((a) => !/^f[cd]/i.test(a)) ?? sat.ipv6[0];
    return v6 ? link(`[${v6}]${port(sat)}`) : localLink(sat);
  }
  const v4 = sat.ipv4[0];
  return v4 ? link(`${v4}${port(sat)}`) : localLink(sat);
}

/** Ways around a dashboard address that doesn't open: the `.local` name and the first IPv4 address. */
export function satelliteFallbacks(sat: SatelliteLike): SatLink[] {
  const v4 = sat.ipv4[0];
  return [localLink(sat), ...(v4 ? [link(`${v4}${port(sat)}`)] : [])];
}

/** What the satellite list says of an entry (ADR 0029): a member of this satellite's group is
 *  confirmed by the group's key; any other entry is only what the network announced. */
export function satelliteMark(sat: Pick<Satellite, "groupMember">): string {
  return sat.groupMember ? "In this satellite's group" : "Only announced on the network";
}

/** The drawing's model id for a satellite's `board` (see `identity::model_name`). */
export function satelliteModel(board: string): string {
  return board === "Echo (2nd Gen)" ? "echo-2" : "echo-dot-2";
}

// A stream's source as its header names it (ADR 0011), or `sendspin`, which
// pinnad plays itself (ADR 0027). A Map: the name comes from the network, and
// an object would answer for "constructor".
const STREAM_SOURCES = new Map([
  ["airplay", "AirPlay"],
  ["bluetooth", "Bluetooth"],
  ["sendspin", "Music Assistant"],
]);

/**
 * Where the media on Now playing comes from: a stream's source (AirPlay,
 * Bluetooth and Sendspin by their names, another program by the name its
 * header gives), a radio station, or Home Assistant, which plays everything
 * through its ffmpeg proxy (ADR 0011). Otherwise the address itself, shown as
 * code (`url`).
 */
export function mediaFrom(m: { stream: boolean; source: string; station: string; url: string }): {
  text: string;
  url: boolean;
} {
  // A paused Sendspin group streams nothing, yet keeps its source (ADR 0027 §14).
  const known = STREAM_SOURCES.get(m.source);
  if (known) return { text: `From ${known}`, url: false };
  // A stream with no source came through the stream socket without a source header.
  // Sendspin never uses the socket and always names itself (ADR 0027 §13).
  if (m.stream) return { text: `From ${m.source || "AirPlay or Bluetooth"}`, url: false };
  if (m.station) return { text: `From ${m.station}`, url: false };
  if (m.url.includes("/api/esphome/ffmpeg_proxy/")) return { text: "From Home Assistant", url: false };
  return { text: m.url, url: true };
}

// pinna.v1.MediaState.ANNOUNCING (a generated TS enum, which `node --test` cannot load).
const ANNOUNCING = 4;

/**
 * Now playing offers Pause and Resume. An announcement can't pause, and nor
 * can AirPlay or Bluetooth: pausing ends their session, as Stop does.
 * Sendspin's pause and play go to its group, which waits paused (ADR 0027 §14).
 */
export function mediaPauses(m: { state: number; stream: boolean; source: string }): boolean {
  return m.state !== ANNOUNCING && (!m.stream || m.source === "sendspin");
}

/** The line under a track's title: its artist and album, where known. */
export function byline(m: { artist: string; album: string }): string {
  return [m.artist, m.album].filter(Boolean).join(" · ");
}

/**
 * An address as Now playing shows it: the server and the file, without the
 * folders between them or the query, which can hold a stream's token. The
 * full address is the element's title.
 */
export function shortUrl(url: string): string {
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return url;
  }
  const parts = u.pathname.split("/").filter(Boolean);
  const last = parts.at(-1);
  if (!last) return u.host;
  let file = last;
  try {
    file = decodeURIComponent(last);
  } catch {
    // Not percent-encoded as it should be: show it as it is.
  }
  return parts.length > 1 ? `${u.host}/…/${file}` : `${u.host}/${file}`;
}

// The dashboard password (ADR 0029).

/** What the page shows first: the offer of a password, the sign-in form, or the app.
 *  Until the security state arrives, the app, which shows its own connecting state. */
export type Gate = "offer" | "signin" | "app";

export function gate(sec: Security | null): Gate {
  if (!sec) return "app";
  if (sec.offerPending) return "offer";
  if (sec.passwordSet && !sec.signedIn) return "signin";
  return "app";
}

export const PASSWORD_MIN = 8;
export const PASSWORD_MAX = 128;

/** Why a new password won't do, or null. The satellite counts characters, not bytes. */
export function passwordProblem(password: string): string | null {
  const n = [...password].length;
  return n < PASSWORD_MIN || n > PASSWORD_MAX ? `Use ${PASSWORD_MIN} to ${PASSWORD_MAX} characters.` : null;
}

/** POST /auth/login's answer: signed in, or the server's sentence and how long to wait. */
export type LoginOutcome = { ok: true } | { ok: false; message: string; wait: number; gone?: true };

export function loginOutcome(status: number, body: unknown): LoginOutcome {
  if (status === 204) return { ok: true };
  const b = (body ?? {}) as { error?: unknown; retry_after_s?: unknown };
  const message = typeof b.error === "string" && b.error ? b.error : "The satellite refused the sign-in. Try again.";
  const wait = status === 429 && typeof b.retry_after_s === "number" ? Math.max(1, Math.ceil(b.retry_after_s)) : 0;
  // 409: the password was removed since this page loaded. There is nothing to sign in to.
  if (status === 409) return { ok: false, message, wait, gone: true };
  return { ok: false, message, wait };
}

/** A sentence that ends in "Try again in 12 s.": the sentence without it, and the seconds.
 *  The page counts the seconds down itself, so it shows one countdown, not a frozen copy as well. */
export function splitRetry(sentence: string): { text: string; wait: number } {
  const m = /\s*Try again in (\d+) s\.$/.exec(sentence);
  if (!m) return { text: sentence, wait: 0 };
  return { text: sentence.slice(0, m.index), wait: Number(m[1]) };
}

/** Whole seconds until `until` (Date.now() milliseconds), never below 0. */
export function secondsUntil(until: number, now: number): number {
  return Math.max(0, Math.ceil((until - now) / 1000));
}

export function secondsWord(n: number): string {
  return n === 1 ? "1 second" : `${n} seconds`;
}

/** How long ago, in the largest unit that fits: "just now", "5 minutes ago". */
export function ago(thenMs: number, nowMs: number): string {
  const s = Math.max(0, Math.round((nowMs - thenMs) / 1000));
  if (s < 60) return "just now";
  const unit = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"} ago`;
  if (s < 3_600) return unit(Math.floor(s / 60), "minute");
  if (s < 86_400) return unit(Math.floor(s / 3_600), "hour");
  return unit(Math.floor(s / 86_400), "day");
}

/** The System page's line about failed sign-ins since the last one. */
export function failuresText(sec: Security, nowMs: number): string {
  const n = sec.failuresSinceLogin;
  if (n === 0) return "None since the last sign-in.";
  const count = n === 1 ? "1 failed sign-in" : `${n} failed sign-ins`;
  const when = sec.lastFailureUnixMs ? Number(sec.lastFailureUnixMs) : 0;
  const from = sec.lastFailureAddress ? `from ${sec.lastFailureAddress}` : "";
  const last = [from, when ? ago(when, nowMs) : ""].filter(Boolean).join(", ");
  return last ? `${count} since the last sign-in. The last was ${last}.` : `${count} since the last sign-in.`;
}

/** A note on a new password that is allowed but may not be meant: spaces at either end. */
export function passwordNote(password: string): string | null {
  return password !== password.trim() ? "It starts or ends with a space." : null;
}

/** How long a grant from a confirmation stays good, from when this page got it. */
export const GRANT_MS = 5 * 60 * 1000;

/** Seconds a grant got at `at` (Date.now()) is still good; 0 once it lapsed. */
export function grantLeft(at: number, now: number): number {
  return secondsUntil(at + GRANT_MS, now);
}

/** What the page tells after it reloads with a new password in place. */
export type PasswordNotice = "set" | "changed";
export const PASSWORD_NOTICES: Record<PasswordNotice, string> = {
  set: "Password set.",
  changed: "Password changed. Every other browser is signed out.",
};

/** The Overview's warning about a confirmation someone else started; null when none waits
 *  or this browser started it. */
export function confirmationWarning(
  sec: Security | null,
  startedHere: boolean,
): { from: string; left: number } | null {
  if (!sec?.signedIn || startedHere || sec.confirmationLeftS <= 0) return null;
  return { from: sec.confirmationWaitingFrom, left: sec.confirmationLeftS };
}

/** This tab was signed in and now isn't, with a password still set: the password changed or
 *  every session ended. A tab that never had a session says nothing. */
export function lostSession(before: Security | null, after: Security): boolean {
  return !!before?.signedIn && after.passwordSet && !after.signedIn;
}

export const SIGNED_OUT = "You were signed out: the password was changed, or every session was ended.";

/** Home Assistant can't add this satellite until the owner opens the window (ADR 0029). */
export function homeAssistantBlocked(
  sec: Pick<Security, "passwordSet"> | null,
  status: { apiEncrypted: boolean; haConnected: boolean } | null | undefined,
): boolean {
  return !!sec?.passwordSet && !!status && !status.apiEncrypted && !status.haConnected;
}

/** The install window's length, "2 h" or "1 h 30 min"; a window may cross midnight. Null for a
 *  time that isn't "HH:MM", and for equal times, which the satellite refuses. */
export function windowLength(start: string, end: string): string | null {
  const minutes = (t: string) => {
    const m = /^(\d{2}):(\d{2})$/.exec(t);
    return m ? Number(m[1]) * 60 + Number(m[2]) : null;
  };
  const a = minutes(start);
  const b = minutes(end);
  if (a === null || b === null || a === b) return null;
  const length = (b - a + 24 * 60) % (24 * 60);
  const h = Math.floor(length / 60);
  const m = length % 60;
  return [h ? `${h} h` : "", m ? `${m} min` : ""].filter(Boolean).join(" ");
}

/** A token as the System page shows it: its last four characters, or "None". */
export function tokenShown(last4: string): string {
  return last4 ? `••••${last4}` : "None";
}

/** Before a first password is saved: who will need a token. Null when nobody called, or when
 *  the token they need exists already. */
export function tokenNeeds(sec: Security | null): string | null {
  if (!sec) return null;
  const did: string[] = [];
  if (sec.mcpClients.length && !sec.agentTokenLast4) did.push(`MCP clients (${sec.mcpClients.join(", ")}) called it`);
  if (sec.metricsRead && !sec.metricsTokenLast4) did.push("something read /metrics, such as Prometheus");
  if (!did.length) return null;
  return `In the last 24 hours, ${did.join(" and ")}. With a password, they need a token from the System page.`;
}

/** The Overview's notice while no password is set; null once one is. */
export function openNotice(sec: Security | null, standalone: boolean): string[] | null {
  if (!sec || sec.passwordSet) return null;
  const lines = ["Anyone on your network can control this satellite."];
  if (standalone) lines.push("That includes where your voice goes.");
  return lines;
}
