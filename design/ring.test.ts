// Mirrors the tests in crates/pinna-hal/src/anim.rs.
// Run: node --test design/ring.test.ts

import assert from "node:assert/strict";
import { test } from "node:test";
import { EQUALIZER, LED, RING_PIXELS, pixelAngle, ringFrame, type Frame } from "./ring.ts";

const lit = (f: Frame) => f.filter((p) => p.c !== null && p.level > 0).length;
const full = (f: Frame, i: number, c: readonly number[]) => f[i].c === c && f[i].level === 1;

test("idle is dark", () => {
  assert.equal(lit(ringFrame("idle", 123)), 0);
});

test("muted is solid red", () => {
  assert.ok(ringFrame("muted", 5).every((p) => p.c === LED.muted && p.level === 1));
});

test("listening pulses between bounds", () => {
  const lo = ringFrame("listening", 0)[0].level;
  const hi = ringFrame("listening", 800)[0].level;
  assert.equal(hi, 1);
  assert.ok(lo > 0 && lo < hi);
});

test("listening points at the talker", () => {
  const base = [28, 74, 102];
  const front = ringFrame("listening-at", 0, 0);
  // 0 degrees falls between the front-centre pair, 15 degrees off each.
  assert.deepEqual(front[10], front[11]);
  assert.deepEqual(front[11].c, [167, 193, 209]);
  assert.deepEqual(front[0].c, [47, 91, 117]);
  assert.deepEqual(front[9], front[0]);
  for (let i = 1; i < 9; i++) assert.deepEqual(front[i].c, base, `pixel ${i}`);
  const right = ringFrame("listening-at", 0, 45);
  assert.deepEqual(right[0].c, [227, 244, 255]);
  assert.deepEqual(right[0], ringFrame("listening-at", 0, 405)[0]);
  const back = ringFrame("listening-at", 0, 195);
  assert.deepEqual(back[5], right[0]);
  assert.deepEqual(back[11].c, base);
  assert.deepEqual(back, ringFrame("listening-at", 777, 195));
});

test("thinking spins with the head at pixel 0 at t=0", () => {
  const a = ringFrame("thinking", 0);
  assert.ok(full(a, 0, LED.thinking));
  assert.notDeepEqual(a, ringFrame("thinking", 300));
  assert.equal(lit(a), 5);
});

test("error flashes three times, then off", () => {
  const on = (ms: number) => lit(ringFrame("error", ms)) > 0;
  assert.ok(on(0) && !on(300) && on(500) && !on(800) && on(1000) && !on(1300));
  assert.ok(!on(1600));
});

test("volume arc scales and expires", () => {
  const f = ringFrame("volume", 0, 50);
  assert.ok(full(f, 0, LED.volume) && full(f, 5, LED.volume) && !full(f, 6, LED.volume));
  assert.ok(full(ringFrame("volume", 0, 100), 11, LED.volume));
  assert.ok(ringFrame("volume", 0, 0).every((p) => p.level <= 0.04));
  assert.equal(lit(ringFrame("volume", 1500, 50)), 0);
});

test("timer arc tracks the fraction left", () => {
  const f = ringFrame("timer", 0, 0.25);
  assert.ok(full(f, 0, LED.amber) && full(f, 2, LED.amber) && !full(f, 3, LED.amber));
  assert.ok(ringFrame("timer", 0, 1).every((p) => p.level === 1));
});

test("timer ringing pulses at 2 Hz", () => {
  assert.ok(ringFrame("timer-ringing", 250)[0].level > ringFrame("timer-ringing", 0)[0].level);
});

test("notification pulses slowly", () => {
  const dim = ringFrame("notification", 0)[0];
  assert.ok(full(ringFrame("notification", 1500), 0, LED.notification));
  assert.ok(dim.level > 0 && dim.level < 0.12);
  const a = ringFrame("notification", 400)[0].level;
  assert.ok(Math.abs(a - ringFrame("notification", 3400)[0].level) < 1e-9);
});

test("notification cleared flashes, then fades out", () => {
  assert.ok(full(ringFrame("notification-cleared", 0), 0, LED.notification));
  const mid = ringFrame("notification-cleared", 200)[0].level;
  assert.ok(mid > 0.4 && mid < 0.6);
  assert.equal(lit(ringFrame("notification-cleared", 400)), 0);
});

test("calibration arc tracks the sensitivity", () => {
  const f = ringFrame("calibration", 0, 50);
  assert.ok(full(f, 0, LED.calibration) && full(f, 5, LED.calibration) && !full(f, 6, LED.calibration));
  assert.ok(full(ringFrame("calibration", 0, 100), 11, LED.calibration));
  assert.ok(ringFrame("calibration", 0, 0).every((p) => p.level <= 0.04));
  assert.notDeepEqual(LED.calibration, LED.volume);
});

test("a heard wake flashes once", () => {
  assert.ok(full(ringFrame("calibration-heard", 0), 0, LED.calibration));
  const mid = ringFrame("calibration-heard", 300)[0].level;
  assert.ok(mid > 0.4 && mid < 0.6, `${mid}`);
  assert.equal(lit(ringFrame("calibration-heard", 600)), 0);
});

test("pairing turns two heads half a ring apart", () => {
  const f = ringFrame("pairing", 0);
  const heads = f.flatMap((p, i) => (p.c === LED.pairing && p.level === 1 ? [i] : []));
  assert.equal(heads.length, 2);
  assert.equal(heads[1] - heads[0], RING_PIXELS / 2);
  assert.notDeepEqual(ringFrame("pairing", 400), f);
});

test("confirm circles one white head slowly", () => {
  const f = ringFrame("confirm", 0);
  const heads = f.flatMap((p, i) => (p.c === LED.confirm && p.level === 1 ? [i] : []));
  assert.deepEqual(heads, [0]);
  assert.ok(full(ringFrame("confirm", 250), 1, LED.confirm));
  assert.deepEqual(ringFrame("confirm", 3000), f);
});

test("confirm-recording circles one head in the listening colour", () => {
  const f = ringFrame("confirm-recording", 0);
  const heads = f.flatMap((p, i) => (p.c === LED.listening && p.level === 1 ? [i] : []));
  assert.deepEqual(heads, [0]);
  assert.ok(full(ringFrame("confirm-recording", 250), 1, LED.listening));
  assert.deepEqual(ringFrame("confirm-recording", 3000), f);
});

test("boot spins, then off", () => {
  assert.ok(lit(ringFrame("boot", 100)) > 0);
  assert.equal(lit(ringFrame("boot", 1300)), 0);
});

test("equalizer mirrors pairs, bass at the front", () => {
  assert.equal(lit(ringFrame("equalizer", 0, Array(12).fill(0))), 0);
  const bands = Array(12).fill(0);
  bands[1] = 1; // bass pair -> pixels 10 and 11
  bands[10] = 0.5; // treble pair -> pixels 4 and 5, half level
  const f = ringFrame("equalizer", 0, bands);
  assert.ok(full(f, 10, EQUALIZER[0]) && full(f, 11, EQUALIZER[0]));
  assert.ok(f[4].c === EQUALIZER[5] && f[4].level === 0.5 && f[5].level === 0.5);
  assert.equal(lit(f), 4);
  const g = Array(12).fill(0);
  g[4] = 1; // pair 2 -> pixels 1 and 8
  const h = ringFrame("equalizer", 0, g);
  assert.ok(full(h, 1, EQUALIZER[2]) && full(h, 8, EQUALIZER[2]));
  assert.equal(lit(h), 2);
});

test("pixels 10 and 11 straddle the front (bottom)", () => {
  assert.equal(RING_PIXELS, 12);
  assert.equal((pixelAngle(10) + pixelAngle(11)) / 2 % 360, 180);
});
