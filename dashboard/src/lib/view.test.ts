import assert from "node:assert/strict";
import { test } from "node:test";

import type { Status, Timer } from "../gen/pinna/v1/device_pb.ts";
import {
  badge,
  canStop,
  clock,
  downloadProgress,
  bitrate,
  health,
  healthWord,
  installSteps,
  ringEffect,
  routeOf,
  since,
  STAGE,
  startBlocked,
  timerLeft,
  uptime,
  wakeHint,
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
    wakeWord: { phrase: "Alexa", enabled: true },
    timers: [],
    audioBackend: "none",
    ledBackend: "none",
    alarmRinging: false,
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
  const idle = { phrase: "Alexa", enabled: false } as Status["wakeWord"];
  assert.match(wakeHint(status({ wakeWord: idle, muted: true })), /while the microphones are off/);
  assert.match(wakeHint(status({ wakeWord: idle, voiceSubscribed: false })), /voice pipeline/);
  assert.match(wakeHint(status({ wakeWord: idle, alarmRinging: true })), /alarm rings/);
  assert.match(wakeHint(status({ wakeWord: idle, phase: 3 })), /during the conversation/);
  const off = { wakeWord: false, wakeSensitivity: 50, ledBrightness: 100 } as Status["settings"];
  assert.equal(wakeHint(status({ settings: off })), "Off. Press the action button to talk.");
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
  const { slotWord, targetSlot, updatePhase } = await import("./view.ts");
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
});

test("routeOf: sections, sub-pages, moved v1 paths and unknowns", () => {
  assert.equal(routeOf(""), "/");
  assert.equal(routeOf("#/"), "/");
  assert.equal(routeOf("#/voice"), "/voice");
  assert.equal(routeOf("#/system/logs"), "/system/logs");
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
