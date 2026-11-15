// The virtual Echo Dot's LED ring as pinnad drew it (ADR 0014): 72 hex digits,
// `rrggbb` per pixel in ring order, to the frame the dashboard's ring draws.
import { RING_PIXELS, type Frame } from "../../../design/ring.ts";

/** `null` when the text is not a ring frame. */
export function frameFromHex(hex: string): Frame | null {
  if (hex.length !== RING_PIXELS * 6 || !/^[0-9a-f]+$/i.test(hex)) return null;
  return Array.from({ length: RING_PIXELS }, (_, i) => {
    const [r, g, b] = [0, 2, 4].map((o) => parseInt(hex.slice(i * 6 + o, i * 6 + o + 2), 16));
    const max = Math.max(r, g, b);
    // The colour at full strength, and how bright the LED is.
    if (max === 0) return { c: null, level: 0 };
    const full = (v: number) => Math.round((v * 255) / max);
    return { c: [full(r), full(g), full(b)] as const, level: max / 255 };
  });
}
