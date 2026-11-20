import assert from "node:assert/strict";
import { test } from "node:test";

import { wavDebug, wavMono16 } from "./wav.ts";

const text = (b: Uint8Array, at: number, n: number) => String.fromCharCode(...b.subarray(at, at + n));
const view = (b: Uint8Array) => new DataView(b.buffer, b.byteOffset, b.byteLength);

test("a mono WAV has the 44-byte PCM header", () => {
  const wav = wavMono16([Uint8Array.of(0x34, 0x12, 0xff, 0xff)], 16_000);
  const v = view(wav);
  assert.equal(wav.length, 48);
  assert.equal(text(wav, 0, 4), "RIFF");
  assert.equal(v.getUint32(4, true), 40);
  assert.equal(text(wav, 8, 8), "WAVEfmt ");
  assert.equal(v.getUint32(16, true), 16);
  assert.equal(v.getUint16(20, true), 1, "PCM");
  assert.equal(v.getUint16(22, true), 1, "mono");
  assert.equal(v.getUint32(24, true), 16_000);
  assert.equal(v.getUint32(28, true), 32_000, "byte rate");
  assert.equal(v.getUint16(32, true), 2, "block align");
  assert.equal(v.getUint16(34, true), 16);
  assert.equal(text(wav, 36, 4), "data");
  assert.equal(v.getUint32(40, true), 4);
  assert.deepEqual([...wav.subarray(44)], [0x34, 0x12, 0xff, 0xff]);
});

test("a sample split across two chunks is whole again, and a trailing odd byte is dropped", () => {
  const wav = wavMono16([Uint8Array.of(0x34), Uint8Array.of(0x12, 0x78), Uint8Array.of(0x56, 0x99)], 16_000);
  assert.equal(view(wav).getUint32(40, true), 4);
  assert.deepEqual([...wav.subarray(44)], [0x34, 0x12, 0x78, 0x56]);
  assert.equal(wavMono16([], 16_000).length, 44);
});

test("the debug WAV is WAVE_FORMAT_EXTENSIBLE, 24-bit, with the lanes and two more channels", () => {
  const wav = wavDebug({
    lanes: [Uint8Array.of(1, 2, 3, 4, 5, 6)],
    laneCount: 2,
    beforeGain: [Uint8Array.of(0x34, 0x12)],
    reference: [Uint8Array.of(0xff, 0xff)],
    rate: 16_000,
  });
  const v = view(wav);
  assert.equal(wav.length, 68 + 12);
  assert.equal(text(wav, 0, 4), "RIFF");
  assert.equal(v.getUint32(4, true), wav.length - 8);
  assert.equal(text(wav, 8, 8), "WAVEfmt ");
  assert.equal(v.getUint32(16, true), 40);
  assert.equal(v.getUint16(20, true), 0xfffe);
  assert.equal(v.getUint16(22, true), 4, "channels");
  assert.equal(v.getUint32(24, true), 16_000);
  assert.equal(v.getUint32(28, true), 16_000 * 12, "byte rate");
  assert.equal(v.getUint16(32, true), 12, "block align");
  assert.equal(v.getUint16(34, true), 24);
  assert.equal(v.getUint16(36, true), 22, "cbSize");
  assert.equal(v.getUint16(38, true), 24, "valid bits");
  assert.equal(v.getUint32(40, true), 0, "channel mask");
  assert.deepEqual(
    [...wav.subarray(44, 60)],
    [0x01, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71],
    "the PCM sub-format",
  );
  assert.equal(text(wav, 60, 4), "data");
  assert.equal(v.getUint32(64, true), 12);
  // Lanes as they come, then before the gain and the reference, each widened to 24 bits.
  assert.deepEqual([...wav.subarray(68)], [1, 2, 3, 4, 5, 6, 0x00, 0x34, 0x12, 0x00, 0xff, 0xff]);
});

test("the shorter streams are padded with silence to the longest", () => {
  // 3 frames of 2 lanes; 1 sample before the gain; 2 of the reference. The lane
  // chunks split a frame in two.
  const lanes = Uint8Array.from({ length: 18 }, (_, i) => i + 1);
  const wav = wavDebug({
    lanes: [lanes.subarray(0, 7), lanes.subarray(7)],
    laneCount: 2,
    beforeGain: [Uint8Array.of(1, 0)],
    reference: [Uint8Array.of(2, 0, 3, 0)],
    rate: 16_000,
  });
  assert.equal(view(wav).getUint32(64, true), 36);
  const data = [...wav.subarray(68)];
  assert.deepEqual(data.slice(0, 12), [1, 2, 3, 4, 5, 6, 0, 1, 0, 0, 2, 0]);
  assert.deepEqual(data.slice(12, 24), [7, 8, 9, 10, 11, 12, 0, 0, 0, 0, 3, 0]);
  assert.deepEqual(data.slice(24), [13, 14, 15, 16, 17, 18, 0, 0, 0, 0, 0, 0]);
});

test("a partial frame at the end is dropped, and the longest stream decides the length", () => {
  const wav = wavDebug({
    lanes: [Uint8Array.of(1, 2, 3, 4, 5, 6, 7)],
    laneCount: 2,
    beforeGain: [Uint8Array.of(1, 0, 2, 0, 3, 0, 4)],
    reference: [],
    rate: 16_000,
  });
  // One whole lane frame, three whole samples before the gain: three frames.
  assert.equal(view(wav).getUint32(64, true), 36);
  assert.deepEqual([...wav.subarray(68, 80)], [1, 2, 3, 4, 5, 6, 0, 1, 0, 0, 0, 0]);
  assert.deepEqual([...wav.subarray(80, 92)], [0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0]);
});

test("a negative 16-bit sample keeps its sign in the top two bytes", () => {
  const wav = wavDebug({
    lanes: [],
    laneCount: 1,
    beforeGain: [Uint8Array.of(0x00, 0x80)],
    reference: [Uint8Array.of(0xfe, 0xff)],
    rate: 16_000,
  });
  // -32768 and -2, as s24: 0x800000 and 0xfffe00; the lane is silent.
  assert.deepEqual([...wav.subarray(68)], [0, 0, 0, 0x00, 0x00, 0x80, 0x00, 0xfe, 0xff]);
});
