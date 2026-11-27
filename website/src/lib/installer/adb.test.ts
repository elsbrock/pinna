// Run: node --test website/src/lib/installer/*.test.ts

import assert from "node:assert/strict";
import { test } from "node:test";
import { Adb, AdbError, parseBanner } from "./adb.ts";
import { FakeAdbd, TWRP_BANNER } from "./fake-adbd.ts";
import { InstallError } from "./install.ts";
import { sha256 } from "./release.ts";
import { assessTwrp, CommandError, DEVICE_TYPE_ID, identifyTwrp, installTwrp, parseVolumes, type TwrpReport } from "./twrp.ts";

const pattern = (n: number, seed = 3) => Uint8Array.from({ length: n }, (_, i) => (i * 7 + seed) & 0xff);

test("the banner gives the state, the properties and the features", () => {
  const b = parseBanner(TWRP_BANNER + "\0");
  assert.equal(b.state, "recovery");
  assert.equal(b.props["ro.product.device"], "cannoli");
  assert.equal(b.props["ro.product.model"], "Echo Dot (5th Gen)");
  assert.deepEqual(b.features, ["shell_v2", "cmd", "stat_v2"]);
  assert.equal(b.props.features, undefined);
});

test("a command returns its output and exit status", async () => {
  const d = new FakeAdbd({ run: (c) => (c === "getprop ro.twrp.version" ? { stdout: "3.7.0_9-0\n", exit: 0 } : { stderr: "nope", exit: 1 }) });
  const adb = new Adb(d);
  await adb.connect();
  assert.deepEqual(await adb.shell("getprop ro.twrp.version"), { exit: 0, stdout: "3.7.0_9-0\n", stderr: "" });
  assert.deepEqual(await adb.shell("false"), { exit: 1, stdout: "", stderr: "nope" });
  assert.deepEqual(d.commands, ["getprop ro.twrp.version", "false"]);
});

test("stdin arrives whole, in payloads within the device's size, one WRTE at a time", async () => {
  // The fake throws when a WRTE goes out before the OKAY for the one before,
  // or a payload is larger than it announced.
  const d = new FakeAdbd({ maxPayload: 4096, run: () => ({ stdout: "ok", exit: 0 }) });
  const adb = new Adb(d);
  await adb.connect();
  const data = pattern(4096 * 5 + 123);
  const sent: number[] = [];
  const r = await adb.shell("cat > /tmp/x", { stdin: data, onSent: (n) => sent.push(n) });
  assert.equal(r.exit, 0);
  assert.deepEqual(d.stdins[0], data);
  assert.equal(sent.at(-1), data.length);
  assert.ok(sent.length > 5, "progress for each payload");
});

test("output split across WRTEs, and across shell packets, is put back together", async () => {
  const text = "b: good\n".repeat(40);
  const d = new FakeAdbd({ chunk: 7, run: () => ({ stdout: text, stderr: "warning", exit: 3 }) });
  const adb = new Adb(d);
  await adb.connect();
  assert.deepEqual(await adb.shell("slot-status"), { exit: 3, stdout: text, stderr: "warning" });
});

test("a command that ends before its input is read returns its status", async () => {
  const d = new FakeAdbd({ maxPayload: 4096, exitAfter: () => 8192, run: () => ({ stderr: "no space", exit: 1 }) });
  const adb = new Adb(d);
  await adb.connect();
  const r = await adb.shell("write-volume", { stdin: pattern(4096 * 10) });
  assert.equal(r.exit, 1);
  assert.equal(r.stderr, "no space");
  assert.ok(d.stdins[0].length < 4096 * 10);
});

test("a device that asks for a key stops the page", async () => {
  const adb = new Adb(new FakeAdbd({ auth: true, run: () => ({ exit: 0 }) }));
  await assert.rejects(adb.connect(), (e: unknown) => e instanceof AdbError && e.kind === "auth");
  assert.ok(adb.broken);
});

test("a device without shell protocol v2 stops the page", async () => {
  const adb = new Adb(new FakeAdbd({ banner: "recovery::ro.product.device=cannoli;features=cmd", run: () => ({ exit: 0 }) }));
  await assert.rejects(adb.connect(), (e: unknown) => e instanceof AdbError && e.kind === "unsupported");
});

test("a command without an answer breaks the connection", async () => {
  const d = new FakeAdbd({ run: () => "hang" });
  const adb = new Adb(d);
  await adb.connect();
  await assert.rejects(adb.shell("sleep"), (e: unknown) => e instanceof AdbError && e.kind === "timeout");
  await assert.rejects(adb.shell("true"), (e: unknown) => e instanceof AdbError && e.kind === "transport");
  assert.deepEqual(d.commands, ["sleep"], "nothing more is sent");
});

// ---- The Echo Dot (5th gen) in TWRP ----

const VOLUMES = "ubi0_0 boot_a 13676544\nubi0_1 boot_b 13676544\nubi0_4 system_a 131088384\nubi0_6 misc 253952\nubi0_9 userdata 300000000\n";

/** TWRP on a C2N6L4, as the install script found it; `over` changes one command's outcome. */
function twrp(over: Record<string, { stdout?: string; stderr?: string; exit: number }> = {}) {
  return new FakeAdbd({
    run: (c) => {
      for (const [prefix, outcome] of Object.entries(over)) if (c.startsWith(prefix)) return outcome;
      if (c === "getprop ro.twrp.version") return { stdout: "3.7.0_9-0\r\n", exit: 0 };
      if (c.startsWith("cat /proc/idme/device_type_id")) return { stdout: `${DEVICE_TYPE_ID}\0`, exit: 0 };
      if (c.startsWith("for d in /sys/class/ubi")) return { stdout: VOLUMES, exit: 0 };
      if (c.startsWith("cat > /tmp/pinna-bootctl")) return { exit: 0 };
      if (c === "/tmp/pinna-bootctl storage-kind") return { stdout: "ubi\n", exit: 0 };
      if (c === "/tmp/pinna-bootctl bootloader") return { stdout: "amazon-uboot\n", exit: 0 };
      if (c.endsWith("slot-status")) return { stdout: "a: unbootable\nb: good (active)\n", exit: 0 };
      if (c.includes("write-volume")) return { stdout: "written and read back\n", exit: 0 };
      if (c.endsWith("set-active a")) return { exit: 0 };
      return { stderr: `${c}: not found`, exit: 127 };
    },
  });
}

async function checked(d: FakeAdbd): Promise<{ adb: Adb; report: TwrpReport }> {
  const adb = new Adb(d);
  const report = await identifyTwrp(adb, "G0000000000000AB");
  return { adb, report };
}

test("TWRP on an Echo Dot (5th gen) is ready", async () => {
  const { report } = await checked(twrp());
  assert.equal(report.twrp, "3.7.0_9-0");
  assert.equal(report.deviceTypeId, DEVICE_TYPE_ID);
  assert.equal(report.device, "cannoli");
  assert.equal(report.state, "recovery");
  assert.equal(report.volumes.length, 5);
  assert.equal(assessTwrp(report).state, "ready");
});

test("another device, a missing TWRP or a missing volume stops it", async () => {
  const other = await checked(twrp({ "cat /proc/idme": { stdout: "A2U21SRK4V1RIR\n", exit: 0 } }));
  assert.equal(assessTwrp(other.report).state, "stop");
  assert.match(assessTwrp(other.report).headline, /isn't an Echo Dot \(5th gen\)/);
  const noTwrp = await checked(twrp({ "getprop ro.twrp.version": { stdout: "\n", exit: 0 } }));
  assert.match(assessTwrp(noTwrp.report).headline, /doesn't run TWRP/);
  const noMisc = await checked(twrp({ "for d in": { stdout: "ubi0_0 boot_a 12648448\nubi0_4 system_a 131088384\n", exit: 0 } }));
  assert.match(assessTwrp(noMisc.report).detail, /misc/);
});

test("volumes are read from node, name and size lines", () => {
  assert.deepEqual(parseVolumes("ubi0_0 boot_a 12648448\nubi*_* cat: no such file\n\n"), [{ node: "ubi0_0", name: "boot_a", bytes: 12648448 }]);
});

const images = () => ({ boot: pattern(5000, 1), root: pattern(70_000, 2), bootctl: pattern(3000, 5) });

test("the install does what install-echo-dot-5.sh does, in the same order", async () => {
  const d = twrp();
  const { adb, report } = await checked(d);
  const img = images();
  const phases: string[] = [];
  await installTwrp(adb, report, img, (p) => phases.push(p.phase));
  const after = d.commands.slice(3);
  assert.deepEqual(after, [
    "cat > /tmp/pinna-bootctl.part && chmod 755 /tmp/pinna-bootctl.part && mv /tmp/pinna-bootctl.part /tmp/pinna-bootctl",
    "/tmp/pinna-bootctl storage-kind",
    "/tmp/pinna-bootctl bootloader",
    "/tmp/pinna-bootctl --misc /dev/ubi0_6 slot-status",
    `/tmp/pinna-bootctl write-volume boot_a --size 5000 --sha256 ${await sha256(img.boot)}`,
    `/tmp/pinna-bootctl write-volume system_a --size 70000 --sha256 ${await sha256(img.root)}`,
    "/tmp/pinna-bootctl --misc /dev/ubi0_6 set-active a",
  ]);
  assert.deepEqual(d.stdins[3], img.bootctl);
  assert.deepEqual(d.stdins[7], img.boot);
  assert.deepEqual(d.stdins[8], img.root);
  assert.equal(d.reboots, 1);
  assert.equal(phases.at(-1), "restarting");
});

test("a slot B that isn't good stops it before anything is written", async () => {
  const d = twrp({ "/tmp/pinna-bootctl --misc /dev/ubi0_6 slot-status": { stdout: "a: good (active)\nb: unbootable\n", exit: 0 } });
  const { adb, report } = await checked(d);
  await assert.rejects(installTwrp(adb, report, images()), (e: unknown) => e instanceof InstallError && /Slot B isn't marked good/.test(e.message));
  assert.ok(!d.commands.some((c) => c.includes("write-volume")));
  assert.equal(d.reboots, 0);
});

test("a failed write stops it before the slot record changes", async () => {
  const d = twrp({ "/tmp/pinna-bootctl write-volume system_a": { stderr: "system_a: read back sha256 differs\n", exit: 1 } });
  const { adb, report } = await checked(d);
  await assert.rejects(
    installTwrp(adb, report, images()),
    (e: unknown) => e instanceof CommandError && e.step === "Writing the root image to system_a" && /read back sha256 differs/.test(e.message),
  );
  assert.ok(!d.commands.some((c) => c.endsWith("set-active a")));
  assert.equal(d.reboots, 0);
});

test("an image bigger than its volume is refused before anything is sent", async () => {
  const d = twrp();
  const { adb, report } = await checked(d);
  const img = { ...images(), root: new Uint8Array(131088385) };
  await assert.rejects(installTwrp(adb, report, img), (e: unknown) => e instanceof InstallError && /bigger than system_a/.test(e.message));
  assert.equal(d.commands.length, 3, "only the check's commands");
});

test("a FIT over the 0xC1C000 bytes U-Boot reads is refused, though boot_a is larger", async () => {
  const d = twrp();
  const { adb, report } = await checked(d);
  const img = { ...images(), boot: new Uint8Array(0xc1c000 + 1) };
  await assert.rejects(installTwrp(adb, report, img), (e: unknown) => e instanceof InstallError && /bootloader reads/.test(e.message));
  assert.equal(d.commands.length, 3, "only the check's commands");
});

test("a tool that doesn't see UBI storage stops it before anything is written", async () => {
  const d = twrp({ "/tmp/pinna-bootctl storage-kind": { stdout: "gpt\n", exit: 0 } });
  const { adb, report } = await checked(d);
  await assert.rejects(installTwrp(adb, report, images()), (e: unknown) => e instanceof InstallError && /not UBI/.test(e.message));
  assert.ok(!d.commands.some((c) => c.includes("write-volume")));
});
