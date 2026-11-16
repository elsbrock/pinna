// The Echo (2nd gen) in three-quarter view, as SVG markup: the Dot's top face
// on a tall cylinder whose wall is a perforated mesh between a short plastic
// collar and the foot. The top face is drawn top-down in a 100x100 box and
// squashed into an ellipse, so the ring, buttons and mic hole foreshorten
// together. The ring is one band of frosted white plastic; the 12 LEDs only
// show as diffused light inside it. Light paths carry data-led / data-glow in
// pixel order, for whoever animates them. A drawing of the device, not a
// photo: no Amazon marks. Styled by .pn-drawing in design/pinna.css.
import {
  RING_PIXELS,
  STILL_MS,
  displayLevel,
  pixelAngle,
  ringFrame,
  segmentPath,
  type Effect,
} from "../ring.ts";

// Geometry in viewBox units. The Echo is 88 mm across and 148 mm tall, seen
// from 30 degrees above.
const CX = 120;
const TOP_Y = 64;
const SX = 2;
const TILT = 0.5; // sin(30 deg): how round the top face looks
const SY = SX * TILT;
const R = 49 * SX;
const RY = 49 * SY;
const MM = (2 * R) / 88; // viewBox units per millimetre
const COS = Math.cos(Math.asin(TILT)); // how much of a vertical length shows
const H = Math.round(148 * MM * COS);
const BOTTOM_Y = TOP_Y + H;
const COLLAR = Math.round(9 * MM * COS); // plastic between the top face and the mesh
const BASE = 6; // rubber foot band
const VIEW = { x: 0, y: 8, w: 240, h: BOTTOM_Y + RY + 12 - 8 };

// The visible front half of a band of the wall, from y1 down to y2.
const band = (y1: number, y2: number) =>
  `M${CX - R} ${y1} A${R} ${RY} 0 0 0 ${CX + R} ${y1} V${y2} A${R} ${RY} 0 0 1 ${CX - R} ${y2} Z`;
const body = `M${CX - R} ${TOP_Y} V${BOTTOM_Y} A${R} ${RY} 0 0 0 ${CX + R} ${BOTTOM_Y} V${TOP_Y} Z`;
const MESH_TOP = TOP_Y + COLLAR;
const MESH_BOTTOM = BOTTOM_Y - BASE;
const fabric = band(MESH_TOP, MESH_BOTTOM);
const foot = band(BOTTOM_Y - BASE, BOTTOM_Y);
const seam = `M${CX - R} ${MESH_TOP} A${R} ${RY} 0 0 0 ${CX + R} ${MESH_TOP}`;
const topTransform = `translate(${CX - 50 * SX} ${TOP_Y - 50 * SY}) scale(${SX} ${SY})`;

// The mesh: holes in staggered columns at equal angles round the wall. Each
// column is one dotted line (zero-length dashes, round caps) squeezed by how
// far it turns away, and dropped by the wall's curve so rows follow the rim.
const HOLE = { pitch: 4.4, row: 4, size: 2.3 };
const COLUMNS = Math.round((Math.PI * R) / HOLE.pitch);
const holes = Array.from({ length: COLUMNS }, (_, j) => {
  const a = -Math.PI / 2 + ((j + 0.5) * Math.PI) / COLUMNS;
  const x = CX + R * Math.sin(a);
  const c = Math.cos(a);
  const drop = RY * c;
  const y1 = MESH_TOP + drop + HOLE.row * (j % 2 ? 1 : 0.5);
  const y2 = MESH_BOTTOM + drop - HOLE.row * 0.5;
  return (
    `<path class="mesh" d="M0 ${y1.toFixed(2)}V${y2.toFixed(2)}" ` +
    `transform="translate(${x.toFixed(2)} 0) scale(${c.toFixed(3)} 1)" stroke-opacity="${(0.35 + 0.65 * c).toFixed(2)}"/>`
  );
}).join("");

// The ring band at the top rim (local units). Each LED lights a stretch of it
// wide enough to overlap its neighbours; blur and a clip to the band diffuse it.
const BAND = { r: 45.5, inner: 43, outer: 48 };
const segments = Array.from({ length: RING_PIXELS }, (_, i) => segmentPath(pixelAngle(i), BAND.r, 19));
const annulus = (o: number, n: number) =>
  `M${50 - o} 50 A${o} ${o} 0 1 1 ${50 + o} 50 A${o} ${o} 0 1 1 ${50 - o} 50 Z ` +
  `M${50 - n} 50 A${n} ${n} 0 1 1 ${50 + n} 50 A${n} ${n} 0 1 1 ${50 - n} 50 Z`;

// Top face, in the local 100x100 box (front = bottom): one microphone hole in
// the centre and the four buttons in a diamond.
const polar = (r: number, deg: number) => {
  const rad = (deg * Math.PI) / 180;
  return { x: 50 + r * Math.sin(rad), y: 50 - r * Math.cos(rad) };
};
// Clockwise from the back: action, volume down, microphone off, volume up.
const buttons = [
  { at: polar(20, 0), glyph: "action" },
  { at: polar(20, 90), glyph: "minus" },
  { at: polar(20, 180), glyph: "mic-off" },
  { at: polar(20, 270), glyph: "plus" },
] as const;

const GLYPHS: Record<(typeof buttons)[number]["glyph"], string> = {
  minus: `<path class="glyph" d="M-2.6 0H2.6"/>`,
  plus: `<path class="glyph" d="M-2.6 0H2.6M0 -2.6V2.6"/>`,
  action: `<circle class="glyph-fill" r="1.5"/>`,
  "mic-off": `<path class="glyph" d="M-1.1 -1.6V-2a1.1 1.1 0 0 1 2.2 0V0.4M-2.4 -0.2a2.4 2.4 0 0 0 4.1 1.7M0 2.2V3.2M-3 -3L3 3"/>`,
};

/** Width over height, for layout before the SVG loads. */
export const ASPECT = VIEW.w / VIEW.h;

/** How bright the glow paths are relative to the LEDs. */
export const GLOW = 0.6;

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
  const led = (cls: string, k: number, attr: string) =>
    segments
      .map(
        (d, i) =>
          `<path class="${cls}" d="${d}" ${attr} stroke="${lit[i].stroke}" stroke-opacity="${(lit[i].opacity * k).toFixed(3)}"/>`,
      )
      .join("");
  return (
    `<svg viewBox="${VIEW.x} ${VIEW.y} ${VIEW.w} ${VIEW.h}" aria-hidden="true" focusable="false">` +
    `<defs>` +
    `<linearGradient id="${id}-side" gradientUnits="userSpaceOnUse" x1="${CX - R}" x2="${CX + R}" y1="0" y2="0">` +
    `<stop offset="0" class="side-edge"/><stop offset="0.2" class="side-light"/><stop offset="0.5" class="side-mid"/>` +
    `<stop offset="0.82" class="side-dark"/><stop offset="1" class="side-edge"/></linearGradient>` +
    `<radialGradient id="${id}-sheen" cx="0.36" cy="0.3" r="0.75"><stop offset="0" class="sheen-hi"/><stop offset="1" class="sheen-lo"/></radialGradient>` +
    `<filter id="${id}-shadow" x="-20%" y="-50%" width="140%" height="200%"><feGaussianBlur stdDeviation="6"/></filter>` +
    `<filter id="${id}-glow" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="4"/></filter>` +
    `<filter id="${id}-diffuse" x="-10%" y="-10%" width="120%" height="120%"><feGaussianBlur stdDeviation="1.6"/></filter>` +
    `<clipPath id="${id}-band"><path d="${annulus(BAND.outer, BAND.inner)}" clip-rule="evenodd"/></clipPath>` +
    `<clipPath id="${id}-wall"><path d="${body}"/></clipPath>` +
    `</defs>` +
    `<ellipse class="shadow" cx="${CX}" cy="${BOTTOM_Y + 6}" rx="${R + 6}" ry="${RY + 2}" filter="url(#${id}-shadow)"/>` +
    `<path class="body" d="${body}" fill="url(#${id}-side)"/>` +
    `<path class="fabric" d="${fabric}"/>` +
    `<g stroke-width="${HOLE.size}" stroke-dasharray="0 ${HOLE.row}">${holes}</g>` +
    `<path class="seam" d="${seam}" clip-path="url(#${id}-wall)"/>` +
    `<path class="foot" d="${foot}"/>` +
    `<g transform="${topTransform}">` +
    `<circle class="top" cx="50" cy="50" r="49"/>` +
    `<circle cx="50" cy="50" r="49" fill="url(#${id}-sheen)"/>` +
    `<circle class="rim" cx="50" cy="50" r="48.6" vector-effect="non-scaling-stroke"/>` +
    `<circle class="hole" cx="50" cy="50" r="0.9"/>` +
    buttons
      .map(
        ({ at, glyph }) =>
          `<g transform="translate(${at.x} ${at.y})"><circle class="button" r="7" vector-effect="non-scaling-stroke"/>${GLYPHS[glyph]}</g>`,
      )
      .join("") +
    `<circle class="band" cx="50" cy="50" r="${BAND.r}"/>` +
    `<g filter="url(#${id}-glow)">${led("led-glow", GLOW, "data-glow")}</g>` +
    `<g clip-path="url(#${id}-band)"><g filter="url(#${id}-diffuse)">${led("led", 1, "data-led")}</g></g>` +
    `<circle class="band-edge" cx="50" cy="50" r="${BAND.outer}" vector-effect="non-scaling-stroke"/>` +
    `</g></svg>`
  );
}
