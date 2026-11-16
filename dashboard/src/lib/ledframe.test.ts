import assert from "node:assert/strict";
import { test } from "node:test";

import { frameFromBytes } from "./ledframe.ts";

test("ring frame to colour and level", () => {
  const f = frameFromBytes(Uint8Array.from([0xff, 0, 0, 0, 0x80, 0x80, ...Array(30).fill(0)]));
  assert.ok(f);
  assert.deepEqual(f[0], { c: [255, 0, 0], level: 1 });
  assert.deepEqual(f[1], { c: [0, 255, 255], level: 128 / 255 });
  assert.deepEqual(f[2], { c: null, level: 0 });
  assert.equal(f.length, 12);
});

test("not a ring frame", () => {
  assert.equal(frameFromBytes(new Uint8Array()), null);
  assert.equal(frameFromBytes(new Uint8Array(35)), null);
});
