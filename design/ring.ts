// The LED ring for screens: a port of crates/pinna-hal/src/anim.rs plus the
// geometry for drawing it. Framework-free, so the website and the dashboard
// share it. Keep ringFrame() in step with anim.rs (ring.test.ts mirrors its
// tests). One deliberate difference: on the device the speaking ring follows
// the loudness of the voice (anim.rs speaking()); screens can't hear it, so
// they show the breathing the device falls back to.

export const RING_PIXELS = 12;

export type Rgb = readonly [number, number, number];

/** The ring colours, as in anim.rs. */
export const LED = {
  listening: [72, 185, 255],
  thinking: [168, 115, 239],
  error: [239, 80, 80],
  muted: [255, 0, 0],
  speaking: [90, 220, 230],
  amber: [255, 160, 0],
  volume: [120, 200, 255],
  notification: [255, 215, 0],
  calibration: [120, 230, 170],
} as const satisfies Record<string, Rgb>;

export type Effect =
  | "idle"
  | "listening"
  | "listening-at"
  | "thinking"
  | "speaking"
  | "error"
  | "muted"
  | "volume"
  | "timer"
  | "timer-ringing"
  | "notification"
  | "notification-cleared"
  | "calibration"
  | "calibration-heard"
  | "boot"
  | "equalizer";

/** Music equalizer, bass to treble. */
export const EQUALIZER: readonly Rgb[] = [
  [255, 40, 110],
  [255, 90, 30],
  [255, 170, 0],
  [110, 230, 50],
  [40, 215, 215],
  [70, 120, 255],
];

/** Effects with a natural end, in ms; the rest run until replaced. */
export const EFFECT_DURATION_MS = {
  error: 250 * 2 * 3,
  volume: 1500,
  boot: 1200,
  "notification-cleared": 400,
  "calibration-heard": 600,
} as const;

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
 * Listening with a brighter segment at `azimuth` (degrees clockwise from the
 * front): the ring at 40% of the colour, the segment's centre 85% towards
 * white, falling off linearly over 50 degrees. Integer maths as in anim.rs.
 */
function direction(c: Rgb, azimuth: number): Frame {
  const base = (v: number) => Math.floor((v * 40) / 100);
  const pointer = (v: number) => v + Math.floor(((255 - v) * 85) / 100);
  return Array.from({ length: RING_PIXELS }, (_, i) => {
    const d = Math.abs(((45 + 30 * i) % 360) - azimuth);
    const distance = d > 180 ? 360 - d : d;
    const level = Math.floor((Math.max(0, 50 - distance) * 100) / 50);
    const mix = (v: number) => base(v) + Math.floor(((pointer(v) - base(v)) * level) / 100);
    return { c: [mix(c[0]), mix(c[1]), mix(c[2])] as const, level: 1 };
  });
}

/**
 * The bands in pairs (the louder of each) as six, mirrored either side of the
 * front centre (pixels 10 and 11): bass at the front, treble at the back.
 */
function equalizer(bands: readonly number[]): Frame {
  const f = dark();
  EQUALIZER.forEach((c, k) => {
    const level = clamp(Math.max(bands[2 * k] ?? 0, bands[2 * k + 1] ?? 0), 0, 1);
    const p = { c: level > 0 ? c : null, level };
    f[(11 + k) % RING_PIXELS] = p;
    f[(10 - k + RING_PIXELS) % RING_PIXELS] = { ...p };
  });
  return f;
}

/**
 * The frame the satellite shows `tMs` after `effect` started.
 * `value`: volume 0..100 for "volume", fraction of the timer left 0..1 for
 * "timer", the 12 band levels 0..1 (bass first) for "equalizer", the
 * talker's azimuth in degrees (0 = front, clockwise) for "listening-at".
 */
export function ringFrame(effect: Effect, tMs: number, value?: number | readonly number[]): Frame {
  const t = tMs / 1000;
  const n = typeof value === "number" ? value : undefined;
  switch (effect) {
    case "listening":
      return solid(LED.listening, 0.3 + 0.7 * wave(t, 1.6));
    case "listening-at":
      return direction(LED.listening, (((n ?? 0) % 360) + 360) % 360);
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
      return arc(LED.volume, Math.min(n ?? 0, 100) / 100);
    case "timer":
      return arc(LED.amber, clamp(n ?? 1, 0, 1));
    case "timer-ringing":
      return solid(LED.amber, 0.15 + 0.85 * wave(t, 0.5));
    case "notification":
      return solid(LED.notification, 0.1 + 0.9 * wave(t, 3));
    case "notification-cleared":
      if (tMs >= EFFECT_DURATION_MS["notification-cleared"]) return dark();
      return solid(LED.notification, 1 - tMs / EFFECT_DURATION_MS["notification-cleared"]);
    case "calibration":
      return arc(LED.calibration, clamp(n ?? 0, 0, 100) / 100);
    case "calibration-heard":
      if (tMs >= EFFECT_DURATION_MS["calibration-heard"]) return dark();
      return solid(LED.calibration, 1 - tMs / EFFECT_DURATION_MS["calibration-heard"]);
    case "boot":
      if (tMs >= EFFECT_DURATION_MS.boot) return dark();
      return spin(LED.listening, t, 0.6, 6);
    case "equalizer":
      return equalizer(Array.isArray(value) ? value : []);
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
  notification: 1500,
  boot: 300,
};
