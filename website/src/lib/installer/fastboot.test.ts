// Run: node --test website/src/lib/installer/

import assert from "node:assert/strict";
import { test } from "node:test";
import { Fastboot, FastbootError } from "./fastboot.ts";
import { ScriptedDevice } from "./scripted-device.ts";

test("getvar returns the text after OKAY", async () => {
  const device = new ScriptedDevice({ "getvar:product": ["OKAYBISCUIT"] });
  const fb = new Fastboot(device);
  assert.equal(await fb.getvar("product"), "BISCUIT");
  assert.deepEqual(device.sent, ["getvar:product"]);
});

test("INFO and TEXT lines are collected until OKAY", async () => {
  const device = new ScriptedDevice({ "oem kaeru-version": ["INFOkaeru v1.2", "TEXTbuilt today", "OKAY"] });
  const reply = await new Fastboot(device).command("oem kaeru-version");
  assert.deepEqual(reply, { message: "", info: ["kaeru v1.2", "built today"] });
});

test("FAIL throws a fail error and leaves the connection usable", async () => {
  const device = new ScriptedDevice({
    "getvar:unlock_status": ["FAILGetVar Variable Not found"],
    "getvar:product": ["OKAYRADAR"],
  });
  const fb = new Fastboot(device);
  await assert.rejects(fb.getvar("unlock_status"), (e: unknown) => {
    assert.ok(e instanceof FastbootError);
    assert.equal(e.kind, "fail");
    assert.equal(e.message, "GetVar Variable Not found");
    return true;
  });
  assert.equal(fb.broken, null);
  assert.equal(await fb.getvar("product"), "RADAR");
});

test("getvarOrNull maps FAIL to null", async () => {
  const fb = new Fastboot(new ScriptedDevice({ "getvar:lk_build_desc": ["FAILnope"] }));
  assert.equal(await fb.getvarOrNull("lk_build_desc"), null);
});

test("no reply breaks the connection: nothing more is sent", async () => {
  const device = new ScriptedDevice({ "getvar:product": "timeout", "getvar:serialno": ["OKAYX"] });
  const fb = new Fastboot(device, 10);
  await assert.rejects(fb.getvar("product"), (e: unknown) => e instanceof FastbootError && e.kind === "timeout");
  assert.match(fb.broken ?? "", /no reply to getvar:product/);
  await assert.rejects(fb.getvar("serialno"), (e: unknown) => e instanceof FastbootError && e.kind === "transport");
  assert.deepEqual(device.sent, ["getvar:product"]);
});

test("the reply text ends at the first NUL", async () => {
  const fb = new Fastboot(new ScriptedDevice({ "getvar:product": ["OKAYBISCUIT\0garbage"] }));
  assert.equal(await fb.getvar("product"), "BISCUIT");
});

test("an unknown status is a protocol error", async () => {
  const fb = new Fastboot(new ScriptedDevice({ "getvar:product": ["WHAT"] }));
  await assert.rejects(fb.getvar("product"), (e: unknown) => e instanceof FastbootError && e.kind === "protocol");
  assert.notEqual(fb.broken, null);
});

test("commands longer than 64 bytes or with control characters are refused unsent", async () => {
  const device = new ScriptedDevice({});
  const fb = new Fastboot(device);
  await assert.rejects(fb.command("getvar:" + "x".repeat(60)), (e: unknown) => e instanceof FastbootError && e.kind === "protocol");
  await assert.rejects(fb.command("reboot\n"), (e: unknown) => e instanceof FastbootError && e.kind === "protocol");
  assert.deepEqual(device.sent, []);
  assert.equal(fb.broken, null);
});
