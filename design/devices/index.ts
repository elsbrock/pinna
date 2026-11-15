// One drawing per supported model, keyed by `Status.model` (pinna-hal's
// board_model()). A model without a drawing gets the flat Ring instead.
import type { Effect } from "../ring.ts";
import * as echoDot2 from "./echo-dot-2.ts";

export type Drawing = {
  /** Model name, for accessible labels. */
  name: string;
  /** Width over height. */
  aspect: number;
  /** Glow-path brightness relative to the LEDs. */
  glow: number;
  /** One `<svg>`; `id` is unique per page, `effect` is the still frame. */
  svg: (id: string, effect?: Effect) => string;
};

const DRAWINGS: Record<string, Drawing> = {
  "echo-dot-2": { name: "Echo Dot (2nd gen)", aspect: echoDot2.ASPECT, glow: echoDot2.GLOW, svg: echoDot2.svg },
};

export function drawingFor(model: string): Drawing | undefined {
  return DRAWINGS[model];
}
