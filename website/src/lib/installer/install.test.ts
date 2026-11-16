// Run: node --test website/src/lib/installer/*.test.ts

import assert from "node:assert/strict";
import { test } from "node:test";
import { identify } from "./device.ts";
import { Fastboot, FastbootError } from "./fastboot.ts";
import { install, InstallError, plan } from "./install.ts";
import { matchRelease, sha256 } from "./release.ts";
import { ScriptedDevice, type Script } from "./scripted-device.ts";
import { BLOCK, isSparse, payloads, SPARSE_MAGIC, type Payload } from "./sparse.ts";

const join = (p: Payload) => {
  const out = new Uint8Array(p.size);
  let at = 0;
  for (const part of p.parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
};

/** Apply one download to `target` as LK does: raw as it is, sparse by its chunks. */
function apply(target: Uint8Array, data: Uint8Array): void {
  const v = new DataView(data.buffer, data.byteOffset, data.byteLength);
  if (v.getUint32(0, true) !== SPARSE_MAGIC) {
    target.set(data, 0);
    return;
  }
  const blockSize = v.getUint32(12, true);
  const chunks = v.getUint32(20, true);
  let at = v.getUint16(8, true);
  let block = 0;
  for (let i = 0; i < chunks; i++) {
    const type = v.getUint16(at, true);
    const blocks = v.getUint32(at + 4, true);
    const size = v.getUint32(at + 8, true);
    if (type === 0xcac1) target.set(data.subarray(at + 12, at + size), block * blockSize);
    else assert.equal(type, 0xcac3);
    block += blocks;
    at += size;
  }
  assert.equal(block, v.getUint32(16, true), "the chunks cover the whole image");
}

const pattern = (n: number) => Uint8Array.from({ length: n }, (_, i) => (i * 7 + 3) & 0xff);

test("an image that fits goes as one raw download", () => {
  const image = pattern(10_000);
  const [only, ...rest] = payloads(image, 10_000);
  assert.equal(rest.length, 0);
  assert.equal(only.parts[0], image);
});

test("a bigger image goes as sparse pieces that rebuild it, each within the limit", () => {
  const image = pattern(10 * BLOCK + 123);
  const max = 3 * BLOCK + 28 + 36;
  const pieces = payloads(image, max);
  assert.equal(pieces.length, 4);
  const target = new Uint8Array(11 * BLOCK).fill(0xee);
  for (const p of pieces) {
    assert.ok(p.size <= max);
    const data = join(p);
    assert.ok(isSparse(data));
    apply(target, data);
  }
  assert.deepEqual(target.subarray(0, image.length), image);
  assert.ok(target.subarray(image.length).every((b) => b === 0), "the last block is padded with zeros");
});

test("download sends the size, then the data, and waits for OKAY", async () => {
  const device = new ScriptedDevice({});
  const fb = new Fastboot(device, 10);
  const seen: number[] = [];
  await fb.download([pattern(5), pattern(3)], (n) => seen.push(n));
  assert.deepEqual(device.sent, ["download:00000008"]);
  assert.deepEqual(device.downloads[0], Uint8Array.from([...pattern(5), ...pattern(3)]));
  assert.deepEqual(seen, [5, 8]);
});

test("a refused download is a FAIL; a wrong size breaks the connection", async () => {
  const refused = new Fastboot(new ScriptedDevice({ "download:00000004": ["FAILdata too large"] }), 10);
  await assert.rejects(refused.download([pattern(4)]), (e: unknown) => e instanceof FastbootError && e.kind === "fail");
  assert.equal(refused.broken, null);

  const wrong = new Fastboot(new ScriptedDevice({ "download:00000004": ["DATA00000002"] }), 10);
  await assert.rejects(wrong.download([pattern(4)]), (e: unknown) => e instanceof FastbootError && e.kind === "protocol");
  assert.notEqual(wrong.broken, null);
});

// A release as GitHub serves it, picked by a user whose browser renamed one file.
async function release(boot: Uint8Array, root: Uint8Array) {
  const sums = `${await sha256(root)}  pinna-0.3.0.root.img\n${await sha256(boot)}  pinna-boot.img\n${await sha256(pattern(9))}  manifest.json\n`;
  return {
    sums: new File([sums], "SHA256SUMS"),
    boot: new File([boot as Uint8Array<ArrayBuffer>], "pinna-boot (1).img"),
    root: new File([root as Uint8Array<ArrayBuffer>], "pinna-0.3.0.root.img"),
    manifest: new File([pattern(9) as Uint8Array<ArrayBuffer>], "manifest.json"),
  };
}

test("release files are recognised by their hash, whatever their names", async () => {
  const r = await release(pattern(100), pattern(300));
  const { release: found, problems } = await matchRelease([r.manifest, r.root, r.boot, r.sums]);
  assert.deepEqual(problems, []);
  assert.equal(found?.version, "0.3.0");
  assert.equal(found?.boot.name, "pinna-boot.img");
  assert.equal(found?.boot.picked, "pinna-boot (1).img");
  assert.deepEqual(found?.root.bytes, pattern(300));
});

test("missing or foreign release files are named", async () => {
  const r = await release(pattern(100), pattern(300));
  assert.match((await matchRelease([r.boot, r.root])).problems[0], /SHA256SUMS/);
  assert.match((await matchRelease([r.sums, r.boot])).problems[0], /pinna-0\.3\.0\.root\.img/);
  const other = new File([pattern(50) as Uint8Array<ArrayBuffer>], "pinna-boot.img");
  const { release: none, problems } = await matchRelease([r.sums, other, r.root]);
  assert.equal(none, null);
  assert.match(problems[0], /pinna-boot\.img isn't in this SHA256SUMS/);
});

const KAERU: Script = {
  "getvar:product": ["OKAYBISCUIT"],
  "getvar:serialno": ["OKAYG090LF0971521APS"],
  "getvar:lk_build_desc": ["OKAY63cb91b-20221007_072309"],
  "getvar:max-download-size": ["OKAY0x10000"],
  "getvar:unlocked": ["OKAY"],
  "getvar:partition-size:boot_a": ["OKAY1000000"],
  "getvar:partition-size:boot_b": ["OKAY1000000"],
  "getvar:partition-size:system_a": ["OKAY30000000"],
  "getvar:partition-size:system_b": ["OKAY30000000"],
  "getvar:partition-size:userdata": ["OKAY4effbe00"],
  "oem kaeru-version": ["INFOkaeru v2.0.0-gab93750", "OKAY"],
  "flash:system_a": ["OKAY"],
  "flash:system_b": ["OKAY"],
  "flash:userdata": ["OKAY"],
  "flash:boot_a": ["OKAY"],
  "flash:boot_b": ["OKAY"],
  reboot: ["OKAY"],
};

test("install writes what flash-gen2.sh writes, in its order, then restarts", async () => {
  const report = await identify(new Fastboot(new ScriptedDevice(KAERU), 10));
  const root = pattern(40 * BLOCK + 5);
  const boot = pattern(3 * BLOCK);
  const device = new ScriptedDevice(KAERU);
  await install(new Fastboot(device, 10), plan(report, { boot, root }));

  const flashes = device.sent.filter((c) => !c.startsWith("download:"));
  assert.deepEqual([...new Set(flashes)], ["flash:system_a", "flash:system_b", "flash:userdata", "flash:boot_a", "flash:boot_b", "reboot"]);
  assert.ok(device.sent.every((c, i) => c.startsWith("download:") === (i % 2 === 0) || c === "reboot"), "each flash follows its download");

  // Replay the downloads onto partitions and compare.
  const partitions: Record<string, Uint8Array> = {
    system_a: new Uint8Array(41 * BLOCK),
    system_b: new Uint8Array(2 << 20).fill(1),
    userdata: new Uint8Array(5 << 20).fill(1),
    boot_a: new Uint8Array(3 * BLOCK),
    boot_b: new Uint8Array(3 * BLOCK),
  };
  let d = 0;
  for (const command of device.sent) {
    if (command.startsWith("flash:")) apply(partitions[command.slice(6)], device.downloads[d++]);
  }
  assert.deepEqual(partitions.system_a.subarray(0, root.length), root);
  assert.ok(partitions.system_b.subarray(0, 1 << 20).every((b) => b === 0));
  assert.equal(partitions.system_b[1 << 20], 1, "only the first MiB of system_b is cleared");
  assert.ok(partitions.userdata.subarray(0, 4 << 20).every((b) => b === 0));
  assert.deepEqual(partitions.boot_a, boot);
  assert.deepEqual(partitions.boot_b, boot);
  for (const data of device.downloads) assert.ok(data.length <= 0x10000);
});

test("an image bigger than its partition stops the install before any write", async () => {
  const report = await identify(new Fastboot(new ScriptedDevice({ ...KAERU, "getvar:partition-size:boot_a": ["OKAY1000"] }), 10));
  assert.throws(() => plan(report, { boot: pattern(2 * BLOCK), root: pattern(BLOCK) }), InstallError);
});

test("a failed write stops the install there", async () => {
  const report = await identify(new Fastboot(new ScriptedDevice(KAERU), 10));
  const device = new ScriptedDevice({ ...KAERU, "flash:userdata": ["FAILwrite error"] });
  await assert.rejects(install(new Fastboot(device, 10), plan(report, { boot: pattern(BLOCK), root: pattern(BLOCK) })), /write error/);
  assert.equal(device.sent.at(-1), "flash:userdata");
});

test("a restart without a reply still finishes the install", async () => {
  const report = await identify(new Fastboot(new ScriptedDevice(KAERU), 10));
  const device = new ScriptedDevice({ ...KAERU, reboot: "timeout" });
  await install(new Fastboot(device, 10), plan(report, { boot: pattern(BLOCK), root: pattern(BLOCK) }));
  assert.equal(device.sent.at(-1), "reboot");
});
