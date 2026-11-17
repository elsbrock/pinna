import assert from "node:assert/strict";
import { test } from "node:test";

import type { Satellite, Settings, Status, Timer, WakeWord, WakeWordOption } from "../gen/pinna/v1/device_pb.ts";

// node's type stripping can't run the generated module (it holds a TS enum),
// so the tests build message shapes by hand.
const word = (id: string, phrase: string): WakeWordOption => ({ $typeName: "pinna.v1.WakeWordOption", id, phrase });
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
  ...over,
});
import {
  badge,
  byline,
  CAL_KIND,
  calibrationAdvice,
  calibrationBlocked,
  canStop,
  clock,
  displayName,
  joinDone,
  joinTone,
  mediaFrom,
  shortUrl,
  mmss,
  securityWord,
  unjoinable,
  downloadProgress,
  bitrate,
  health,
  healthWord,
  installSteps,
  ringEffect,
  routeOf,
  satelliteFallbacks,
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

test("routeOf: sections, sub-pages, moved v1 paths and unknowns", () => {
  assert.equal(routeOf(""), "/");
  assert.equal(routeOf("#/"), "/");
  assert.equal(routeOf("#/voice"), "/voice");
  assert.equal(routeOf("#/system/logs"), "/system/logs");
  assert.equal(routeOf("#/voice/wake-word"), "/voice/wake-word");
  assert.equal(routeOf("#/voice/media-sources"), "/voice/media-sources");
  assert.equal(routeOf("#/settings"), "/voice");
  assert.equal(routeOf("#/updates"), "/system");
  assert.equal(routeOf("#/logs"), "/system/logs");
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
  assert.equal(satelliteFallbacks(renamed)[0].host, "echo8.local");
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

test("fallbacks: the .local name and the first IPv4 address", () => {
  assert.deepEqual(satelliteFallbacks(satellite({ webPort: 8080 })), [
    { href: "http://kitchen.local:8080/", host: "kitchen.local:8080" },
    { href: "http://192.168.1.20:8080/", host: "192.168.1.20:8080" },
  ]);
  assert.equal(satelliteFallbacks(satellite({ ipv4: [] })).length, 1);
});

test("the drawing follows the board", () => {
  assert.equal(satelliteModel("Echo (2nd Gen)"), "echo-2");
  assert.equal(satelliteModel("Echo Dot (2nd Gen)"), "echo-dot-2");
  assert.equal(satelliteModel(""), "echo-dot-2");
});

test("now playing says where media comes from: a stream's source, a station, Home Assistant, or the address", () => {
  const m = { stream: false, source: "", station: "", url: "" };
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "airplay" }), { text: "From AirPlay", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "bluetooth" }), { text: "From Bluetooth", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true }), { text: "From AirPlay or Bluetooth", url: false });
  assert.deepEqual(mediaFrom({ ...m, stream: true, source: "my-player" }), { text: "From my-player", url: false });
  const radio = "http://radio.example/live.mp3";
  assert.deepEqual(mediaFrom({ ...m, url: radio, station: "Radio One" }), { text: "From Radio One", url: false });
  assert.deepEqual(mediaFrom({ ...m, url: "http://ha.local:8123/api/esphome/ffmpeg_proxy/abc/def.mp3" }), {
    text: "From Home Assistant",
    url: false,
  });
  assert.deepEqual(mediaFrom({ ...m, url: radio }), { text: radio, url: true });
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
