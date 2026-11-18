import assert from "node:assert/strict";
import { test } from "node:test";

import { type ListOption, matching, step, typeahead } from "./listbox.ts";

const opts = (...labels: string[]): ListOption[] =>
  labels.map((label, i) => ({ id: String(i), label: label.replace("!", ""), disabled: label.endsWith("!") }));

test("the arrows move over the enabled options and stop at the ends", () => {
  const o = opts("None", "Low!", "Medium", "High");
  assert.equal(step(o, 0, "ArrowDown"), 2);
  assert.equal(step(o, 2, "ArrowUp"), 0);
  assert.equal(step(o, 3, "ArrowDown"), 3);
  assert.equal(step(o, 0, "ArrowUp"), 0);
  assert.equal(step(o, -1, "ArrowDown"), 0);
  assert.equal(step(o, -1, "ArrowUp"), 3);
});

test("Home, End and the page keys reach the first and last enabled options", () => {
  const o = opts("Choose!", "a", "b", "c", "d!");
  assert.equal(step(o, 2, "Home"), 1);
  assert.equal(step(o, 2, "End"), 3);
  assert.equal(step(o, 1, "PageDown"), 3);
  assert.equal(step(o, 3, "PageUp"), 1);
  assert.equal(step(o, 2, "Tab"), 2);
  assert.equal(step(opts("x!"), -1, "ArrowDown"), -1);
});

test("typing jumps to the next option that starts with it, and a repeated letter cycles", () => {
  const o = opts("Home Assistant", "Standalone", "Stable", "Off!", "off");
  assert.equal(typeahead(o, 0, "s"), 1);
  assert.equal(typeahead(o, 1, "s"), 2);
  assert.equal(typeahead(o, 2, "ss"), 1);
  assert.equal(typeahead(o, 1, "sta"), 1);
  assert.equal(typeahead(o, 1, "stab"), 2);
  assert.equal(typeahead(o, 0, "o"), 4);
  assert.equal(typeahead(o, 0, "x"), -1);
  assert.equal(typeahead(o, 0, ""), -1);
});

test("suggestions match the id or the label, the ones that start with it first", () => {
  const voices = [
    { id: "JBFqnCBsd6RMkjVDRZzb", label: "George" },
    { id: "onwK4e9ZLuTAKqWW03F9", label: "Daniel" },
    { id: "gpt-4o-mini", label: "gpt-4o-mini" },
    { id: "o4-mini", label: "o4-mini" },
  ];
  assert.deepEqual(
    matching(voices, "geo").map((c) => c.label),
    ["George"],
  );
  assert.deepEqual(
    matching(voices, "JBF").map((c) => c.label),
    ["George"],
  );
  assert.deepEqual(
    matching(voices, "mini").map((c) => c.id),
    ["gpt-4o-mini", "o4-mini"],
  );
  // One that starts with it comes before one that only contains it.
  assert.deepEqual(
    matching([...voices, { id: "mini-tts", label: "mini-tts" }], "mini").map((c) => c.id),
    ["mini-tts", "gpt-4o-mini", "o4-mini"],
  );
  assert.equal(matching(voices, "").length, 4);
  assert.equal(matching(voices, "", 2).length, 2);
});
