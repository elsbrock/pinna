import assert from "node:assert/strict";
import { test } from "node:test";

import type {
  Satellite,
  Security,
  Settings,
  Status,
  Timer,
  WakeWord,
  WakeWordOption,
} from "../gen/pinna/v1/device_pb.ts";

// node's type stripping can't run the generated module (it holds a TS enum),
// so the tests build message shapes by hand.
const word = (id: string, phrase: string): WakeWordOption => ({
  $typeName: "pinna.v1.WakeWordOption",
  id,
  phrase,
  custom: false,
  error: "",
});
const wakeWord = (over: Partial<WakeWord> = {}): WakeWord => ({
  $typeName: "pinna.v1.WakeWord",
  phrase: "",
  enabled: false,
  threshold: 0,
  recent: [],
  available: [],
  ...over,
});
const settings = (over: Partial<Settings> = {}): Settings => ({
  $typeName: "pinna.v1.Settings",
  wakeWord: true,
  wakeSensitivity: 50,
  ledBrightness: 100,
  wakeWords: [],
  friendlyName: "Echo Dot aabbcc",
  defaultName: true,
  wakeSound: false,
  url: "",
  airplay: false,
  bluetooth: false,
  timeZone: "UTC",
  listeningDirection: true,
  // UPDATE_CHANNEL_STABLE
  updateChannel: 1,
  mode: 0,
  groupMessages: true,
  sendspin: false,
  installUpdates: 1,
  installWindowStart: "03:00",
  installWindowEnd: "05:00",
  includeInStatistics: true,
  ...over,
});
import {
  addedNextStep,
  ago,
  badge,
  confirmationWarning,
  PURPOSE,
  recordHint,
  recordingFile,
  recordingWarning,
  failuresText,
  gate,
  grantLeft,
  homeAssistantBlocked,
  loginOutcome,
  lostSession,
  openNotice,
  passwordNote,
  passwordProblem,
  secondsUntil,
  splitRetry,
  tokenNeeds,
  tokenShown,
  windowLength,
  byline,
  CAL_KIND,
  calibrationAdvice,
  calibrationBlocked,
  canStop,
  clock,
  customWordCount,
  displayName,
  fileSize,
  modelFileProblem,
  phraseProblem,
  removeWordEffects,
  wordHint,
  joinDone,
  joinTone,
  mediaFrom,
  mediaPauses,
  OLDER_SLOT_WARNING,
  olderSlotWarning,
  shortUrl,
  mmss,
  securityWord,
  unjoinable,
  downloadProgress,
  bitrate,
  health,
  healthWord,
  heldText,
  installSteps,
  ringEffect,
  routeOf,
  satelliteLink,
  satelliteModel,
  since,
  spokenWakeWords,
  STAGE,
  startBlocked,
  timerLeft,
  uptime,
  scoreText,
  thresholdFor,
  wakeAdvice,
  wakeHint,
  wakeWordsHint,
  when,
  wifiConnected,
  windowLeft,
} from "./view.ts";

// A connected, idle satellite; tests override what they need.
const status = (over: Partial<Status> = {}): Status =>
  ({
    version: "0.1.0",
    name: "pinna-test",
    mac: "02:00:00:aa:bb:cc",
    uptimeSeconds: 0n,
    phase: 1,
    muted: false,
    hardwareMuted: false,
    softwareMuted: false,
    volume: 50,
    haConnected: true,
    voiceSubscribed: true,
    wakeWord: wakeWord({ phrase: "Alexa", enabled: true }),
    timers: [],
    audioBackend: "none",
    ledBackend: "none",
    alarmRinging: false,
    notification: false,
    settings: { wakeWord: true, wakeSensitivity: 50, ledBrightness: 100 },
    ...over,
  }) as Status;

test("ring follows the device: muted, then a ringing alarm, then the phase", () => {
  assert.equal(ringEffect(status({ phase: 3 }), "live"), "thinking");
  assert.equal(ringEffect(status({ alarmRinging: true }), "live"), "timer-ringing");
  assert.equal(ringEffect(status({ muted: true, alarmRinging: true, phase: 2 }), "live"), "muted");
  assert.equal(ringEffect(status({ phase: 0 }), "live"), "idle");
  assert.equal(ringEffect(status({ phase: 2 }), "offline"), "idle");
  assert.equal(ringEffect(null, "connecting"), "idle");
});

test("the test pattern shows above everything, step by step, and holds its last step", () => {
  const testing = status({ ledTest: true, muted: true, alarmRinging: true, phase: 3 });
  assert.equal(ringEffect(testing, "live"), "listening");
  assert.equal(ringEffect(testing, "live", 1), "thinking");
  assert.equal(ringEffect(testing, "live", 2), "speaking");
  assert.equal(ringEffect(testing, "live", 3), "muted");
  assert.equal(ringEffect(testing, "live", 9), "muted");
  assert.equal(ringEffect(testing, "offline", 1), "idle");
});

test("an open pairing window shows while idle, under the phase", () => {
  const open = { pairingSecondsLeft: 60 } as Status["bluetooth"];
  assert.equal(ringEffect(status({ bluetooth: open }), "live"), "pairing");
  assert.equal(ringEffect(status({ bluetooth: open, phase: 3 }), "live"), "thinking");
  const closed = { pairingSecondsLeft: 0 } as Status["bluetooth"];
  assert.equal(ringEffect(status({ bluetooth: closed, notification: true }), "live"), "notification");
});

test("a waiting notification shows while idle, below a running timer", () => {
  const tea = { id: "1", name: "Tea", secondsLeft: 90, totalSeconds: 180, active: true } as Timer;
  assert.equal(ringEffect(status({ notification: true }), "live"), "notification");
  assert.equal(ringEffect(status({ notification: true, phase: 2 }), "live"), "listening");
  assert.equal(ringEffect(status({ notification: true, muted: true }), "live"), "muted");
  assert.equal(ringEffect(status({ notification: true, timers: [tea] }), "live"), "timer");
  assert.equal(ringEffect(status({ timers: [tea] }), "live"), "timer");
  assert.equal(ringEffect(status({ timers: [tea], phase: 2 }), "live"), "listening");
  assert.equal(ringEffect(status({ timers: [{ ...tea, active: false }] }), "live"), "idle");
  assert.equal(ringEffect(status({ notification: true, timers: [{ ...tea, active: false }] }), "live"), "notification");
});

test("the satellite goes by the name Home Assistant shows, else its node name", () => {
  assert.equal(displayName(status({ settings: settings({ friendlyName: "Küche" }) })), "Küche");
  assert.equal(displayName(status({ settings: settings({ friendlyName: "" }) })), "pinna-test");
  assert.equal(displayName(status({ settings: undefined })), "pinna-test");
});

test("badge words are the proto's", () => {
  assert.deepEqual(badge(status()), { phase: "idle", word: "Idle" });
  assert.deepEqual(badge(status({ phase: 4 })), { phase: "speaking", word: "Speaking" });
  assert.deepEqual(badge(status({ muted: true, phase: 2 })), { phase: "muted", word: "Muted" });
  assert.deepEqual(badge(status({ alarmRinging: true })), { phase: "timer", word: "Ringing" });
});

test("health line wording", () => {
  assert.deepEqual(health(status(), "live", null), { tone: "ok", text: "Connected to Home Assistant" });
  assert.equal(health(status({ voiceSubscribed: false }), "live", null).text, "Connected, no voice pipeline");
  assert.deepEqual(health(status({ haConnected: false }), "live", null), {
    tone: "caution",
    text: "Not connected to Home Assistant",
  });
  const since = new Date(2026, 10, 15, 14, 2);
  assert.deepEqual(health(status(), "offline", since), { tone: "danger", text: "Not reachable since 14:02" });
  assert.equal(health(null, "offline", null).text, "Not reachable");
  assert.deepEqual(health(status(), "offline", since, true), { tone: "caution", text: "Restarting" });
});

// pinna.v1.Mode values: 1 is standalone.
const assist = (ready: boolean) =>
  ({ ready, sttReady: ready, brainReady: ready, ttsReady: ready, label: "", lastError: "" }) as Status["assistant"];
const standalone = (ready: boolean, over: Partial<Status> = {}) =>
  status({ mode: 1, haConnected: false, voiceSubscribed: false, assistant: assist(ready), ...over });

test("in standalone mode the health line is the services', not Home Assistant's", () => {
  const ready = health(standalone(true), "live", null);
  assert.deepEqual(ready, { tone: "ok", text: "Answers in standalone mode", word: "Standalone" });
  assert.equal(healthWord(ready), "Standalone");
  const waiting = health(standalone(false), "live", null);
  assert.equal(waiting.tone, "caution");
  assert.equal(healthWord(waiting), "Not ready");
  assert.equal(health(standalone(true), "offline", null).text, "Not reachable");
});

test("in standalone mode start, the wake word and its pickers follow the services", () => {
  assert.equal(startBlocked(standalone(true), "live"), null);
  assert.match(startBlocked(standalone(false), "live") ?? "", /standalone services/);
  assert.match(startBlocked(standalone(true, { muted: true }), "live") ?? "", /microphones on/);

  const idle = wakeWord({ phrase: "Alexa" });
  assert.match(wakeHint(standalone(false, { wakeWord: idle })), /until the standalone services pass their check/);
  assert.equal(wakeHint(standalone(true, { wakeWord: idle })), 'Not listening for "Alexa" yet');

  // Home Assistant may stay connected for media; the dashboard still chooses the words.
  const withWords = wakeWord({ phrase: "Alexa", enabled: true, available: [word("alexa_v0.1", "Alexa")] });
  assert.equal(wakeWordsHint(standalone(true, { wakeWord: withWords, haConnected: true })), undefined);
});

test("start says why it is disabled", () => {
  assert.equal(startBlocked(status(), "live"), null);
  assert.match(startBlocked(status({ haConnected: false }), "live") ?? "", /Connect Home Assistant/);
  assert.match(startBlocked(status({ muted: true }), "live") ?? "", /microphones on/);
  assert.match(startBlocked(status({ phase: 2 }), "live") ?? "", /already running/);
  assert.match(startBlocked(status({ alarmRinging: true }), "live") ?? "", /alarm/);
  assert.match(startBlocked(status(), "offline") ?? "", /not reachable/);
});

test("stop covers conversations and a ringing alarm", () => {
  assert.equal(canStop(status(), "live"), false);
  assert.equal(canStop(status({ phase: 4 }), "live"), true);
  assert.equal(canStop(status({ alarmRinging: true }), "live"), true);
  assert.equal(canStop(status({ phase: 4 }), "offline"), false);
});

test("active timers tick locally, paused ones hold", () => {
  const t = { id: "1", name: "Tea", secondsLeft: 90, totalSeconds: 180, active: true } as Timer;
  assert.equal(timerLeft(t, 0), 90);
  assert.equal(timerLeft(t, 1999), 89);
  assert.equal(timerLeft(t, 120_000), 0);
  assert.equal(timerLeft({ ...t, active: false } as Timer, 60_000), 90);
});

test("clock and uptime formats", () => {
  assert.equal(clock(272), "04:32");
  assert.equal(clock(0), "00:00");
  assert.equal(clock(3872), "1:04:32");
  assert.equal(uptime(45), "45s");
  assert.equal(uptime(12 * 60), "12m");
  assert.equal(uptime(4 * 3600 + 12 * 60), "4h 12m");
  assert.equal(uptime(3 * 86400 + 4 * 3600 + 59), "3d 4h");
});

test("wake word hint names pinnad's reason for pausing", () => {
  assert.equal(wakeHint(status()), 'Listening for "Alexa"');
  const idle = wakeWord({ phrase: "Alexa" });
  assert.match(wakeHint(status({ wakeWord: idle, muted: true })), /while the microphones are off/);
  assert.match(wakeHint(status({ wakeWord: idle, voiceSubscribed: false })), /voice pipeline/);
  assert.match(wakeHint(status({ wakeWord: idle, phase: 3 })), /during the conversation/);
  const off = settings({ wakeWord: false });
  assert.equal(wakeHint(status({ settings: off })), "Off. Press the action button to talk.");
  // Home Assistant can set both dropdowns to no word; the hint says so then.
  assert.equal(wakeHint(status({ wakeWord: wakeWord() })), "No wake word is chosen.");
});

test("wake word pickers say who chooses", () => {
  const withWords = wakeWord({ phrase: "Alexa", enabled: true, available: [word("alexa_v0.1", "Alexa")] });
  assert.equal(wakeWordsHint(status({ wakeWord: withWords, haConnected: false })), undefined);
  assert.match(wakeWordsHint(status({ wakeWord: withWords })) ?? "", /Home Assistant chooses/);
  // Nothing installed: the pickers say so before anything else.
  assert.match(wakeWordsHint(status()) ?? "", /No wake word models are installed/);
});

test("spoken wake words: quoted, in the order they listen", () => {
  const words = [word("alexa_v0.1", "Alexa"), word("hey_jarvis_v0.1", "Hey Jarvis")];
  const st = (ids: string[]) =>
    status({
      wakeWord: wakeWord({ phrase: "Alexa or Hey Jarvis", enabled: true, available: words }),
      settings: settings({ wakeWords: ids }),
    });
  assert.equal(spokenWakeWords(st(["alexa_v0.1"])), "“Alexa”");
  assert.equal(spokenWakeWords(st(["alexa_v0.1", "hey_jarvis_v0.1"])), "“Alexa” or “Hey Jarvis”");
  // A word that is no longer installed is left out.
  assert.equal(spokenWakeWords(st(["gone", "hey_jarvis_v0.1"])), "“Hey Jarvis”");
  assert.equal(spokenWakeWords(status({ wakeWord: wakeWord() })), "the wake word");
});

test("calibration starts only when the satellite would", () => {
  assert.equal(calibrationBlocked(status(), "live"), null);
  assert.match(calibrationBlocked(status({ muted: true }), "live") ?? "", /microphones on/);
  const off = { wakeWord: false, wakeSensitivity: 50, ledBrightness: 100 } as Status["settings"];
  assert.match(calibrationBlocked(status({ settings: off }), "live") ?? "", /wake word on/);
  assert.match(calibrationBlocked(status({ alarmRinging: true }), "live") ?? "", /alarm/);
  assert.match(calibrationBlocked(status({ phase: 3 }), "live") ?? "", /already running/);
  assert.match(calibrationBlocked(status(), "offline") ?? "", /not reachable/);
});

test("calibration events say what a press did", () => {
  const event = (over: object) =>
    ({ id: 1, kind: CAL_KIND.woke, raise: false, from: 68, to: 68, ...over }) as Parameters<
      typeof calibrationAdvice
    >[0];

  const woke = calibrationAdvice(event({ kind: CAL_KIND.woke, score: 0.62 }));
  assert.equal(woke.label, "Heard the wake word");
  assert.match(woke.text, /Score 0\.62/);
  assert.match(woke.text, /No request started/);

  const raised = calibrationAdvice(event({ kind: CAL_KIND.raised, raise: true, score: 0.5, to: 85 }));
  assert.equal(raised.label, "+ raised the sensitivity");
  assert.match(raised.text, /68 → 85, to catch a near miss at 0\.50/);

  const lowered = calibrationAdvice(event({ kind: CAL_KIND.lowered, score: 0.62, from: 85, to: 49 }));
  assert.equal(lowered.label, "− lowered the sensitivity");
  assert.match(lowered.text, /85 → 49, to ignore a wake at 0\.62/);

  const nothing = calibrationAdvice(event({ kind: CAL_KIND.nothingHeard, raise: true }));
  assert.equal(nothing.label, "+ did nothing");
  assert.match(nothing.text, /last 15 seconds/);

  // Too faint for any setting: below what 100 catches.
  const faint = calibrationAdvice(event({ kind: CAL_KIND.noSettingHelps, raise: true, score: 0.35 }));
  assert.match(faint.text, /too faint for any setting/);
  // Reached what this setting needs, rejected for another reason.
  const rejected = calibrationAdvice(event({ kind: CAL_KIND.noSettingHelps, raise: true, score: 0.62 }));
  assert.match(rejected.text, /quiet against the background or too short/);
  // Every setting wakes on it.
  const everyone = calibrationAdvice(event({ kind: CAL_KIND.noSettingHelps, score: 0.95 }));
  assert.match(everyone.text, /Every setting wakes on a score of 0\.95/);

  const undoRaise = calibrationAdvice(event({ kind: CAL_KIND.wouldUndo, raise: true, score: 0.48 }));
  assert.match(undoRaise.text, /let back the false wake/);
  const undoLower = calibrationAdvice(event({ kind: CAL_KIND.wouldUndo, score: 0.62 }));
  assert.match(undoLower.text, /lose the wake you caught/);
});

test("network words", async () => {
  const { band, interfaceLabel, signalWord, wifiIssue } = await import("./view.ts");
  assert.equal(signalWord(-50), "Excellent");
  assert.equal(signalWord(-67), "Good");
  assert.equal(signalWord(-70), "Fair");
  assert.equal(signalWord(-80), "Weak");
  assert.equal(band(2437), "2.4 GHz");
  assert.equal(band(5180), "5 GHz");
  assert.equal(interfaceLabel("wlan0", true), "Wi-Fi");
  assert.equal(interfaceLabel("ap0", true), "Setup hotspot");
  assert.equal(interfaceLabel("usb0", false), "USB (console)");
  assert.match(wifiIssue(undefined)?.title ?? "", /No Wi-Fi interface/);
  assert.match(wifiIssue({ addresses: [] })?.title ?? "", /Not connected/);
  assert.match(wifiIssue({ wifi: { ssid: "Home" }, addresses: ["fd00::2/64"] })?.title ?? "", /no IPv4/);
  assert.equal(wifiIssue({ wifi: { ssid: "Home" }, addresses: ["192.168.1.20/24"] }), null);
  // Without link details (no iw), an address still means connected.
  assert.equal(wifiIssue({ addresses: ["192.168.1.20/24"] }), null);
});

test("update phases come from the slots", async () => {
  const { slotWord, switchTarget, targetSlot, updatePhase } = await import("./view.ts");
  const slot = (name: string, over: object = {}) => ({
    name,
    version: "0.1.0",
    state: "good",
    triesLeft: 0,
    active: false,
    booted: false,
    ...over,
  });
  const u = (slots: ReturnType<typeof slot>[], over: object = {}) => ({ available: true, installing: false, slots, ...over });
  const a = slot("a", { active: true, booted: true });
  assert.equal(updatePhase(u([a, slot("b", { state: "unbootable" })])), "ready");
  assert.equal(updatePhase(u([a, slot("b")], { installing: true })), "installing");
  assert.equal(updatePhase(u([a, slot("b")], { available: false })), "unavailable");
  // Installed into b, not restarted yet.
  const pending = u([slot("a", { booted: true }), slot("b", { active: true, state: "trial", triesLeft: 3 })]);
  assert.equal(updatePhase(pending), "pending");
  assert.equal(targetSlot(pending)?.name, "b");
  // Restarted into b, which is on trial until the health check marks it good.
  const trial = u([slot("a"), slot("b", { active: true, booted: true, state: "trial", triesLeft: 2 })]);
  assert.equal(updatePhase(trial), "trial");
  assert.equal(slotWord(trial.slots[1]), "On trial, 2 tries left");
  assert.equal(slotWord(slot("b", { state: "unbootable" })), "Can't boot");
  // The update in b was kept back: it hasn't been started.
  const kept = u([a, slot("b", { state: "trial", triesLeft: 3 })]);
  assert.equal(updatePhase(kept), "ready");
  assert.equal(slotWord(kept.slots[1]), "Not started yet");

  // Switching: to the other slot when it is good or not started yet, and nothing else waits.
  assert.equal(switchTarget(u([a, slot("b")]))?.name, "b");
  assert.equal(switchTarget(kept)?.name, "b");
  assert.equal(switchTarget(u([a, slot("b", { state: "unbootable" })])), undefined);
  assert.equal(switchTarget(u([a, slot("b", { version: "" })])), undefined);
  assert.equal(switchTarget(u([a, slot("b")], { installing: true })), undefined);
  assert.equal(switchTarget(pending), undefined);
  assert.equal(switchTarget(trial), undefined);
});

test("lastStart: one line per outcome, crashes and rescue mode marked", async () => {
  const { BOOT, lastStart } = await import("./view.ts");
  assert.deepEqual(lastStart(undefined), { hint: "What the satellite is doing, as it happens", danger: false });
  const b = (outcome: number, reason = "") => ({ outcome, reason, summary: "pinnad's summary" });
  assert.deepEqual(lastStart(b(BOOT.coldStart)), { hint: "Last start: from power off, or after a power cut", danger: false });
  assert.deepEqual(lastStart(b(BOOT.restarted, "update")), { hint: "Last start: a restart (update)", danger: false });
  assert.deepEqual(lastStart(b(BOOT.restarted)), { hint: "Last start: a restart", danger: false });
  assert.deepEqual(lastStart(b(BOOT.rescue, "fsck")), { hint: "Last start: rescue mode (fsck)", danger: true });
  assert.deepEqual(lastStart(b(BOOT.crashed)), { hint: "Last start: after a crash, a kernel panic or oops", danger: true });
  assert.deepEqual(lastStart(b(0)), { hint: "Last start: pinnad's summary", danger: false });
});

test("updateSummary: the first matching state wins, failures first", async () => {
  const { ACTION, CHANNEL, INSTALL, updateSummary } = await import("./view.ts");
  const slot = (name: string, over: object = {}) => ({
    name,
    version: "0.1.0",
    state: "good",
    triesLeft: 0,
    active: false,
    booted: false,
    ...over,
  });
  const slots = [slot("a", { active: true, booted: true, version: "0.1.0" }), slot("b", { version: "0.0.9" })];
  const u = (over: object = {}) =>
    ({ available: true, installing: false, restartPending: false, unavailableReason: "", slots, ...over }) as never;
  const settings = (over: object = {}) =>
    ({
      updateChannel: CHANNEL.stable,
      installUpdates: INSTALL.manually,
      installWindowStart: "03:00",
      installWindowEnd: "05:00",
      ...over,
    }) as never;
  const run = "0.1.0";

  assert.deepEqual(updateSummary(null, run, settings()), {
    value: run,
    hint: "Where updates come from, and installing them",
  });
  // The reason stays on the Updates page.
  assert.deepEqual(updateSummary(u({ available: false, unavailableReason: "No A/B slots." }), run, settings()), {
    value: run,
    hint: "Updates aren't available here",
  });
  assert.equal(updateSummary(u({ available: false }), run, settings()).hint, "Updates aren't available here");
  assert.deepEqual(updateSummary(u({ installing: true }), run, settings()), {
    value: "Installing",
    hint: "Writing the other slot",
  });
  assert.deepEqual(updateSummary(u({ release: { version: "0.2.0", action: ACTION.failed } }), run, settings()), {
    value: "0.2.0",
    hint: "It didn't start, so the satellite went back",
    tone: "danger",
  });
  assert.deepEqual(updateSummary(u({ lastInstall: { ok: false } }), run, settings()), {
    value: run,
    hint: "The last install failed",
    tone: "danger",
  });
  const trial = [slot("a", { active: true, booted: true, state: "trial" }), slot("b")];
  assert.deepEqual(updateSummary(u({ slots: trial }), run, settings()), {
    value: run,
    hint: "On trial until its check passes",
  });
  // Another slot boots next: its version waits for the restart.
  const pending = [slot("a", { booted: true }), slot("b", { active: true, version: "0.2.0" })];
  assert.deepEqual(updateSummary(u({ slots: pending }), run, settings()), {
    value: "0.2.0",
    hint: "Starts at the next restart",
  });
  // A restart pending with the booted slot still active: the slot an install writes.
  assert.deepEqual(updateSummary(u({ restartPending: true }), run, settings()), {
    value: "0.0.9",
    hint: "Starts at the next restart",
  });
  assert.deepEqual(updateSummary(u({ release: { version: "0.3.0", action: ACTION.blocked } }), run, settings(), "echo-dot-2"), {
    value: "0.3.0",
    hint: "Needs a new boot image, installed over USB",
    tone: "caution",
  });
  assert.equal(
    updateSummary(u({ release: { version: "0.3.0", action: ACTION.blocked } }), run, settings(), "echo-dot-5").hint,
    "Needs a new boot image, installed over USB",
  );
  assert.equal(
    updateSummary(u({ release: { version: "0.3.0", action: ACTION.blocked } }), run, settings(), "").hint,
    "Needs a new boot image, which an update can't install",
  );
  assert.equal(
    updateSummary(
      u({
        release: {
          version: "0.3.0",
          action: ACTION.blocked,
          note: "This release has no boot image for the Echo Dot (5th gen).",
        },
      }),
      run,
      settings(),
      "echo-dot-5",
    ).hint,
    "This release has no boot image for the Echo Dot (5th gen).",
  );
  assert.deepEqual(updateSummary(u({ release: { version: "0.3.0", action: ACTION.install } }), run, settings()), {
    value: "0.3.0",
    hint: "Ready to install",
  });
  assert.deepEqual(updateSummary(u({ manifestOverride: "http://192.168.1.5:8000/manifest.json" }), run, settings()), {
    value: run,
    hint: "Updates come from a developer's address",
  });
  assert.deepEqual(updateSummary(u(), run, settings({ updateChannel: CHANNEL.off })), {
    value: run,
    hint: "Updates are off",
    tone: "caution",
  });
  assert.deepEqual(updateSummary(u({ check: { error: "no route" } }), run, settings()), {
    value: run,
    hint: "The last check failed",
    tone: "caution",
  });
  assert.deepEqual(updateSummary(u(), run, settings({ updateChannel: CHANNEL.beta })), {
    value: run,
    hint: "Beta channel. You install updates.",
  });
  assert.deepEqual(updateSummary(u(), run, settings({ installUpdates: INSTALL.automatically })), {
    value: run,
    hint: "Stable channel. Installs automatically between 03:00 and 05:00.",
  });
  // Up to date, with a release that is not newer: the quiet line.
  assert.equal(updateSummary(u({ release: { version: run, action: ACTION.upToDate } }), run, settings()).tone, undefined);
});

test("routeOf: sections, sub-pages, moved paths and unknowns", () => {
  assert.equal(routeOf(""), "/");
  assert.equal(routeOf("#/"), "/");
  assert.equal(routeOf("#/voice"), "/voice");
  assert.equal(routeOf("#/system/logs"), "/system/logs");
  assert.equal(routeOf("#/voice/wake-word"), "/voice/wake-word");
  assert.equal(routeOf("#/voice/wake-words"), "/voice/wake-words");
  assert.equal(routeOf("#/voice/media-sources"), "/voice/media-sources");
  assert.equal(routeOf("#/settings"), "/voice");
  assert.equal(routeOf("#/updates"), "/system/updates");
  assert.equal(routeOf("#/system/updates"), "/system/updates");
  assert.equal(routeOf("#/system/password"), "/system/password");
  assert.equal(routeOf("#/logs"), "/system/logs");
  assert.equal(routeOf("#/assistant"), "/assistant");
  assert.equal(routeOf("#/assistant/services"), "/assistant/services");
  assert.equal(routeOf("#/assistant/conversation"), "/assistant/conversation");
  assert.equal(routeOf("#/assistant/tools"), "/assistant/tools");
  assert.equal(routeOf("#/assistant/group"), "/assistant/group");
  assert.equal(routeOf("#/home-assistant"), "/assistant");
  assert.equal(routeOf("#/nope"), null);
});

test("installSteps: from the stage reached and how it ended", () => {
  const states = (stage: number, finished: { ok: boolean } | null) =>
    installSteps(stage, finished).map((s) => s.state);
  assert.deepEqual(states(0, null), ["current", "pending", "pending", "pending"]);
  assert.deepEqual(states(STAGE.check, null), ["done", "current", "pending", "pending"]);
  assert.deepEqual(states(STAGE.download, null), ["done", "current", "pending", "pending"]);
  assert.deepEqual(states(STAGE.write, null), ["done", "done", "current", "pending"]);
  assert.deepEqual(states(STAGE.verify, null), ["done", "done", "current", "pending"]);
  assert.deepEqual(states(STAGE.installed, null), ["done", "done", "done", "current"]);
  assert.deepEqual(states(STAGE.installed, { ok: true }), ["done", "done", "done", "current"]);
  // A failure fails the step it was on.
  assert.deepEqual(states(STAGE.download, { ok: false }), ["done", "failed", "pending", "pending"]);
  assert.deepEqual(states(0, { ok: false }), ["failed", "pending", "pending", "pending"]);
});

test("downloadProgress: pinnactl's progress lines only", () => {
  assert.deepEqual(downloadProgress("  53.1 MiB of 212.4 MiB"), { text: "53.1 of 212.4 MiB", fraction: 53.1 / 212.4 });
  assert.equal(downloadProgress("downloading http://x/root.img (212.4 MiB) into /dev/x"), null);
  assert.equal(downloadProgress("  indented"), null);
});

test("since: the start time from uptime", () => {
  const now = new Date(2026, 10, 12, 9, 20); // a Thursday
  assert.equal(since(now, 6 * 60), "Thu 09:14");
});

test("healthWord: the app bar's word for each health line", () => {
  assert.equal(healthWord({ tone: "ok", text: "Connected to Home Assistant" }), "Connected");
  assert.equal(healthWord({ tone: "danger", text: "Not reachable since 14:02" }), "Not reachable");
  assert.equal(healthWord({ tone: "caution", text: "Restarting" }), "Restarting");
  assert.equal(healthWord({ tone: "caution", text: "Connected, no voice pipeline" }), "Check Home Assistant");
});

test("bitrate: iw's tx bitrate, rounded", () => {
  assert.equal(bitrate("433.3 MBit/s VHT-MCS 9 80MHz short GI"), "433 Mbit/s");
  assert.equal(bitrate("odd"), "odd");
});

test("wifiConnected: link details or an IPv4 address", () => {
  assert.equal(wifiConnected(undefined), false);
  assert.equal(wifiConnected({ wifi: {}, addresses: [] }), true);
  assert.equal(wifiConnected({ addresses: ["192.168.1.42/24"] }), true);
  assert.equal(wifiConnected({ addresses: ["fe80::1/64"] }), false);
});

test("wakeAdvice: the setting that ignores a wake or catches a near miss", () => {
  const wake = (score: number, wakesFrom?: number) => ({ woke: true, score, wakesFrom, reachedThreshold: false });
  const near = (score: number, wakesFrom?: number, reachedThreshold = false) => ({
    woke: false,
    score,
    wakesFrom,
    reachedThreshold,
  });

  // A wake at 0.62 first wakes at 53: 52 ignores it, 49 with the margin.
  assert.deepEqual(wakeAdvice(wake(0.62, 53), 68), {
    text: "Score 0.62. If it wasn't you, sensitivity 49 ignores it.",
    set: 49,
  });
  assert.deepEqual(wakeAdvice(wake(0.62, 53), 40), { text: "Score 0.62. Ignored at this setting." });
  assert.match(wakeAdvice(wake(0.91, 0), 68).text, /^Score 0\.91: every setting wakes on this\. If it wasn't you, move/);
  assert.equal(wakeAdvice(wake(0.91, 0), 68).set, undefined);
  assert.equal(wakeAdvice(wake(0.84, 2), 68).set, 0);

  // A near miss at 0.48 first wakes at 85: 90 with the margin.
  assert.deepEqual(wakeAdvice(near(0.48, 85), 68), {
    text: "Score 0.48. If it was you, sensitivity 90 catches it.",
    set: 90,
  });
  assert.equal(wakeAdvice(near(0.42, 98), 68).set, 100);
  assert.deepEqual(wakeAdvice(near(0.48, 85), 85), { text: "Score 0.48. Wakes at this setting." });
  assert.equal(wakeAdvice(near(0.35), 68).set, undefined);
  assert.match(wakeAdvice(near(0.35), 68).text, /too faint for any setting/);

  // What else the suggestion ignores or catches, of what the list holds: a cost for wakes.
  const strong = wake(0.81, 10);
  const list = [strong, wake(0.62, 53), wake(0.7, 35), wake(0.62, 53), near(0.5, 80), near(0.45, 91)];
  assert.equal(
    wakeAdvice(strong, 68, list).text,
    "Score 0.81. If it wasn't you, sensitivity 6 ignores it. But it also ignores 3 other wakes listed here.",
  );
  assert.equal(
    wakeAdvice(list[1], 68, list).text,
    "Score 0.62. If it wasn't you, sensitivity 49 ignores it. But it also ignores 1 other wake listed here.",
  );
  // A wake already ignored now isn't lost again.
  assert.match(wakeAdvice(strong, 50, list).text, /also ignores 1 other wake listed/);
  assert.equal(wakeAdvice(list[4], 68, list).text, "Score 0.50. If it was you, sensitivity 85 catches it.");
  assert.equal(
    wakeAdvice(list[5], 68, list).text,
    "Score 0.45. If it was you, sensitivity 96 catches it. It also catches 1 other near miss listed here.",
  );

  // Enough at the time, rejected for being too quiet or short: a next step, no setting.
  const other = wakeAdvice(near(0.6, 57, true), 50);
  assert.equal(other.set, undefined);
  assert.match(other.text, /^Score 0\.60: high enough at the time, .* Speak closer or louder/);
});

test("scoreText rounds away from the threshold, thresholdFor matches the detector", () => {
  assert.equal(thresholdFor(0), 0.85);
  assert.ok(Math.abs(thresholdFor(68) - 0.5508) < 1e-9);
  assert.ok(Math.abs(thresholdFor(100) - 0.41) < 1e-9);
  // 68 needs 0.5508: the hint, a wake just over it and a near miss just under it.
  assert.equal(scoreText(thresholdFor(68), true), "0.56");
  assert.equal(scoreText(0.5509, true), "0.56");
  assert.equal(scoreText(0.5499, false), "0.54");
  // Exact hundredths stay as they are, either way.
  assert.equal(scoreText(thresholdFor(75), true), "0.52");
  assert.equal(scoreText(0.62, true), "0.62");
  assert.equal(scoreText(0.62, false), "0.62");
  // f32 scores from pinnad.
  assert.equal(scoreText(Math.fround(0.48), false), "0.48");
  assert.equal(scoreText(Math.fround(0.81), true), "0.81");
});

test("when: time today, yesterday, or the date", () => {
  const now = new Date(2026, 10, 15, 21, 30);
  assert.equal(when(new Date(2026, 10, 15, 9, 5).getTime(), now), "09:05");
  assert.equal(when(new Date(2026, 10, 14, 23, 59).getTime(), now), "Yesterday 23:59");
  assert.equal(when(new Date(2026, 10, 12, 14, 2).getTime(), now), "12 Nov 14:02");
});

test("Wi-Fi words: security, what can't be joined, a join's end", () => {
  assert.equal(securityWord(2), "WPA2");
  assert.equal(securityWord(3), "WPA3 only");
  assert.equal(unjoinable(1), null);
  assert.equal(unjoinable(2), null);
  assert.match(unjoinable(3) ?? "", /can't join/);
  assert.match(unjoinable(4) ?? "", /Enterprise/);
  // CONNECTING, ADDRESS run on; TRIAL, JOINED, FAILED end the stream.
  assert.deepEqual([1, 2, 3, 4, 5].map((stage) => joinDone({ stage })), [false, false, true, true, true]);
  assert.equal(joinTone({ stage: 4 }), "ok");
  assert.equal(joinTone({ stage: 5 }), "caution");
  assert.equal(joinTone({ stage: 3 }), null);
  assert.equal(joinTone(null), null);
  assert.equal(mmss(245), "4:05");
  assert.equal(mmss(-3), "0:00");
});

const satellite = (over: Partial<Satellite> = {}): Satellite => ({
  $typeName: "pinna.v1.Satellite",
  name: "kitchen",
  host: "kitchen",
  friendlyName: "Kitchen",
  board: "Echo Dot (2nd Gen)",
  mac: "02:00:00:aa:bb:cc",
  version: "0.9.0",
  webPort: 80,
  url: "",
  ipv4: ["192.168.1.20"],
  ipv6: ["fd00::20", "2001:db8::20"],
  groupMember: false,
  ...over,
});
const at = (hostname: string) => ({ protocol: "http:", hostname });

test("a satellite's own dashboard address wins", () => {
  const s = satellite({ url: "https://kitchen.example.com/", webPort: 8080 });
  assert.deepEqual(satelliteLink(s, at("pinna.local")), { href: "https://kitchen.example.com/", host: "kitchen.example.com" });
  assert.equal(satelliteLink(satellite({ url: "http://10.0.0.2:8123/" }), at("x.local")).host, "10.0.0.2:8123");
});

test("an announced address that isn't plain http(s) is not a link", () => {
  for (const url of ["javascript:alert(1)", "ftp://x/", "https://u:p@x.example/", "not a url"]) {
    assert.equal(satelliteLink(satellite({ url }), at("pinna.local")).href, "http://kitchen.local/", url);
  }
});

test("opened by a .local name: the other's .local name", () => {
  assert.deepEqual(satelliteLink(satellite(), at("Pinna.local.")), { href: "http://kitchen.local/", host: "kitchen.local" });
  assert.equal(satelliteLink(satellite({ webPort: 8080 }), at("pinna.local")).href, "http://kitchen.local:8080/");
});

test("the .local name is the host the SRV record names, which a name conflict may have changed", () => {
  const renamed = satellite({ name: "echo10", host: "echo8" });
  assert.equal(satelliteLink(renamed, at("pinna.local")).href, "http://echo8.local/");
  // Older firmware sends no host: the instance name, as before.
  assert.equal(satelliteLink(satellite({ host: "" }), at("pinna.local")).href, "http://kitchen.local/");
});

test("opened by an IPv4 address or another name: the other's IPv4 address", () => {
  assert.deepEqual(satelliteLink(satellite(), at("192.168.1.5")), { href: "http://192.168.1.20/", host: "192.168.1.20" });
  assert.equal(satelliteLink(satellite({ webPort: 8080 }), at("dot.fritz.box")).href, "http://192.168.1.20:8080/");
  // No IPv4 address: the .local name.
  assert.equal(satelliteLink(satellite({ ipv4: [] }), at("192.168.1.5")).href, "http://kitchen.local/");
});

test("opened by an IPv6 literal: a global address before a unique-local one", () => {
  assert.equal(satelliteLink(satellite(), at("[2001:db8::5]")).href, "http://[2001:db8::20]/");
  assert.equal(satelliteLink(satellite({ ipv6: ["fd00::20"], webPort: 81 }), at("[fd00::5]")).href, "http://[fd00::20]:81/");
  assert.equal(satelliteLink(satellite({ ipv6: [] }), at("[2001:db8::5]")).href, "http://kitchen.local/");
});

test("the drawing follows the board", () => {
  assert.equal(satelliteModel("Echo (2nd Gen)"), "echo-2");
  assert.equal(satelliteModel("Echo Dot (2nd Gen)"), "echo-dot-2");
  assert.equal(satelliteModel("Echo Dot (5th Gen)"), "echo-dot-5");
  assert.equal(satelliteModel(""), "");
  assert.equal(satelliteModel("Echo Show 8"), "");
});

test("now playing says where media comes from: a stream's source, a station, Home Assistant, or the address", () => {
  const m = { stream: false, source: "", station: "", url: "" };
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "airplay" }), { text: "From AirPlay", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "bluetooth" }), { text: "From Bluetooth", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "sendspin" }), { text: "From Music Assistant", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true }), { text: "From AirPlay or Bluetooth", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "constructor" }), { text: "From constructor", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "my-player" }), { text: "From my-player", url: false });
  const radio = "http://radio.example/live.mp3";
  assert.deepEqual(mediaFrom({ ...m, url: radio, station: "Radio One" }), { text: "From Radio One", url: false });
  assert.deepEqual(mediaFrom({ ...m, url: "http://ha.local:8123/api/esphome/ffmpeg_proxy/abc/def.mp3" }), {
    text: "From Home Assistant",
    url: false,
  });
  assert.deepEqual(mediaFrom({ ...m, url: radio }), { text: radio, url: true });
});

test("a paused Sendspin group reads as paused from Music Assistant, and can resume", () => {
  // MediaState: PLAYING 2, PAUSED 3, ANNOUNCING 4.
  const paused = { state: 3, stream: false, source: "sendspin", station: "", url: "" };
  assert.deepEqual(mediaFrom(paused), { text: "From Music Assistant", url: false });
  assert.deepEqual(mediaFrom({ ...paused, stream: true }), { text: "From Music Assistant", url: false });
  assert.equal(mediaPauses(paused), true);
  assert.equal(mediaPauses({ ...paused, stream: true }), true);
  assert.equal(mediaPauses({ ...paused, state: 2, stream: true }), true);
});

test("Pause shows for a URL and Sendspin, not for AirPlay, Bluetooth or an announcement", () => {
  const url = { state: 2, stream: false, source: "" };
  assert.equal(mediaPauses(url), true);
  assert.equal(mediaPauses({ ...url, state: 4 }), false);
  assert.equal(mediaPauses({ ...url, stream: true, source: "airplay" }), false);
  assert.equal(mediaPauses({ ...url, stream: true, source: "bluetooth" }), false);
  assert.equal(mediaPauses({ ...url, stream: true, source: "" }), false);
  assert.equal(mediaPauses({ ...url, state: 4, stream: true, source: "sendspin" }), false);
});

test("a track's byline joins the artist and album it has", () => {
  assert.equal(byline({ artist: "Artist", album: "Album" }), "Artist · Album");
  assert.equal(byline({ artist: "", album: "Album" }), "Album");
  assert.equal(byline({ artist: "", album: "" }), "");
});

test("a URL shows as its server and file, without the folders or the query", () => {
  assert.equal(shortUrl("https://radio.example/live.mp3?token=secret"), "radio.example/live.mp3");
  assert.equal(shortUrl("http://nas.local:8080/music/Some%20Album/01%20Song.flac"), "nas.local:8080/…/01 Song.flac");
  assert.equal(shortUrl("http://radio.example/"), "radio.example");
  assert.equal(shortUrl("http://radio.example/a/%E0%A4%A.mp3"), "radio.example/…/%E0%A4%A.mp3");
  assert.equal(shortUrl("not a url"), "not a url");
});

test("update checks in words", async () => {
  const { checkStatus, channelName, CHANNEL } = await import("./view.ts");
  const now = new Date(2026, 10, 17, 15, 0);
  const check = (c: Partial<{ checking: boolean; finishedUnixMs: bigint; error: string; noRelease: boolean }>) => ({
    checking: false,
    finishedUnixMs: 0n,
    error: "",
    noRelease: false,
    ...c,
  });
  assert.equal(checkStatus(check({}), true, now), "Off: the satellite doesn't look for releases.");
  assert.equal(checkStatus(undefined, false, now), "Not checked yet since the satellite started.");
  assert.equal(checkStatus(check({ checking: true }), false, now), "Checking…");
  const at = BigInt(new Date(2026, 10, 17, 14, 2).getTime());
  assert.equal(checkStatus(check({ finishedUnixMs: at }), false, now), "Checked at 14:02.");
  const monday = BigInt(new Date(2026, 10, 16, 9, 5).getTime());
  assert.equal(checkStatus(check({ finishedUnixMs: monday }), false, now), "Checked Mon 09:05.");
  assert.equal(checkStatus(check({ finishedUnixMs: at, error: "x" }), false, now), "Last check failed at 14:02.");
  assert.equal(channelName(CHANNEL.beta), "beta");
  assert.equal(channelName(0), "off");
});

test("install updates is manual until the satellite says otherwise", async () => {
  const { installName, INSTALL } = await import("./view.ts");
  assert.equal(installName(INSTALL.automatically), "automatically");
  assert.equal(installName(INSTALL.manually), "manually");
  assert.equal(installName(0), "manually");
  assert.equal(installName(undefined), "manually");
});

test("held messages from the satellite group are counted only when there are some", () => {
  assert.equal(heldText(0, false), null);
  assert.equal(heldText(1, false), "1 message held");
  assert.equal(heldText(5, false), "5 messages held");
  assert.equal(heldText(0, true), null);
});

test("held messages name the microphones while they are off: they play once the microphones are on", () => {
  assert.equal(heldText(1, true), "1 message held · it plays once the microphones are on");
  assert.equal(heldText(3, true), "3 messages held · they play once the microphones are on");
});

test("custom wake words: the phrase and the model file are checked before the upload", () => {
  assert.equal(phraseProblem("Hey Pinna"), null);
  assert.match(phraseProblem("a") ?? "", /2 to 40 characters/);
  assert.match(phraseProblem("  x  ") ?? "", /2 to 40 characters/);
  assert.match(phraseProblem("x".repeat(41)) ?? "", /2 to 40 characters/);
  assert.equal(phraseProblem("é".repeat(40)), null);
  assert.equal(modelFileProblem("hey_pinna.onnx", 1000), null);
  assert.equal(modelFileProblem("Hey.TFLITE", 2 * 1024 * 1024), null);
  assert.match(modelFileProblem("hey.bin", 1000) ?? "", /\.onnx or \.tflite/);
  assert.match(modelFileProblem("hey.onnx", 0) ?? "", /empty/);
  assert.match(modelFileProblem("hey.onnx", 3 * 1024 * 1024) ?? "", /3\.0 MiB.*at most 2 MiB/);
  assert.equal(fileSize(800), "800 bytes");
  assert.equal(fileSize(120 * 1024), "120 KiB");
  assert.equal(fileSize(1.5 * 1024 * 1024), "1.5 MiB");
});

test("custom wake words: each word's hint says where it came from and whether it listens", () => {
  const mine = { ...word("hey_pinna", "Hey Pinna"), custom: true };
  const broken = { ...word("bad", "Bad"), custom: true, error: "The model failed to load." };
  const s = status({
    wakeWord: wakeWord({ available: [word("alexa_v0.1", "Alexa"), mine, broken] }),
    settings: settings({ wakeWords: ["hey_pinna"] }),
  });
  assert.equal(customWordCount(s), 2);
  assert.equal(wordHint(s, s.wakeWord!.available[0]), "Built in.");
  assert.equal(wordHint(s, mine), "Added on this satellite. Chosen.");
  assert.equal(wordHint(s, broken), "The model failed to load.");
});

test("removing a wake word says what happens", () => {
  const withWords = (over: Partial<Status>) =>
    status({
      wakeWord: wakeWord({ available: [word("a", "A"), word("b", "B")] }),
      settings: settings({ wakeWords: ["a"] }),
      ...over,
    });
  assert.deepEqual(removeWordEffects(withWords({ haConnected: false }), "b"), [
    "It deletes the model from this satellite.",
  ]);
  // The only chosen word: the satellite is left without one.
  const only = removeWordEffects(withWords({ haConnected: false }), "a");
  assert.equal(only.length, 3);
  assert.match(only[1], /stops listening/);
  assert.match(only[2], /no wake word/);
  const twoChosen = withWords({ haConnected: false, settings: settings({ wakeWords: ["a", "b"] }) });
  assert.equal(removeWordEffects(twoChosen, "a").length, 2);
  const ha = removeWordEffects(withWords({ haConnected: true }), "a");
  assert.equal(ha.length, 4);
  assert.match(ha[3], /choose another one there/);
  // A word that isn't chosen needs no replacement.
  assert.equal(removeWordEffects(withWords({ haConnected: true }), "b").length, 1);
  // Standalone mode: the dashboard chooses, even with Home Assistant connected.
  assert.equal(removeWordEffects(standalone(true, { haConnected: true }), "b").length, 1);
});

test("after adding a wake word the next step follows who chooses", () => {
  assert.match(addedNextStep(status({ haConnected: true })), /device page in Home Assistant.*once the satellite is idle/);
  assert.match(addedNextStep(status({ haConnected: false })), /Voice & sound/);
  assert.match(addedNextStep(standalone(true, { haConnected: true })), /Voice & sound/);
  assert.match(addedNextStep(null), /recent wakes under Sensitivity/);
});

const security = (over: Partial<Security> = {}): Security => ({
  $typeName: "pinna.v1.Security",
  passwordSet: false,
  leftOpen: false,
  offerPending: false,
  signedIn: false,
  metricsTokenLast4: "",
  agentTokenLast4: "",
  failuresSinceLogin: 0,
  lastFailureAddress: "",
  lastFailureUnixMs: 0n,
  loginRetryAfterS: 0,
  confirmationCooldownS: 0,
  mcpClients: [],
  metricsRead: false,
  homeAssistantWindowLeftS: 0,
  groupJoinLeftS: 0,
  confirmationWaitingFrom: "",
  confirmationLeftS: 0,
  confirmationPurpose: PURPOSE.password,
  ...over,
});

test("the page opens on the offer, the sign-in form or the app", () => {
  assert.equal(gate(null), "app");
  assert.equal(gate(security({ offerPending: true })), "offer");
  assert.equal(gate(security({ leftOpen: true })), "app");
  assert.equal(gate(security({ passwordSet: true })), "signin");
  assert.equal(gate(security({ passwordSet: true, signedIn: true })), "app");
});

test("a new password has 8 to 128 characters, counted as characters", () => {
  assert.match(passwordProblem("short") ?? "", /8 to 128/);
  assert.equal(passwordProblem("12345678"), null);
  assert.equal(passwordProblem("ä".repeat(128)), null);
  assert.match(passwordProblem("a".repeat(129)) ?? "", /8 to 128/);
});

test("the sign-in answer gives the server's sentence and how long to wait", () => {
  assert.deepEqual(loginOutcome(204, null), { ok: true });
  assert.deepEqual(loginOutcome(401, { error: "Wrong password." }), { ok: false, message: "Wrong password.", wait: 0 });
  assert.deepEqual(loginOutcome(429, { error: "Too many attempts. Try again in 7 s.", retry_after_s: 7 }), {
    ok: false,
    message: "Too many attempts. Try again in 7 s.",
    wait: 7,
  });
  const odd = loginOutcome(502, "<html>");
  assert.equal(odd.ok, false);
  assert.equal(odd.ok === false && odd.wait, 0);
});

test("the wait counts down in whole seconds and stops at zero", () => {
  assert.equal(secondsUntil(10_000, 7_500), 3);
  assert.equal(secondsUntil(10_000, 10_000), 0);
  assert.equal(secondsUntil(10_000, 12_000), 0);
});

test("a time reads as the largest unit that fits", () => {
  const now = 10_000_000_000;
  assert.equal(ago(now - 20_000, now), "just now");
  assert.equal(ago(now - 60_000, now), "1 minute ago");
  assert.equal(ago(now - 5 * 3_600_000, now), "5 hours ago");
  assert.equal(ago(now - 2 * 86_400_000, now), "2 days ago");
});

test("failed sign-ins are counted with the last address and time", () => {
  const now = 10_000_000_000;
  assert.equal(failuresText(security(), now), "None since the last sign-in.");
  assert.equal(
    failuresText(
      security({ failuresSinceLogin: 3, lastFailureAddress: "192.168.1.9", lastFailureUnixMs: BigInt(now - 300_000) }),
      now,
    ),
    "3 failed sign-ins since the last sign-in. The last was from 192.168.1.9, 5 minutes ago.",
  );
  assert.equal(failuresText(security({ failuresSinceLogin: 1 }), now), "1 failed sign-in since the last sign-in.");
});

test("a token shows by its last four characters, or as none", () => {
  assert.equal(tokenShown(""), "None");
  assert.equal(tokenShown("a1b2"), "••••a1b2");
});

test("the first password lists who will need a token", () => {
  assert.equal(tokenNeeds(security()), null);
  assert.match(tokenNeeds(security({ mcpClients: ["claude-code", "cursor"] })) ?? "", /claude-code, cursor.*token/);
  assert.match(tokenNeeds(security({ metricsRead: true })) ?? "", /\/metrics/);
  assert.equal(
    tokenNeeds(security({ mcpClients: ["cursor"], metricsRead: true })),
    "In the last 24 hours, MCP clients (cursor) called it and something read /metrics, such as Prometheus. With a password, they need a token from System › Password and tokens.",
  );
  // A token that exists already is not news.
  assert.equal(tokenNeeds(security({ metricsRead: true, metricsTokenLast4: "a1b2" })), null);
  assert.equal(tokenNeeds(security({ mcpClients: ["cursor"], agentTokenLast4: "a1b2" })), null);
  assert.match(
    tokenNeeds(security({ mcpClients: ["cursor"], metricsRead: true, metricsTokenLast4: "a1b2" })) ?? "",
    /^In the last 24 hours, MCP clients \(cursor\) called it\. With/,
  );
});

test("the Overview warns while no password is set, and adds the voice in standalone mode", () => {
  assert.equal(openNotice(null, false), null);
  assert.equal(openNotice(security({ passwordSet: true }), true), null);
  assert.deepEqual(openNotice(security(), false), ["Anyone on your network can control this satellite."]);
  assert.deepEqual(openNotice(security({ leftOpen: true }), true), [
    "Anyone on your network can control this satellite.",
    "That includes where your voice goes.",
  ]);
});

test("the rollback warning shows for an older slot, with a password set", () => {
  assert.equal(
    OLDER_SLOT_WARNING,
    "The version in the other slot is older, and may not ask for a password: until the next update, anyone on your network could use the dashboard, /mcp and the metrics.",
  );
  assert.equal(olderSlotWarning(true, { olderThanRunning: true }), OLDER_SLOT_WARNING);
  // Without a password there is nothing to lose, and a newer slot is no rollback.
  assert.equal(olderSlotWarning(false, { olderThanRunning: true }), null);
  assert.equal(olderSlotWarning(true, { olderThanRunning: false }), null);
  assert.equal(olderSlotWarning(true, undefined), null);
});

test("a window says what it has left", () => {
  assert.equal(windowLeft(900), "15 min left");
  assert.equal(windowLeft(61), "2 min left");
  assert.equal(windowLeft(60), "1 min left");
  assert.equal(windowLeft(59), "59 s left");
  assert.equal(windowLeft(0.2), "1 s left");
  assert.equal(windowLeft(0), "");
  assert.equal(windowLeft(-3), "");
});

test("a refused sign-in with a removed password says so by status 409", () => {
  assert.deepEqual(loginOutcome(409, { error: "This satellite has no password." }), {
    ok: false,
    message: "This satellite has no password.",
    wait: 0,
    gone: true,
  });
});

test("a sentence that ends in a wait is split from it, so the page counts it down once", () => {
  assert.deepEqual(splitRetry("Too many attempts. Try again in 42 s."), { text: "Too many attempts.", wait: 42 });
  assert.deepEqual(splitRetry("A confirmation ended just now. Try again in 7 s."), {
    text: "A confirmation ended just now.",
    wait: 7,
  });
  assert.deepEqual(splitRetry("Wrong password."), { text: "Wrong password.", wait: 0 });
});

test("a password with a space at either end is allowed, with a note", () => {
  assert.equal(passwordNote("correct horse"), null);
  assert.equal(passwordNote(" correct horse"), "It starts or ends with a space.");
  assert.equal(passwordNote("correct horse "), "It starts or ends with a space.");
  assert.equal(passwordProblem(" 1234567"), null);
});

test("a grant lasts five minutes from when the page got it", () => {
  assert.equal(grantLeft(1_000, 1_000), 300);
  assert.equal(grantLeft(1_000, 1_000 + 299_001), 1);
  assert.equal(grantLeft(1_000, 1_000 + 300_000), 0);
  assert.equal(grantLeft(1_000, 1_000 + 900_000), 0);
});

test("the Overview warns of a confirmation another browser started, for a signed-in session", () => {
  const waiting = { signedIn: true, confirmationWaitingFrom: "192.168.1.23", confirmationLeftS: 41 };
  assert.deepEqual(confirmationWarning(security(waiting), false), {
    from: "192.168.1.23",
    left: 41,
    recording: false,
    debugging: false,
  });
  assert.deepEqual(confirmationWarning(security({ ...waiting, confirmationPurpose: PURPOSE.recording }), false), {
    from: "192.168.1.23",
    left: 41,
    recording: true,
    debugging: false,
  });
  assert.deepEqual(confirmationWarning(security({ ...waiting, confirmationPurpose: PURPOSE.debuggingText }), false), {
    from: "192.168.1.23",
    left: 41,
    recording: false,
    debugging: true,
  });
  assert.equal(confirmationWarning(security(waiting), true), null, "this browser started it");
  assert.equal(confirmationWarning(security({ ...waiting, confirmationLeftS: 0 }), false), null);
  assert.equal(confirmationWarning(security({ ...waiting, signedIn: false }), false), null);
  assert.equal(confirmationWarning(null, false), null);
});

test("a recording another dashboard runs is named, never to the one that records", () => {
  const recording = { $typeName: "pinna.v1.Recording", secondsLeft: 12, from: "192.168.1.30" } as const;
  const signedIn = security({ passwordSet: true, signedIn: true });
  assert.deepEqual(recordingWarning(status({ recording }), signedIn, false), { from: "192.168.1.30", left: 12 });
  assert.equal(recordingWarning(status({ recording }), signedIn, true), null, "this dashboard records it");
  assert.equal(recordingWarning(status({ recording }), security({ passwordSet: true }), false), null, "no session");
  assert.equal(recordingWarning(status(), signedIn, false), null, "none runs");
  assert.equal(recordingWarning(null, signedIn, false), null);
});

test("the Record audio row says what a recording needs, or that one runs", () => {
  assert.deepEqual(recordHint(false, false), { hint: "Needs a dashboard password", caution: true });
  assert.deepEqual(recordHint(true, false), { hint: "Up to 30 seconds from the microphones", caution: false });
  assert.deepEqual(recordHint(undefined, false), { hint: "Up to 30 seconds from the microphones", caution: false });
  assert.deepEqual(recordHint(true, true), { hint: "Recording…", caution: false });
});

test("a recording's files are named for the satellite, the kind and the local time", () => {
  const at = new Date(2026, 10, 20, 9, 5);
  assert.equal(recordingFile("kitchen", "heard", at), "kitchen-heard-2026-11-20-0905.wav");
  assert.equal(recordingFile("kitchen", "debug", at), "kitchen-debug-2026-11-20-0905.wav");
  assert.equal(recordingFile("", "heard", at), "pinna-heard-2026-11-20-0905.wav");
});

test("a recording shows Listening on the ring, above a timer's arc and a notification", () => {
  const recording = { $typeName: "pinna.v1.Recording", secondsLeft: 5, from: "192.168.1.30" } as const;
  assert.equal(ringEffect(status({ recording }), "live"), "listening");
  const tea = { id: "1", name: "Tea", secondsLeft: 90, totalSeconds: 180, active: true } as Timer;
  assert.equal(ringEffect(status({ recording, notification: true, timers: [tea] }), "live"), "listening");
  assert.equal(ringEffect(status({ recording, muted: true }), "live"), "muted");
  assert.equal(ringEffect(status({ recording, alarmRinging: true }), "live"), "timer-ringing");
  assert.equal(ringEffect(status({ recording, phase: 3 }), "live"), "thinking");
  assert.equal(ringEffect(status({ recording }), "offline"), "idle");
});

test("a tab that was signed in and isn't any more was signed out", () => {
  const was = security({ passwordSet: true, signedIn: true });
  assert.equal(lostSession(was, security({ passwordSet: true })), true);
  assert.equal(lostSession(was, security({ passwordSet: true, signedIn: true })), false);
  // The password was removed: nothing to sign in to.
  assert.equal(lostSession(was, security({ leftOpen: true })), false);
  assert.equal(lostSession(security({ passwordSet: true }), security({ passwordSet: true })), false);
  assert.equal(lostSession(null, security({ passwordSet: true })), false);
});

test("Home Assistant can't add a keyless satellite behind a password until the window is open", () => {
  const status = { apiEncrypted: false, haConnected: false };
  assert.equal(homeAssistantBlocked(security({ passwordSet: true }), status), true);
  assert.equal(homeAssistantBlocked(security(), status), false);
  assert.equal(homeAssistantBlocked(security({ passwordSet: true }), { ...status, apiEncrypted: true }), false);
  assert.equal(homeAssistantBlocked(security({ passwordSet: true }), { ...status, haConnected: true }), false);
  assert.equal(homeAssistantBlocked(security({ passwordSet: true }), undefined), false);
});

test("the install window's length is shown across midnight too", () => {
  assert.equal(windowLength("03:00", "05:00"), "2 h");
  assert.equal(windowLength("23:00", "01:00"), "2 h");
  assert.equal(windowLength("03:00", "04:30"), "1 h 30 min");
  assert.equal(windowLength("03:00", "03:20"), "20 min");
  assert.equal(windowLength("05:00", "03:00"), "22 h");
  assert.equal(windowLength("03:00", "03:00"), null);
  assert.equal(windowLength("", "03:00"), null);
});
