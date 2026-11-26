// The Echo Dot (5th gen) in three-quarter view, as SVG markup: a sphere of
// fabric on a narrow base, the top cut flat for the buttons. The ring is the
// 12 LEDs under the sphere's lower edge, shining onto the surface the satellite
// stands on. Each one is drawn as a dot with its own small glow, never as a
// continuous band: a lit band round the base would read as someone else's
// ring. The state's colour as a whole is the halo's job (Device.svelte). The
// dots sit in a 100x100 box squashed into an ellipse, as the other drawings
// do their top faces. Light paths carry data-led / data-glow in pixel
// order, for whoever animates them. A drawing of the device, not a photo: no
// Amazon marks. Styled by .pn-drawing in design/pinna.css.
import {
  RING_PIXELS,
  STILL_MS,
  displayLevel,
  pixelAngle,
  ringFrame,
  type Effect,
} from "../ring.ts";

// Geometry in viewBox units. The Dot is 100 mm across and 88 mm tall, seen
// from 20 degrees above.
const CX = 120;
const TILT = Math.sin((20 * Math.PI) / 180); // how round the floor and top look
const COS = Math.cos(Math.asin(TILT));
const RS = 98; // the sphere's radius
const MM = (2 * RS) / 100; // viewBox units per millimetre
const CY = 15 + RS; // the sphere's centre
const CUT = 56; // how far below the centre the sphere is cut for the base
const CUT_Y = CY + CUT;
const CUT_R = Math.sqrt(RS * RS - CUT * CUT); // the cut's radius
const CUT_RY = CUT_R * TILT;
const BASE = { r: 50, h: 16, drop: 8 }; // narrow base: radius, height, start below the cut
const BASE_TOP = CUT_Y + BASE.drop;
const BASE_BOTTOM = BASE_TOP + BASE.h;
const BASE_RY = BASE.r * TILT;
const VIEW = { x: 0, y: 8, w: 240, h: BASE_BOTTOM + 68 * TILT + 14 - 8 };

const sphere =
  `M${(CX - CUT_R).toFixed(2)} ${CUT_Y} A${RS} ${RS} 0 1 1 ${(CX + CUT_R).toFixed(2)} ${CUT_Y} ` +
  `A${CUT_R.toFixed(2)} ${CUT_RY.toFixed(2)} 0 0 1 ${(CX - CUT_R).toFixed(2)} ${CUT_Y} Z`;
const base = `M${CX - BASE.r} ${BASE_TOP} V${BASE_BOTTOM} A${BASE.r} ${BASE_RY.toFixed(2)} 0 0 0 ${CX + BASE.r} ${BASE_BOTTOM} V${BASE_TOP} Z`;
const foot =
  `M${CX - BASE.r} ${BASE_BOTTOM - 4} A${BASE.r} ${BASE_RY.toFixed(2)} 0 0 0 ${CX + BASE.r} ${BASE_BOTTOM - 4} ` +
  `V${BASE_BOTTOM} A${BASE.r} ${BASE_RY.toFixed(2)} 0 0 1 ${CX - BASE.r} ${BASE_BOTTOM} Z`;

// Fabric: the front halves of latitude circles as rows of dots, squeezed by
// how far the sphere turns away from the viewer.
const WEAVE = { row: 4.4, size: 2.2, step: 6 };
const LATS = Math.floor(80 / WEAVE.step);
const weave = Array.from({ length: 2 * LATS + 1 }, (_, k) => {
  const lat = ((k - LATS) * WEAVE.step * Math.PI) / 180;
  const r = RS * Math.cos(lat);
  const cy = CY - RS * Math.sin(lat) * COS;
  return `<path class="mesh" d="M${(CX - r).toFixed(2)} ${cy.toFixed(2)} A${r.toFixed(2)} ${(r * TILT).toFixed(2)} 0 0 0 ${(CX + r).toFixed(2)} ${cy.toFixed(2)}"/>`;
}).join("");

// The flat top, where the buttons are: an ellipse near the apex, drawn top-down
// in a local 100x100 box (front = bottom) and squashed like the floor.
const TOP = { r: 38, y: CY - RS * COS + 12 };
const TOP_S = TOP.r / 49;
const topTransform = `translate(${CX - 50 * TOP_S} ${TOP.y - 50 * TOP_S * TILT}) scale(${TOP_S} ${TOP_S * TILT})`;
const polar = (r: number, deg: number) => {
  const rad = (deg * Math.PI) / 180;
  return { x: 50 + r * Math.sin(rad), y: 50 - r * Math.cos(rad) };
};
// Clockwise from the back: action, volume down, microphone off, volume up.
const buttons = [
  { at: polar(27, 0), glyph: "action" },
  { at: polar(27, 90), glyph: "minus" },
  { at: polar(27, 180), glyph: "mic-off" },
  { at: polar(27, 270), glyph: "plus" },
] as const;

const GLYPHS: Record<(typeof buttons)[number]["glyph"], string> = {
  minus: `<path class="glyph" d="M-3.4 0H3.4"/>`,
  plus: `<path class="glyph" d="M-3.4 0H3.4M0 -3.4V3.4"/>`,
  action: `<circle class="glyph-fill" r="2"/>`,
  "mic-off": `<path class="glyph" d="M-1.4 -2.1V-2.6a1.4 1.4 0 0 1 2.8 0V0.5M-3.1 -0.3a3.1 3.1 0 0 0 5.3 2M0 2.8V4.1M-3.9 -3.9L3.9 3.9"/>`,
};

// The ring on the floor round the base (local units, scaled by FLOOR_S). Each
// LED is one dot: a zero-length path with a round cap, as wide as its stroke.
const FLOOR_S = 2;
const DOT_R = 33;
const floorTransform = `translate(${CX - 50 * FLOOR_S} ${BASE_BOTTOM - 50 * FLOOR_S * TILT}) scale(${FLOOR_S} ${FLOOR_S * TILT})`;
const dots = Array.from({ length: RING_PIXELS }, (_, i) => {
  const rad = (pixelAngle(i) * Math.PI) / 180;
  return `M${(50 + DOT_R * Math.sin(rad)).toFixed(2)} ${(50 - DOT_R * Math.cos(rad)).toFixed(2)}h0`;
});

/** Width over height, for layout before the SVG loads. */
export const ASPECT = VIEW.w / VIEW.h;

/** Where the device stands, for placing drawings on one floor. */
export const STAGE = { view: VIEW, foot: { x: CX, y: BASE_BOTTOM }, mm: MM, radius: 50, tilt: TILT };

/** How bright the glow paths are relative to the LEDs. */
export const GLOW = 0.45;

/**
 * The drawing as one `<svg>` element. `id` must be unique per page (it prefixes
 * gradient and filter ids); `effect` is the still frame drawn before (or
 * without) JavaScript.
 */
export function svg(id: string, effect: Effect = "idle"): string {
  const frame = ringFrame(effect, STILL_MS[effect] ?? 0);
  const lit = frame.map((p) => ({
    stroke: p.c ? `rgb(${p.c.join(",")})` : "none",
    opacity: p.c ? displayLevel(p.level) : 0,
  }));
  const led = (cls: string, k: number, attr: string, width: number) =>
    dots
      .map(
        (d, i) =>
          `<path class="${cls}" d="${d}" ${attr} style="stroke-linecap:round;stroke-width:${width}" stroke="${lit[i].stroke}" stroke-opacity="${(lit[i].opacity * k).toFixed(3)}"/>`,
      )
      .join("");
  return (
    `<svg viewBox="${VIEW.x} ${VIEW.y} ${VIEW.w} ${VIEW.h}" aria-hidden="true" focusable="false">` +
    `<defs>` +
    `<radialGradient id="${id}-ball" gradientUnits="userSpaceOnUse" cx="${CX - 0.3 * RS}" cy="${CY - 0.35 * RS}" r="${1.5 * RS}">` +
    `<stop offset="0" class="side-light"/><stop offset="0.45" class="side-mid"/><stop offset="1" class="side-edge"/></radialGradient>` +
    `<linearGradient id="${id}-side" gradientUnits="userSpaceOnUse" x1="${CX - BASE.r}" x2="${CX + BASE.r}" y1="0" y2="0">` +
    `<stop offset="0" class="side-edge"/><stop offset="0.25" class="side-light"/><stop offset="0.6" class="side-mid"/>` +
    `<stop offset="1" class="side-edge"/></linearGradient>` +
    `<radialGradient id="${id}-sheen" cx="0.36" cy="0.3" r="0.75"><stop offset="0" class="sheen-hi"/><stop offset="1" class="sheen-lo"/></radialGradient>` +
    `<filter id="${id}-shadow" x="-20%" y="-50%" width="140%" height="200%"><feGaussianBlur stdDeviation="6"/></filter>` +
    `<filter id="${id}-glow" x="-30%" y="-80%" width="160%" height="260%"><feGaussianBlur stdDeviation="3"/></filter>` +
    `<clipPath id="${id}-ball-clip"><path d="${sphere}"/></clipPath>` +
    `</defs>` +
    `<ellipse class="shadow" cx="${CX}" cy="${BASE_BOTTOM + 4}" rx="${(CUT_R + 6).toFixed(2)}" ry="${(CUT_RY + 4).toFixed(2)}" filter="url(#${id}-shadow)"/>` +
    `<g transform="${floorTransform}">` +
    `<g filter="url(#${id}-glow)">${led("led-glow", GLOW, "data-glow", 9)}</g>` +
    led("led", 1, "data-led", 5) +
    `</g>` +
    `<path class="body" d="${base}" fill="url(#${id}-side)"/>` +
    `<path class="foot" d="${foot}"/>` +
    `<path class="body" d="${sphere}" fill="url(#${id}-ball)"/>` +
    `<g clip-path="url(#${id}-ball-clip)" stroke-width="${WEAVE.size}" stroke-dasharray="0 ${WEAVE.row}">${weave}</g>` +
    `<path d="${sphere}" fill="url(#${id}-sheen)"/>` +
    `<g transform="${topTransform}">` +
    `<circle class="top" cx="50" cy="50" r="49"/>` +
    `<circle class="rim" cx="50" cy="50" r="48.6" vector-effect="non-scaling-stroke"/>` +
    `<circle class="hole" cx="50" cy="50" r="1.2"/>` +
    buttons
      .map(
        ({ at, glyph }) =>
          `<g transform="translate(${at.x} ${at.y})"><circle class="button" r="9" vector-effect="non-scaling-stroke"/>${GLYPHS[glyph]}</g>`,
      )
      .join("") +
    `</g></svg>`
  );
}
