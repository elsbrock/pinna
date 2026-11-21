// Get a release from the release service (ADR 0022): the channel's
// manifest, its signature checked with WebCrypto against the project key,
// then the boot and root image, each checked against the manifest's size
// and SHA-256 before the page uses it. Picking downloaded files
// (release.ts) stays for builds of one's own.
import { sha256, type Release, type ReleaseImage } from "./release.ts";

export const SERVICE = "https://updates.getpinna.dev";

export type Channel = "stable" | "beta";

type Artifact = { url: string; size: number; sha256: string };
type Manifest = { version: string; channel: string; summary?: string; root: Artifact; boot?: Artifact };

export class DownloadError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DownloadError";
  }
}

const MANIFEST_MAX = 1 << 20;

/** A public key as `keys/release.pub` holds it: 64 hex characters. */
export function parseKey(text: string): Uint8Array {
  const hex = text.trim();
  if (!/^[0-9a-f]{64}$/i.test(hex)) throw new DownloadError("The release key is not 64 hex characters.");
  return Uint8Array.from(hex.match(/../g)!, (b) => Number.parseInt(b, 16));
}

/** Ed25519 over the manifest's exact bytes; false for a wrong signature. */
export async function verifySignature(manifest: Uint8Array, signature: Uint8Array, key: Uint8Array): Promise<boolean> {
  if (signature.length !== 64) return false;
  let publicKey: CryptoKey;
  try {
    publicKey = await crypto.subtle.importKey("raw", key as Uint8Array<ArrayBuffer>, { name: "Ed25519" }, false, ["verify"]);
  } catch {
    throw new DownloadError(
      "This browser can't check the release's signature: it has no Ed25519 in WebCrypto. Update the browser, or download the files and choose them below.",
    );
  }
  return crypto.subtle.verify(
    { name: "Ed25519" },
    publicKey,
    signature as Uint8Array<ArrayBuffer>,
    manifest as Uint8Array<ArrayBuffer>,
  );
}

type Options = {
  fetch?: typeof fetch;
  service?: string;
  /** What the page is doing, for its status line. */
  progress?: (text: string) => void;
};

/** A response's body, at most `max` bytes, reporting progress for large ones. */
async function body(res: Response, max: number, progress?: (done: number) => void): Promise<Uint8Array> {
  const out = new Uint8Array(max);
  let at = 0;
  const reader = res.body?.getReader();
  if (!reader) return new Uint8Array(await res.arrayBuffer());
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (at + value.length > max) {
      await reader.cancel();
      throw new DownloadError(`${res.url} is larger than the release says (${max} bytes).`);
    }
    out.set(value, at);
    at += value.length;
    progress?.(at);
  }
  return out.subarray(0, at);
}

async function get(f: typeof fetch, url: string, what: string): Promise<Response> {
  let res: Response;
  try {
    res = await f(url);
  } catch {
    throw new DownloadError(`${new URL(url).host} did not answer. Check the connection, or download the files and choose them below.`);
  }
  if (!res.ok) {
    const text = (await res.text().catch(() => "")).trim();
    const why = text.length > 0 && text.length <= 200 && !text.startsWith("<") ? text : `it answered ${res.status}`;
    throw new DownloadError(`Can't get ${what}: ${why}`);
  }
  return res;
}

const MiB = (n: number) => (n / 2 ** 20).toFixed(0);

async function image(f: typeof fetch, base: string, a: Artifact, what: string, progress?: (t: string) => void): Promise<ReleaseImage> {
  const url = new URL(a.url, base).toString();
  const res = await get(f, url, what);
  const bytes = await body(res, a.size, (done) => progress?.(`Downloading ${what}: ${MiB(done)} of ${MiB(a.size)} MiB`));
  if (bytes.length !== a.size) {
    throw new DownloadError(`The ${what} has ${bytes.length} bytes; the release says ${a.size}. Try again.`);
  }
  progress?.(`Checking ${what}`);
  const hash = await sha256(bytes);
  if (hash !== a.sha256.toLowerCase()) throw new DownloadError(`The ${what} doesn't match the release. Try again.`);
  return { name: a.url.split("/").pop() ?? a.url, picked: url, sha256: hash, bytes };
}

/**
 * The manifest of the newest release on `channel`, its signature checked
 * with `key`, and the URL its files are relative to. Fetches no image.
 */
export async function fetchManifest(channel: Channel, key: Uint8Array, opts: Options = {}): Promise<{ manifest: Manifest; url: string }> {
  const f = opts.fetch ?? fetch;
  const service = opts.service ?? SERVICE;
  opts.progress?.("Looking for the newest release");
  const res = await get(f, `${service}/v1/channels/${channel}`, `the newest release on ${channel}`);
  // Files sit next to the manifest the channel redirected to.
  const manifestUrl = res.url || `${service}/v1/channels/${channel}`;
  const bytes = await body(res, MANIFEST_MAX);
  const sigRes = await get(f, `${manifestUrl}.sig`, "the release's signature");
  const sig = await body(sigRes, 1024);
  if (!(await verifySignature(bytes, sig, key))) {
    throw new DownloadError("The release's signature doesn't match Pinna's release key. Nothing was installed.");
  }
  let m: Manifest;
  try {
    m = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new DownloadError("The release's manifest is not valid JSON.");
  }
  if (typeof m.version !== "string" || !m.root?.url || !m.boot?.url) {
    throw new DownloadError("The release's manifest names no boot image or no root image.");
  }
  if (channel === "stable" && m.channel !== "stable") {
    throw new DownloadError(`The release service offered the beta ${m.version} on stable.`);
  }
  return { manifest: m, url: manifestUrl };
}

/**
 * The newest release on `channel`, checked: the manifest's signature with
 * `key`, then each image against the manifest.
 */
export async function fetchRelease(channel: Channel, key: Uint8Array, opts: Options = {}): Promise<Release & { summary: string }> {
  const f = opts.fetch ?? fetch;
  const { manifest: m, url } = await fetchManifest(channel, key, opts);
  const boot = await image(f, url, m.boot!, "boot image", opts.progress);
  const root = await image(f, url, m.root, "root image", opts.progress);
  return { version: m.version, summary: m.summary ?? "", boot, root };
}
