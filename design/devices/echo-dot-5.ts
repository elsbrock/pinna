// The Echo Dot (5th gen) in three-quarter view, as SVG markup: a sphere of
// fabric with its buttons in the fabric at the top, cut flat at the bottom,
// where it stands. The ring is a band of frosted white plastic round the sphere's
// lowest stretch, just above the bottom cut, as the other Echos have one
// round their top: the 12 LEDs only show as diffused light inside it, and a
// little of it spills onto the surface the satellite stands on. The LED
// stretches are drawn in a 100x100 box squashed into an ellipse, as the
// other drawings do their top faces. Light paths
// carry data-led / data-glow in pixel order, for whoever animates them. A
// drawing of the device, not a photo: no Amazon marks. Styled by .pn-drawing
// in design/pinna.css.
import {
  RING_PIXELS,
  STILL_MS,
  displayLevel,
  pixelAngle,
  ringFrame,
  segmentPath,
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
// A plane `d` below the centre cuts the sphere in a circle, seen as an
// ellipse whose ends lie on the outline.
const slice = (d: number) => {
  const r = Math.sqrt(RS * RS - d * d);
  return { y: CY + d, r, ry: r * TILT };
};
const CUT = 38 * MM; // the bottom cut: 50 + 38 = 88 mm tall
const BOTTOM = slice(CUT);
const BAND = { h: 6 * MM }; // the ring's height on the sphere
const BAND_TOP = slice(CUT - BAND.h);
const VIEW = { x: 0, y: 8, w: 240, h: BOTTOM.y + BOTTOM.ry + 22 - 8 };

const f = (n: number) => n.toFixed(2);
const sphere =
  `M${f(CX - BOTTOM.r)} ${f(BOTTOM.y)} A${RS} ${RS} 0 1 1 ${f(CX + BOTTOM.r)} ${f(BOTTOM.y)} ` +
  `A${f(BOTTOM.r)} ${f(BOTTOM.ry)} 0 0 1 ${f(CX - BOTTOM.r)} ${f(BOTTOM.y)} Z`;
// The band: the front of the sphere between the band's top and the bottom cut,
// closed along the outline at both sides.
const band =
  `M${f(CX - BAND_TOP.r)} ${f(BAND_TOP.y)} A${f(BAND_TOP.r)} ${f(BAND_TOP.ry)} 0 0 0 ${f(CX + BAND_TOP.r)} ${f(BAND_TOP.y)} ` +
  `A${RS} ${RS} 0 0 1 ${f(CX + BOTTOM.r)} ${f(BOTTOM.y)} ` +
  `A${f(BOTTOM.r)} ${f(BOTTOM.ry)} 0 0 1 ${f(CX - BOTTOM.r)} ${f(BOTTOM.y)} ` +
  `A${RS} ${RS} 0 0 1 ${f(CX - BAND_TOP.r)} ${f(BAND_TOP.y)} Z`;

// Fabric: a fine plain weave over all of the sphere you see, on the diagonal.
// In a tile `KNIT` units wide, each quarter holds one thread passing over,
// across in two and along in the other two, over dark gaps; a thin highlight
// rounds each. Towards the outline the weave fades into the shading, as
// fabric that turns away does.
const KNIT = 3;
const Q = KNIT / 4;
const threads =
  `M${Q * 0.4} ${Q}H${Q * 1.6}M${Q * 3} ${Q * 0.4}V${Q * 1.6}` +
  `M${Q} ${Q * 2.4}V${Q * 3.6}M${Q * 2.4} ${Q * 3}H${Q * 3.6}`;

// The buttons are in the fabric round the top: each a faint raised patch with
// its symbol, laid on the sphere `BUTTON.from` degrees from the middle of the
// group. Each is drawn in a local box (y towards the front) mapped onto the
// sphere's tangent plane at its centre, so the ones further round lean away.
// The group sits `BUTTON.forward` degrees in front of the apex: from 20
// degrees above, a button behind the apex would be on the outline.
const BUTTON = { from: 12, forward: 8, r: 9, scale: 0.78 };
type Vec = [number, number, number];
const dot = (a: Vec, b: Vec) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const along = (v: Vec, n: Vec): Vec => {
  const d = dot(v, n);
  const t: Vec = [v[0] - d * n[0], v[1] - d * n[1], v[2] - d * n[2]];
  const l = Math.sqrt(dot(t, t));
  return [t[0] / l, t[1] / l, t[2] / l];
};
// x right, y up, z towards the viewer, onto the viewBox.
const screen = (v: Vec) => [v[0], -v[1] * COS + v[2] * TILT];
const onSphere = (deg: number) => {
  const lat = ((90 - BUTTON.from) * Math.PI) / 180;
  const lon = (deg * Math.PI) / 180;
  const tip = (BUTTON.forward * Math.PI) / 180;
  const [x, y, z] = [Math.cos(lat) * Math.sin(lon), Math.sin(lat), Math.cos(lat) * Math.cos(lon)];
  const n: Vec = [x, y * Math.cos(tip) - z * Math.sin(tip), y * Math.sin(tip) + z * Math.cos(tip)];
  const [a, b] = screen(along([1, 0, 0], n)).map((c) => c * BUTTON.scale);
  const [c, d] = screen(along([0, 0, 1], n)).map((c) => c * BUTTON.scale);
  const [e, g] = screen(n).map((c) => c * RS);
  return `matrix(${[a, b, c, d, CX + e, CY + g].map(f).join(" ")})`;
};
// Round from the front: microphone off, volume down, action, volume up.
const buttons = [
  { at: onSphere(0), glyph: "mic-off" },
  { at: onSphere(90), glyph: "minus" },
  { at: onSphere(180), glyph: "action" },
  { at: onSphere(270), glyph: "plus" },
] as const;

const GLYPHS: Record<(typeof buttons)[number]["glyph"], string> = {
  minus: `<path class="glyph" d="M-3.4 0H3.4"/>`,
  plus: `<path class="glyph" d="M-3.4 0H3.4M0 -3.4V3.4"/>`,
  action: `<circle class="glyph-fill" r="2"/>`,
  "mic-off": `<path class="glyph" d="M-1.4 -2.1V-2.6a1.4 1.4 0 0 1 2.8 0V0.5M-3.1 -0.3a3.1 3.1 0 0 0 5.3 2M0 2.8V4.1M-3.9 -3.9L3.9 3.9"/>`,
};

// The LEDs, in a local 100x100 box whose circle (r 50) is the slice through
// the middle of the band. Each LED lights a stretch wide enough to overlap its
// neighbours and taller than the band; blur and a clip to the band diffuse
// it. The glow is the same stretches on the floor just outside the bottom
// cut, where the light lands.
const MID = slice(CUT - BAND.h / 2);
const ellipseAt = (y: number, r: number) => {
  const s = r / 50;
  return `translate(${f(CX - 50 * s)} ${f(y - 50 * s * TILT)}) scale(${f(s)} ${f(s * TILT)})`;
};
const bandTransform = ellipseAt(MID.y, MID.r);
const floorTransform = ellipseAt(BOTTOM.y, BOTTOM.r + 12);
// 1.4 times the band's height at the front, in local units.
const LED_W = (1.4 * (BOTTOM.y + BOTTOM.ry - BAND_TOP.y - BAND_TOP.ry)) / ((MID.r / 50) * TILT);
const segments = Array.from({ length: RING_PIXELS }, (_, i) => segmentPath(pixelAngle(i), 50, 19));

/** Width over height, for layout before the SVG loads. */
export const ASPECT = VIEW.w / VIEW.h;

/** Where the device stands, for placing drawings on one floor. */
export const STAGE = { view: VIEW, foot: { x: CX, y: BOTTOM.y }, mm: MM, radius: 50, tilt: TILT };

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
    segments
      .map(
        (d, i) =>
          `<path class="${cls}" d="${d}" ${attr} style="stroke-width:${width.toFixed(1)}" stroke="${lit[i].stroke}" stroke-opacity="${(lit[i].opacity * k).toFixed(3)}"/>`,
      )
      .join("");
  return (
    `<svg viewBox="${VIEW.x} ${VIEW.y} ${VIEW.w} ${VIEW.h}" aria-hidden="true" focusable="false">` +
    `<defs>` +
    `<radialGradient id="${id}-ball" gradientUnits="userSpaceOnUse" cx="${CX - 0.3 * RS}" cy="${CY - 0.35 * RS}" r="${1.5 * RS}">` +
    `<stop offset="0" class="side-light"/><stop offset="0.45" class="side-mid"/><stop offset="1" class="side-edge"/></radialGradient>` +
    `<linearGradient id="${id}-band-shade" gradientUnits="userSpaceOnUse" x1="${f(CX - BAND_TOP.r)}" x2="${f(CX + BAND_TOP.r)}" y1="0" y2="0">` +
    `<stop offset="0" class="band-shade-edge"/><stop offset="0.3" class="band-shade-mid"/><stop offset="0.65" class="band-shade-mid"/>` +
    `<stop offset="1" class="band-shade-edge"/></linearGradient>` +
    `<radialGradient id="${id}-sheen" cx="0.36" cy="0.3" r="0.75"><stop offset="0" class="sheen-hi"/><stop offset="1" class="sheen-lo"/></radialGradient>` +
    `<filter id="${id}-shadow" x="-20%" y="-50%" width="140%" height="200%"><feGaussianBlur stdDeviation="6"/></filter>` +
    `<filter id="${id}-glow" x="-30%" y="-80%" width="160%" height="260%"><feGaussianBlur stdDeviation="4"/></filter>` +
    `<filter id="${id}-diffuse" x="-10%" y="-40%" width="120%" height="180%"><feGaussianBlur stdDeviation="1.6"/></filter>` +
    `<pattern id="${id}-knit" patternUnits="userSpaceOnUse" width="${KNIT}" height="${KNIT}" patternTransform="rotate(45 ${CX} ${CY})">` +
    `<rect class="knit-gap" width="${KNIT}" height="${KNIT}"/>` +
    `<path class="knit" d="${threads}"/><path class="knit-hi" d="${threads}"/></pattern>` +
    `<radialGradient id="${id}-knit-fade" gradientUnits="userSpaceOnUse" cx="${CX}" cy="${CY}" r="${RS}">` +
    `<stop offset="0.55" stop-color="#fff"/><stop offset="1" stop-color="#333"/></radialGradient>` +
    `<mask id="${id}-knit-mask"><path d="${sphere}" fill="url(#${id}-knit-fade)"/></mask>` +
    `<clipPath id="${id}-ball-clip"><path d="${sphere}"/></clipPath>` +
    `<clipPath id="${id}-band"><path d="${band}"/></clipPath>` +
    `</defs>` +
    `<ellipse class="shadow" cx="${CX}" cy="${f(BOTTOM.y + 4)}" rx="${f(BOTTOM.r + 10)}" ry="${f(BOTTOM.ry + 4)}" filter="url(#${id}-shadow)"/>` +
    `<g filter="url(#${id}-glow)"><g transform="${floorTransform}">${led("led-glow", GLOW, "data-glow", 14)}</g></g>` +
    `<path class="body" d="${sphere}" fill="url(#${id}-ball)"/>` +
    `<path d="${sphere}" fill="url(#${id}-knit)" mask="url(#${id}-knit-mask)"/>` +
    `<path d="${sphere}" fill="url(#${id}-sheen)"/>` +
    `<path class="band-side" d="${band}"/>` +
    `<g clip-path="url(#${id}-band)"><g filter="url(#${id}-diffuse)"><g transform="${bandTransform}">${led("led", 1, "data-led", LED_W)}</g></g></g>` +
    `<path d="${band}" fill="url(#${id}-band-shade)"/>` +
    buttons
      .map(({ at, glyph }) => `<g transform="${at}"><circle class="press" r="${BUTTON.r}"/>${GLYPHS[glyph]}</g>`)
      .join("") +
    `</svg>`
  );
}
