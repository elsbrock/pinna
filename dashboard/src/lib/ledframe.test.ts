import assert from "node:assert/strict";
import { test } from "node:test";

import { frameFromHex } from "./ledframe.ts";

test("hex ring frame to colour and level", () => {
  const f = frameFromHex("ff0000" + "008080" + "000000".repeat(10));
  assert.ok(f);
  assert.deepEqual(f[0], { c: [255, 0, 0], level: 1 });
  assert.deepEqual(f[1], { c: [0, 255, 255], level: 128 / 255 });
  assert.deepEqual(f[2], { c: null, level: 0 });
  assert.equal(f.length, 12);
});

test("not a ring frame", () => {
  assert.equal(frameFromHex(""), null);
  assert.equal(frameFromHex("ff".repeat(35)), null);
  assert.equal(frameFromHex("zz".repeat(36)), null);
});
