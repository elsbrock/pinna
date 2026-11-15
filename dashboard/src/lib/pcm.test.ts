import assert from "node:assert/strict";
import { test } from "node:test";

import { Resampler, fromS16, rms, toS16 } from "./pcm.ts";

test("S16 round trip, clipped at full scale", () => {
  const back = fromS16(toS16(Float32Array.from([0, 0.5, -0.5, 1, -1, 2, -2])));
  assert.deepEqual(
    Array.from(back, (x) => Math.round(x * 1000) / 1000),
    [0, 0.5, -0.5, 1, -1, 1, -1],
  );
  assert.equal(fromS16(new ArrayBuffer(5)).length, 2, "odd trailing byte ignored");
  assert.equal(new DataView(toS16(Float32Array.from([-1]))).getInt16(0, true), -32768);
});

test("rms", () => {
  assert.equal(rms(new Float32Array(0)), 0);
  assert.equal(rms(Float32Array.from([0.5, -0.5])), 0.5);
});

function tone(rate: number, hz: number, seconds: number): Float32Array {
  return Float32Array.from({ length: rate * seconds }, (_, i) => 0.5 * Math.sin((2 * Math.PI * hz * i) / rate));
}

for (const rate of [48_000, 44_100, 16_000]) {
  test(`resampler ${rate} Hz -> 16 kHz: length, speech kept, alias removed`, () => {
    // Fed in odd-sized pieces, as worklet blocks arrive.
    const feed = (input: Float32Array) => {
      const r = new Resampler(rate);
      const parts: number[] = [];
      for (let i = 0; i < input.length; i += 128 + (i % 3)) parts.push(...r.push(input.subarray(i, i + 128 + (i % 3))));
      return Float32Array.from(parts);
    };
    const speech = feed(tone(rate, 1_000, 1));
    assert.ok(Math.abs(speech.length - 16_000) <= 1, `length ${speech.length}`);
    // Past the filter's start-up, a 1 kHz tone keeps its level (0.5 / sqrt 2).
    assert.ok(Math.abs(rms(speech.subarray(1_000)) - 0.3536) < 0.01, `1 kHz rms ${rms(speech.subarray(1_000))}`);
    if (rate > 16_000) {
      // 12 kHz would fold to 4 kHz; the low-pass takes it out.
      const alias = feed(tone(rate, 12_000, 1));
      assert.ok(rms(alias.subarray(1_000)) < 0.01, `12 kHz rms ${rms(alias.subarray(1_000))}`);
    }
  });
}

test("resampler refuses to go up", () => {
  assert.throws(() => new Resampler(8_000));
});
