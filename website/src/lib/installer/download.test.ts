// Run: node --test website/src/lib/installer/*.test.ts

import assert from "node:assert/strict";
import { test } from "node:test";
import { DownloadError, fetchRelease, parseKey, verifySignature } from "./download.ts";
import { sha256 } from "./release.ts";

const SERVICE = "https://updates.test";
const BASE = `${SERVICE}/v1/releases/v0.4.0`;

async function keys() {
  const pair = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const sign = async (data: Uint8Array) =>
    new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, pair.privateKey, data as Uint8Array<ArrayBuffer>));
  return { raw, sign };
}

/** A release service with one release, as fetch answers it. */
async function service(over: { manifest?: object; sign?: (b: Uint8Array) => Promise<Uint8Array>; files?: Record<string, Uint8Array> } = {}) {
  const k = await keys();
  const boot = new Uint8Array(4096).fill(1);
  const root = new Uint8Array(3 * 4096).fill(2);
  const manifest = {
    version: "0.4.0",
    channel: "stable",
    summary: "Summary.",
    requires: { boot_level: 1 },
    root: { url: "pinna-0.4.0.root.img", size: root.length, sha256: await sha256(root) },
    boot: { url: "pinna-boot.img", size: boot.length, sha256: await sha256(boot) },
    ...over.manifest,
  };
  const bytes = new TextEncoder().encode(JSON.stringify(manifest, null, 2));
  const files: Record<string, Uint8Array> = {
    [`${BASE}/manifest.json`]: bytes,
    [`${BASE}/manifest.json.sig`]: await (over.sign ?? k.sign)(bytes),
    [`${BASE}/pinna-boot.img`]: boot,
    [`${BASE}/pinna-0.4.0.root.img`]: root,
    ...over.files,
  };
  const asked: string[] = [];
  const fake = (async (input: string | URL | Request) => {
    const url = String(input);
    asked.push(url);
    // The channel redirects to the release's manifest; fetch follows it.
    const at = url === `${SERVICE}/v1/channels/stable` ? `${BASE}/manifest.json` : url;
    const data = files[at];
    const res = data
      ? new Response(data as Uint8Array<ArrayBuffer>)
      : new Response(url.includes("/channels/") ? "No release on stable yet." : "Not found.", { status: 404 });
    Object.defineProperty(res, "url", { value: at });
    return res;
  }) as typeof fetch;
  return { key: k.raw, fetch: fake, asked, boot, root };
}

test("a release comes with a valid signature and checked images", async () => {
  const s = await service();
  const steps: string[] = [];
  const r = await fetchRelease("stable", s.key, { fetch: s.fetch, service: SERVICE, progress: (t) => steps.push(t) });
  assert.equal(r.version, "0.4.0");
  assert.equal(r.summary, "Summary.");
  assert.deepEqual(r.boot.bytes, s.boot);
  assert.deepEqual(r.root.bytes, s.root);
  assert.equal(r.root.name, "pinna-0.4.0.root.img");
  assert.deepEqual(s.asked, [
    `${SERVICE}/v1/channels/stable`,
    `${BASE}/manifest.json.sig`,
    `${BASE}/pinna-boot.img`,
    `${BASE}/pinna-0.4.0.root.img`,
  ]);
  assert.ok(steps.includes("Checking root image"));
});

test("a signature from another key, or none, stops before any image", async () => {
  const other = await keys();
  const s = await service({ sign: other.sign });
  await assert.rejects(fetchRelease("stable", s.key, { fetch: s.fetch, service: SERVICE }), /doesn't match Pinna's release key/);
  assert.equal(s.asked.length, 2, "no image was fetched");

  const t = await service({ sign: async () => new Uint8Array(0) });
  await assert.rejects(fetchRelease("stable", t.key, { fetch: t.fetch, service: SERVICE }), DownloadError);
});

test("an image that doesn't match the manifest is refused", async () => {
  const s = await service({ files: { [`${BASE}/pinna-0.4.0.root.img`]: new Uint8Array(3 * 4096).fill(3) } });
  await assert.rejects(fetchRelease("stable", s.key, { fetch: s.fetch, service: SERVICE }), /doesn't match the release/);
  const t = await service({ files: { [`${BASE}/pinna-boot.img`]: new Uint8Array(8192) } });
  await assert.rejects(fetchRelease("stable", t.key, { fetch: t.fetch, service: SERVICE }), /larger than the release says/);
});

test("the service's own sentence explains a missing release", async () => {
  const s = await service();
  await assert.rejects(
    fetchRelease("beta", s.key, { fetch: s.fetch, service: SERVICE }),
    /Can't get the newest release on beta: No release on stable yet\./,
  );
  const b = await service({ manifest: { version: "0.5.0-beta.1", channel: "beta" } });
  await assert.rejects(fetchRelease("stable", b.key, { fetch: b.fetch, service: SERVICE }), /offered the beta 0\.5\.0-beta\.1 on stable/);
});

test("keys and signatures", async () => {
  assert.equal(parseKey("11".repeat(32)).length, 32);
  assert.throws(() => parseKey("11"), DownloadError);
  const k = await keys();
  const data = new TextEncoder().encode("{}");
  assert.equal(await verifySignature(data, await k.sign(data), k.raw), true);
  assert.equal(await verifySignature(new TextEncoder().encode("{ }"), await k.sign(data), k.raw), false);
  assert.equal(await verifySignature(data, new Uint8Array(63), k.raw), false);
});
