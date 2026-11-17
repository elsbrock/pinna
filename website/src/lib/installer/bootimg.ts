// Mark a boot image with the boot partition it goes into (ADR 0019): the
// same as `pinna_bootimg::tag_slot` and `tag_slot` in flash-gen2.sh.
//
// The header's command line (bytes 64-575) gets `pinna.boot=a` or `=b`. The
// kernel ignores it, but LK passes it on, so pinna-init sees which boot
// partition LK really loaded.

const CMDLINE_START = 64;
const CMDLINE_END = 576;
const TAG = "pinna.boot=";

/** A copy of `image` marked for `slot`; an earlier mark is replaced. */
export function tagSlot(image: Uint8Array, slot: "a" | "b"): Uint8Array {
  const magic = String.fromCharCode(...image.subarray(0, 8));
  if (magic !== "ANDROID!") throw new Error("not a boot image (no ANDROID! magic)");
  const field = image.subarray(CMDLINE_START, CMDLINE_END);
  const end = field.indexOf(0);
  const old = String.fromCharCode(...(end === -1 ? field : field.subarray(0, end)));
  const words = old.split(/\s+/).filter((w) => w && !w.startsWith(TAG));
  words.push(`${TAG}${slot}`);
  const line = words.join(" ");
  if (line.length >= CMDLINE_END - CMDLINE_START) throw new Error("the boot image's command line is full");

  const out = image.slice();
  out.fill(0, CMDLINE_START, CMDLINE_END);
  for (let i = 0; i < line.length; i++) out[CMDLINE_START + i] = line.charCodeAt(i);
  return out;
}

/** The slot an image is marked for, if any. */
export function slotTag(image: Uint8Array): "a" | "b" | null {
  const field = image.subarray(CMDLINE_START, CMDLINE_END);
  const end = field.indexOf(0);
  const line = String.fromCharCode(...(end === -1 ? field : field.subarray(0, end)));
  const word = line.split(/\s+/).find((w) => w.startsWith(TAG));
  const s = word?.slice(TAG.length);
  return s === "a" || s === "b" ? s : null;
}
