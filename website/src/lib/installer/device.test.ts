// Run: node --test website/src/lib/installer/*.test.ts

import assert from "node:assert/strict";
import { test } from "node:test";
import { assess, formatReport, identify, maskSerial, type DeviceReport } from "./device.ts";
import { Fastboot, FastbootError } from "./fastboot.ts";
import { ScriptedDevice, type Script } from "./scripted-device.ts";

// What the owner's Echo Dot (2nd gen) with Kaeru answered (2026-11-15).
const KAERU_DOT: Script = {
  "getvar:product": ["OKAYBISCUIT"],
  "getvar:serialno": ["OKAYG090LF0971521APS"],
  "getvar:lk_build_desc": ["OKAY63cb91b-20221007_072309"],
  "getvar:max-download-size": ["OKAY0x6d00000"],
  "getvar:unlocked": ["OKAY"],
  "getvar:partition-size:boot_a": ["OKAY1000000"],
  "getvar:partition-size:boot_b": ["OKAY1000000"],
  "getvar:partition-size:system_a": ["OKAY30000000"],
  "getvar:partition-size:system_b": ["OKAY30000000"],
  "getvar:partition-size:userdata": ["OKAY4effbe00"],
  "oem kaeru-version": ["INFOkaeru 1.0", "OKAY"],
};

const stock = (overrides: Script = {}): Script => ({
  ...KAERU_DOT,
  "oem kaeru-version": ["FAILunknown command"],
  ...overrides,
});

async function run(script: Script): Promise<DeviceReport> {
  return identify(new Fastboot(new ScriptedDevice(script), 10));
}

test("a Dot with Kaeru is ready", async () => {
  const report = await run(KAERU_DOT);
  assert.deepEqual(report.bootloader, { kind: "kaeru", version: "kaeru 1.0" });
  assert.equal(report.partitions.system_a, 0x30000000);
  assert.equal(report.unlocked, "");
  assert.equal(assess(report, "echo-dot-2").state, "ready");
});

test("a stock Dot with a pinned LK build needs the unlock", async () => {
  const report = await run(stock());
  assert.equal(report.bootloader.kind, "stock");
  assert.equal(assess(report, "echo-dot-2").state, "needs-unlock");
});

test("a stock device with an LK build nobody pinned is refused, asking for a report", async () => {
  const report = await run(stock({ "getvar:product": ["OKAYRADAR"], "getvar:lk_build_desc": ["OKAY2c3569d-20260617_051433"] }));
  const a = assess(report, "echo-2");
  assert.equal(a.state, "unknown-build");
  assert.match(a.detail, /2c3569d-20260617_051433/);
});

test("a stock device without lk_build_desc can't be unlocked", async () => {
  const report = await run(stock({ "getvar:lk_build_desc": ["FAILnot found"] }));
  assert.equal(report.lkBuild, null);
  assert.equal(assess(report, "echo-dot-2").state, "unknown-build");
});

test("'restricted on locked hw' counts as a stock refusal", async () => {
  const report = await run(stock({ "oem kaeru-version": ["FAILrestricted on locked hw"] }));
  assert.equal(report.bootloader.kind, "stock");
});

test("any other refusal of oem kaeru-version stops the page", async () => {
  const report = await run(stock({ "oem kaeru-version": ["FAILsomething else"] }));
  assert.deepEqual(report.bootloader, { kind: "unknown", reply: "something else" });
  assert.equal(assess(report, "echo-dot-2").state, "stop");
});

test("an Echo (2nd gen) that reports BISCUIT runs the old boot chain", async () => {
  const a = assess(await run(KAERU_DOT), "echo-2");
  assert.equal(a.state, "stop");
  assert.match(a.detail, /old boot chain/);
});

test("a device that isn't BISCUIT or RADAR is refused", async () => {
  assert.equal(assess(await run({ ...KAERU_DOT, "getvar:product": ["OKAYKARNAK"] }), "echo-dot-2").state, "stop");
});

test("a missing partition stops a Kaeru device", async () => {
  const report = await run({ ...KAERU_DOT, "getvar:partition-size:boot_b": ["FAILno such partition"] });
  const a = assess(report, "echo-dot-2");
  assert.equal(a.state, "stop");
  assert.match(a.detail, /boot_b/);
});

test("no reply to the product stops before anything else is asked", async () => {
  const device = new ScriptedDevice({ ...KAERU_DOT, "getvar:product": "timeout" });
  await assert.rejects(identify(new Fastboot(device, 10)), (e: unknown) => e instanceof FastbootError && e.kind === "timeout");
  assert.deepEqual(device.sent, ["getvar:product"]);
});

test("the report masks the serial number", async () => {
  const report = await run(KAERU_DOT);
  const text = formatReport(report, "echo-dot-2", assess(report, "echo-dot-2"), "Chrome 142 on Linux");
  assert.doesNotMatch(text, /0971521APS/);
  assert.match(text, /serialno: G090LF…/);
  assert.match(text, /partition-size:userdata: 0x4effbe00/);
  assert.match(text, /result: ready/);
  assert.equal(maskSerial(null), "(none)");
});
