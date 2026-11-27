// Recognise the release files the user picked (ADR 0016): by their SHA-256
// against the release's SHA256SUMS, not by their names, because browsers
// rename repeated downloads ("pinna-boot (1).img").
import type { ModelId } from "./device.ts";

/** The parts of a File this code uses; the tests pass Node's File. */
export type PickedFile = { name: string; size: number; arrayBuffer(): Promise<ArrayBuffer>; text(): Promise<string> };

export type ReleaseImage = {
  /** Its name in SHA256SUMS. */
  name: string;
  /** The name of the file the user picked. */
  picked: string;
  sha256: string;
  bytes: Uint8Array;
};

export type Release = {
  version: string | null;
  /** The model the files are for: the boot image, and the tool, differ by model. */
  model: ModelId;
  boot: ReleaseImage;
  root: ReleaseImage;
  /** pinna-bootctl for the device, which the Echo Dot (5th gen)'s install runs in TWRP. */
  bootctl: ReleaseImage | null;
};

export type Match = { release: Release | null; problems: string[] };

const SUMS_MAX = 64 * 1024;
const ROOT_NAME = /^pinna-(.+)\.root\.img$/;

/** The release's files, by their names in SHA256SUMS, that each model needs beside the root. */
export const MODEL_FILES: Record<ModelId, { boot: string; bootctl: string | null }> = {
  "echo-dot-2": { boot: "pinna-boot.img", bootctl: null },
  "echo-2": { boot: "pinna-boot.img", bootctl: null },
  "echo-dot-5": { boot: "pinna-boot-mt8519.itb", bootctl: "pinna-bootctl-armv7" },
};

/** SHA256SUMS as a map from hash to name, or null if the text isn't one. */
export function parseSums(text: string): Map<string, string> | null {
  const sums = new Map<string, string>();
  for (const line of text.split("\n")) {
    if (line.trim() === "") continue;
    const m = /^([0-9a-f]{64}) [ *](\S+)$/i.exec(line.trimEnd());
    if (!m) return null;
    sums.set(m[1].toLowerCase(), m[2]);
  }
  return sums.size > 0 ? sums : null;
}

export async function sha256(bytes: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes as Uint8Array<ArrayBuffer>));
  return Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * Find SHA256SUMS among `files`, and the files it lists that `model` needs:
 * the root, the model's boot image, and for the Echo Dot (5th gen)
 * pinna-bootctl. Other files of the release (manifest.json, flash-gen2.sh)
 * are ignored.
 */
export async function matchRelease(files: PickedFile[], model: ModelId, onFile: (name: string) => void = () => {}): Promise<Match> {
  const want = MODEL_FILES[model];
  const problems: string[] = [];
  const sumsFiles: { file: PickedFile; sums: Map<string, string> }[] = [];
  const others: PickedFile[] = [];
  for (const file of files) {
    const sums = file.size <= SUMS_MAX ? parseSums(await file.text()) : null;
    if (sums) sumsFiles.push({ file, sums });
    else others.push(file);
  }

  if (sumsFiles.length === 0) {
    return { release: null, problems: ["Add SHA256SUMS from the same release. The page checks each file against it."] };
  }
  if (sumsFiles.length > 1) {
    return { release: null, problems: ["Choose one SHA256SUMS. These files come from more than one release."] };
  }
  const sums = sumsFiles[0].sums;

  let boot: ReleaseImage | null = null;
  let root: ReleaseImage | null = null;
  let bootctl: ReleaseImage | null = null;
  for (const file of others) {
    onFile(file.name);
    const bytes = new Uint8Array(await file.arrayBuffer());
    const hash = await sha256(bytes);
    const name = sums.get(hash);
    if (!name) {
      problems.push(`${file.name} isn't in this SHA256SUMS: it comes from another release, or the download is damaged. Download it again.`);
      continue;
    }
    const image = { name, picked: file.name, sha256: hash, bytes };
    if (name === want.boot) boot = image;
    else if (name === want.bootctl) bootctl = image;
    else if (ROOT_NAME.test(name)) root = image;
  }

  const listed = [...sums.values()];
  if (!boot) problems.push(`Add ${want.boot} from the same release.`);
  if (!root) problems.push(`Add ${listed.find((n) => ROOT_NAME.test(n)) ?? "pinna-<version>.root.img"} from the same release.`);
  if (want.bootctl && !bootctl) problems.push(`Add ${want.bootctl} from the same release.`);
  if (!boot || !root || (want.bootctl && !bootctl) || problems.length > 0) return { release: null, problems };
  return { release: { version: ROOT_NAME.exec(root.name)?.[1] ?? null, model, boot, root, bootctl }, problems: [] };
}
