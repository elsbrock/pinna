import assert from "node:assert/strict";
import { test } from "node:test";

import { discussionUrl, issueUrl, logBlock, REPORT_MAX, satelliteDetails, scrub } from "./report.ts";
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
const at = { now: new Date("2026-11-20T20:44:10Z"), offset: 60 };
const updates = {
  slots: [
    { name: "a", booted: false },
    { name: "b", booted: true },
  ],
};
const details = (st: Parameters<typeof satelliteDetails>[0], u: typeof updates | null) => satelliteDetails(st, u, at);

test("the details name the version, model, slot, last start, mode and channel, and nothing personal", () => {
  const md = details(dot, updates);
  assert.equal(
    md,
    [
      "- Pinna: 0.5.0-beta.2",
      "- Device: Echo Dot (2nd gen)",
      "- Running slot: b",
      "- Last start: a restart (update)",
      "- Mode: Home Assistant",
      "- Update channel: Beta",
      "- Copied: 2026-11-20 21:44, UTC+01:00, the zone of the log's times",
    ].join("\n"),
  );
  assert.ok(!md.includes("f0:27") && !md.includes("kitchen"));
});

test("the details say what they don't know", () => {
  const md = satelliteDetails(
    { version: "1.0.0", model: "", virtualDevice: true, mode: 1, settings: { updateChannel: CHANNEL.stable } },
    null,
    { now: at.now, offset: -330 },
  );
  assert.match(md, /^- Device: Virtual Echo Dot$/m);
  assert.match(md, /^- Running slot: unknown$/m);
  assert.match(md, /^- Last start: unknown$/m);
  assert.match(md, /^- Mode: Standalone$/m);
  assert.match(md, /^- Update channel: Stable$/m);
  assert.match(md, /^- Copied: 2026-11-20 15:14, UTC-05:30, the zone of the log's times$/m);
  assert.match(details({ ...dot, virtualDevice: true }, updates), /^- Device: Virtual Echo Dot$/m);
  assert.match(details({ ...dot, model: "echo-2" }, updates), /^- Device: Echo \(2nd gen\)$/m);
  assert.match(details({ ...dot, model: "kindle" }, { slots: [] }), /^- Device: Unknown \(kindle\)$\n- Running slot: none$/m);
});

test("the log is a collapsed section, and nothing without lines", () => {
  assert.equal(
    logBlock(["21:16:06 INFO  microphone running"]).markdown,
    ["<details>", "<summary>Log, 1 line</summary>", "", "```text", "21:16:06 INFO  microphone running", "```", "", "</details>", ""].join(
      "\n",
    ),
  );
  assert.deepEqual(logBlock([]), { markdown: "", kept: 0 });
});

test("the issue link opens the form with the details, and fits a URL", () => {
  const md = details(dot, updates);
  const url = new URL(issueUrl(md));
  assert.equal(url.origin + url.pathname, "https://github.com/elsbrock/pinna/issues/new");
  assert.equal(url.searchParams.get("template"), "problem.yml");
  assert.equal(url.searchParams.get("satellite"), md);
  // Spaces as %20, not +: what a form field gets is unambiguous.
  assert.ok(!url.search.includes("+"));
  assert.ok(url.href.length < 2000, `${url.href.length} characters`);
});

test("a discussion starts with one line about the satellite, or without one", () => {
  const url = new URL(discussionUrl("q-a", dot));
  assert.equal(url.origin + url.pathname, "https://github.com/elsbrock/pinna/discussions/new");
  assert.equal(url.searchParams.get("category"), "q-a");
  assert.equal(url.searchParams.get("body"), "\n\n---\nPinna 0.5.0-beta.2 · Echo Dot (2nd gen) · Home Assistant");
  assert.equal(discussionUrl("general", null), "https://github.com/elsbrock/pinna/discussions/new?category=general");
});

test("the log loses MAC and IP addresses, but not loopback, ports, times or versions", () => {
  assert.equal(
    scrub("pinnad 0.1.0 as pinna-6d484b (02:7f:57:6d:48:4b) at 21:16:06, version 2026.11.17.2043"),
    "pinnad 0.1.0 as pinna-6d484b ([MAC]) at 21:16:06, version 2026.11.17.2043",
  );
  assert.equal(scrub("bluealsa: AA:BB:CC:DD:EE:FF connected"), "bluealsa: [MAC] connected");
  assert.equal(scrub("Pairing for /org/bluez/hci0/dev_44_63_B6_7A_85_8B: accepted"), "Pairing for /org/bluez/hci0/dev_[MAC]: accepted");
  assert.equal(scrub("lease for 44-63-b6-7a-85-8b"), "lease for [MAC]");
  // The default names carry the satellite's own MAC's last three bytes.
  assert.equal(
    scrub("published _esphomelib._tcp as pinna-6d484b, friendly name Echo Dot 6D484B, not 6d484bc", { ownMac: "02:7f:57:6d:48:4b" }),
    "published _esphomelib._tcp as pinna-[MAC], friendly name Echo Dot [MAC], not 6d484bc",
  );
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
    scrub("Connected to Home 5G. Home 5G-guest is saved. Homework, a ab.", { wifi: ["Home 5G", "Home 5G-guest", "ab"] }),
    "Connected to [Wi-Fi network]. [Wi-Fi network] is saved. Homework, a ab.",
  );
  assert.equal(scrub("joined net (2.4)", { wifi: ["net (2.4)"] }), "joined [Wi-Fi network]");
});

test("the log block scrubs the lines it holds", () => {
  const log = logBlock(
    ['Wi-Fi: trying "Home" (hidden: false)', "Home refused the password.", "gateway 10.0.0.1", "pinnad as pinna-aabbcc (f0:27:2d:aa:bb:cc)"],
    { wifi: ["Home"], ownMac: dot.mac },
  ).markdown;
  assert.ok(!log.includes("Home") && !log.includes("10.0.0.1") && !log.includes("aabbcc"), log);
  assert.match(log, /^Wi-Fi: trying "\[Wi-Fi network\]" \(hidden: false\)$/m);
});

test("a log too long for an issue keeps its newest lines", () => {
  const log = Array.from({ length: 2000 }, (_, i) => `21:16:06 INFO  line ${i} ${"x".repeat(60)}`);
  const { markdown: md, kept } = logBlock(log);
  assert.ok(md.length <= REPORT_MAX, `${md.length} characters`);
  assert.ok(md.includes(`<summary>Log, the newest ${kept} of 2,000 lines</summary>`));
  assert.ok(kept > 500 && kept < 2000, `kept ${kept}`);
  assert.ok(md.includes(`line 1999 `) && md.includes(`line ${2000 - kept} `) && !md.includes(`line ${1999 - kept} `));
  assert.ok(md.endsWith("```\n\n</details>\n"));
});

test("backticks in the log don't end its code block", () => {
  const md = logBlock(["a ``` b", "c ```` d"]).markdown;
  assert.match(md, /^`````text$/m);
  assert.ok(md.endsWith("\n`````\n\n</details>\n"));
});
