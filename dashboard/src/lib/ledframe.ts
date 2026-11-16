// The LED ring as the satellite shows it (WatchRing): 3 bytes, red, green and
// blue, per pixel in ring order, to the frame the dashboard's ring draws.
import { RING_PIXELS, type Frame } from "../../../design/ring.ts";

/** `null` when the bytes are not a ring frame. */
export function frameFromBytes(rgb: Uint8Array): Frame | null {
  if (rgb.length !== RING_PIXELS * 3) return null;
  return Array.from({ length: RING_PIXELS }, (_, i) => {
    const [r, g, b] = rgb.subarray(i * 3, i * 3 + 3);
    const max = Math.max(r, g, b);
    // The colour at full strength, and how bright the LED is.
    if (max === 0) return { c: null, level: 0 };
    const full = (v: number) => Math.round((v * 255) / max);
    return { c: [full(r), full(g), full(b)] as const, level: max / 255 };
  });
}
