// The LED ring for screens: a port of crates/pinna-hal/src/anim.rs plus the
// geometry for drawing it. Framework-free, so the website and the dashboard
// share it. Keep ringFrame() in step with anim.rs (ring.test.ts mirrors its
// tests).

export const RING_PIXELS = 12;

export type Rgb = readonly [number, number, number];

/** LibreEcho's esphomed theme colours, as in anim.rs. */
export const LED = {
  listening: [72, 185, 255],
  thinking: [168, 115, 239],
  error: [239, 80, 80],
  muted: [255, 0, 0],
  speaking: [90, 220, 230],
  amber: [255, 160, 0],
  volume: [120, 200, 255],
} as const satisfies Record<string, Rgb>;

export type Effect =
  | "idle"
  | "listening"
  | "thinking"
  | "speaking"
  | "error"
  | "muted"
  | "volume"
  | "timer"
  | "timer-ringing"
  | "boot";

/** Effects with a natural end, in ms; the rest run until replaced. */
export const EFFECT_DURATION_MS = { error: 250 * 2 * 3, volume: 1500, boot: 1200 } as const;

/** One pixel: colour (null when dark) and LED duty 0..1. */
export type Pixel = { c: Rgb | null; level: number };
export type Frame = Pixel[];

const TAU = Math.PI * 2;
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
/** Smooth 0..1 oscillation with the given period (seconds). */
const wave = (t: number, period: number) => 0.5 - 0.5 * Math.cos((t / period) * TAU);
const solid = (c: Rgb | null, level: number): Frame =>
  Array.from({ length: RING_PIXELS }, () => ({ c, level }));
const dark = () => solid(null, 0);

/** A bright head with a fading tail rotating around the ring. */
function spin(c: Rgb, t: number, revS: number, tail: number): Frame {
  const pos = ((t / revS) % 1) * RING_PIXELS;
  return Array.from({ length: RING_PIXELS }, (_, i) => {
    const behind = (((pos - i) % RING_PIXELS) + RING_PIXELS) % RING_PIXELS;
    const level = Math.max(0, 1 - behind / tail);
    return { c, level: level * level };
  });
}

/** Meter / countdown arc from pixel 0, with a dim track for the rest. */
function arc(c: Rgb, fraction: number): Frame {
  const lit = fraction * RING_PIXELS;
  return Array.from({ length: RING_PIXELS }, (_, i) => {
    const level = clamp(lit - i, 0, 1);
    return { c, level: level > 0 ? level : 0.04 };
  });
}

/**
 * The frame the satellite shows `tMs` after `effect` started.
 * `value`: volume 0..100 for "volume", fraction of the timer left 0..1 for "timer".
 */
export function ringFrame(effect: Effect, tMs: number, value?: number): Frame {
  const t = tMs / 1000;
  switch (effect) {
    case "listening":
      return solid(LED.listening, 0.3 + 0.7 * wave(t, 1.6));
    case "thinking":
      return spin(LED.thinking, t, 1.2, 5);
    case "speaking":
      return solid(LED.speaking, 0.25 + 0.55 * wave(t, 2.4));
    case "error":
      if (tMs >= EFFECT_DURATION_MS.error) return dark();
      return Math.floor(tMs / 250) % 2 === 0 ? solid(LED.error, 1) : dark();
    case "muted":
      return solid(LED.muted, 1);
    case "volume":
      if (tMs >= EFFECT_DURATION_MS.volume) return dark();
      return arc(LED.volume, Math.min(value ?? 0, 100) / 100);
    case "timer":
      return arc(LED.amber, clamp(value ?? 1, 0, 1));
    case "timer-ringing":
      return solid(LED.amber, 0.15 + 0.85 * wave(t, 0.5));
    case "boot":
      if (tMs >= EFFECT_DURATION_MS.boot) return dark();
      return spin(LED.listening, t, 0.6, 6);
    default:
      return dark();
  }
}

// ---------------------------------------------------------------------------
// Drawing. The SVG uses a 100x100 viewBox: puck r=49, pixels on r=36.

/**
 * Angle of pixel `i` in degrees, clockwise from 12 o'clock in a top-down view.
 * Pixels 10 and 11 straddle the front centre (pinna-hal FRONT_CENTRE), drawn at
 * the bottom; the index runs clockwise. The direction is not yet confirmed on
 * hardware: if it is the other way, change this function only.
 */
export const pixelAngle = (i: number) => 225 + 30 * i;

/**
 * SVG path for one pixel: a short arc centred on `angle`, on radius `r`,
 * spanning `half` degrees either side.
 */
export function segmentPath(angle: number, r = 36, half = 6.5): string {
  const pt = (a: number) => {
    const rad = (a * Math.PI) / 180;
    return `${(50 + r * Math.sin(rad)).toFixed(2)} ${(50 - r * Math.cos(rad)).toFixed(2)}`;
  };
  return `M${pt(angle - half)} A${r} ${r} 0 0 1 ${pt(angle + half)}`;
}

/** Frame values are LED duty; on a screen, draw each lit pixel at this opacity. */
export const displayLevel = (level: number) => Math.sqrt(level);

/** The moment each effect is most legible, for a still frame (reduced motion). */
export const STILL_MS: Partial<Record<Effect, number>> = {
  listening: 800,
  speaking: 1200,
  "timer-ringing": 250,
  boot: 300,
};
