import assert from "node:assert/strict";
import { test } from "node:test";

import { issueReport, REPORT_MAX, scrub } from "./report.ts";
import { BOOT, CHANNEL } from "./view.ts";

const dot = {
  version: "0.5.0-beta.2",
  model: "echo-dot-2",
  virtualDevice: false,
  mode: 0,
  lastBoot: { outcome: BOOT.restarted, reason: "update", summary: "" },
  settings: { updateChannel: CHANNEL.beta },
  // Fields a Status has that the report must not show.
  mac: "f0:27:2d:aa:bb:cc",
  name: "kitchen",
};
const updates = {
  slots: [
    { name: "a", booted: false },
    { name: "b", booted: true },
  ],
};

test("the report names the version, model, slot, last start, mode and channel", () => {
  const md = issueReport(dot, updates, ["21:16:06 INFO  microphone running"]);
  assert.equal(
    md,
    [
      "- Pinna: 0.5.0-beta.2",
      "- Device: Echo Dot (2nd gen)",
      "- Running slot: b",
      "- Last start: a restart (update)",
      "- Mode: Home Assistant",
      "- Update channel: Beta",
      "",
      "<details>",
      "<summary>Log, 1 line</summary>",
      "",
      "```text",
      "21:16:06 INFO  microphone running",
      "```",
      "",
      "</details>",
      "",
    ].join("\n"),
  );
  assert.ok(!md.includes("f0:27") && !md.includes("kitchen"));
});

test("the report says what it doesn't know", () => {
  const md = issueReport(
    { version: "1.0.0", model: "", virtualDevice: true, mode: 1, settings: { updateChannel: CHANNEL.stable } },
    null,
    [],
  );
  assert.match(md, /^- Device: Virtual Echo Dot$/m);
  assert.match(md, /^- Running slot: unknown$/m);
  assert.match(md, /^- Last start: unknown$/m);
  assert.match(md, /^- Mode: Standalone$/m);
  assert.match(md, /^- Update channel: Stable$/m);
  assert.match(md, /<summary>Log, 0 lines<\/summary>/);
  assert.match(issueReport({ ...dot, virtualDevice: true }, updates, []), /^- Device: Virtual Echo Dot$/m);
  assert.match(issueReport({ ...dot, model: "echo-2" }, updates, []), /^- Device: Echo \(2nd gen\)$/m);
  assert.match(issueReport({ ...dot, model: "kindle" }, { slots: [] }, []), /^- Device: Unknown \(kindle\)$\n- Running slot: none$/m);
});

test("the log loses MAC and IP addresses, but not loopback, ports, times or versions", () => {
  assert.equal(
    scrub("pinnad 0.1.0 as pinna-6d484b (02:7f:57:6d:48:4b) at 21:16:06, version 2026.11.17.2043"),
    "pinnad 0.1.0 as pinna-6d484b ([MAC]) at 21:16:06, version 2026.11.17.2043",
  );
  assert.equal(scrub("bluealsa: AA:BB:CC:DD:EE:FF connected"), "bluealsa: [MAC] connected");
  assert.equal(scrub("got 192.168.1.23/24 via 192.168.1.1"), "got [IP]/24 via [IP]");
  assert.equal(scrub("control API listening on 127.0.0.1:7470, metrics on 0.0.0.0:9100"), "control API listening on 127.0.0.1:7470, metrics on 0.0.0.0:9100");
  assert.equal(scrub("address 2001:db8:85a3::8a2e:370:7334 and fe80::1%wlan0"), "address [IP] and [IP]%wlan0");
  assert.equal(scrub("full 2001:0db8:0000:0000:0000:ff00:0042:8329 here"), "full [IP] here");
  assert.equal(scrub("esphome api listening addr=[::]:6053, ::1 too"), "esphome api listening addr=[::]:6053, ::1 too");
  assert.equal(scrub("pinnad::api::logs failed: deadbeef"), "pinnad::api::logs failed: deadbeef");
});

test("the log loses Wi-Fi names", () => {
  // pinnad logs a name with Rust's {:?}, also one that is no longer saved.
  assert.equal(scrub('Wi-Fi: trying "Old \\"Flat\\"" (hidden: false)'), 'Wi-Fi: trying "[Wi-Fi network]" (hidden: false)');
  assert.equal(scrub('Wi-Fi: forgot "Café"'), 'Wi-Fi: forgot "[Wi-Fi network]"');
  // Known names go wherever they stand, the longer name first; tiny ones would blank out words.
  assert.equal(
    scrub("Connected to Home 5G. Home 5G-guest is saved. Homework, a ab.", ["Home 5G", "Home 5G-guest", "ab"]),
    "Connected to [Wi-Fi network]. [Wi-Fi network] is saved. Homework, a ab.",
  );
  assert.equal(scrub("joined net (2.4)", ["net (2.4)"]), "joined [Wi-Fi network]");
});

test("the report scrubs the log it holds", () => {
  const md = issueReport(dot, updates, ['Wi-Fi: trying "Home" (hidden: false)', "Home refused the password.", "gateway 10.0.0.1"], ["Home"]);
  const log = md.split("<details>")[1];
  assert.ok(!log.includes("Home") && !log.includes("10.0.0.1"), log);
  assert.match(log, /^Wi-Fi: trying "\[Wi-Fi network\]" \(hidden: false\)$/m);
  // "Home Assistant" in the facts is the mode, not the network.
  assert.match(md, /^- Mode: Home Assistant$/m);
});

test("a log too long for an issue keeps its newest lines", () => {
  const log = Array.from({ length: 2000 }, (_, i) => `21:16:06 INFO  line ${i} ${"x".repeat(60)}`);
  const md = issueReport(dot, updates, log);
  assert.ok(md.length <= REPORT_MAX, `${md.length} characters`);
  const kept = Number(/the newest ([\d,]+) of 2,000 lines/.exec(md)?.[1].replace(",", ""));
  assert.ok(kept > 500 && kept < 2000, `kept ${kept}`);
  assert.ok(md.includes(`line 1999 `) && md.includes(`line ${2000 - kept} `) && !md.includes(`line ${1999 - kept} `));
  assert.ok(md.endsWith("```\n\n</details>\n"));
});

test("backticks in the log don't end its code block", () => {
  const md = issueReport(dot, updates, ["a ``` b", "c ```` d"]);
  assert.match(md, /^`````text$/m);
  assert.ok(md.endsWith("\n`````\n\n</details>\n"));
});
