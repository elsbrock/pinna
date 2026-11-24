// One drawing per supported model, keyed by `Status.model` (pinna-hal's
// board_model()). A model without a drawing gets the flat Ring instead.
import type { Effect } from "../ring.ts";
import * as echo2 from "./echo-2.ts";
import * as echoDot2 from "./echo-dot-2.ts";

export type Stage = typeof echoDot2.STAGE;

export type Drawing = {
  /** Model name, for accessible labels. */
  name: string;
  /** Width over height. */
  aspect: number;
  /** Glow-path brightness relative to the LEDs. */
  glow: number;
  /** Where it stands: the viewBox, the centre of its footprint in it, viewBox units per mm, footprint radius in mm, sin of the view angle. */
  stage: Stage;
  /** One `<svg>`; `id` is unique per page, `effect` is the still frame. */
  svg: (id: string, effect?: Effect) => string;
};

const DRAWINGS: Record<string, Drawing> = {
  "echo-dot-2": { name: "Echo Dot (2nd gen)", aspect: echoDot2.ASPECT, glow: echoDot2.GLOW, stage: echoDot2.STAGE, svg: echoDot2.svg },
  "echo-2": { name: "Echo (2nd gen)", aspect: echo2.ASPECT, glow: echo2.GLOW, stage: echo2.STAGE, svg: echo2.svg },
};

/** Width over height of the box every drawing fits: the Dot's. A taller model keeps its height and narrows. */
export const BOX_ASPECT = echoDot2.ASPECT;

export function drawingFor(model: string): Drawing | undefined {
  return DRAWINGS[model];
}

/** Every model Pinna runs on, drawn or not, by `Status.model` (pinna-board's `Model::name`). */
const NAMES: Record<string, string> = {
  "echo-dot-2": "Echo Dot (2nd gen)",
  "echo-2": "Echo (2nd gen)",
  "echo-dot-5": "Echo Dot (5th gen)",
};

/** The name owners know a model by, or `undefined` for an id Pinna doesn't know. */
export function modelName(model: string): string | undefined {
  return Object.hasOwn(NAMES, model) ? NAMES[model] : undefined;
}
