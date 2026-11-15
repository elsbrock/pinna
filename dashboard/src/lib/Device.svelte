<!-- The satellite as a drawing of its model (design/devices/), with the ring
     lit the way the device lights it. A model without a drawing gets the flat
     Ring. One still frame under reduced motion. -->
<script lang="ts">
  import { drawingFor } from "../../../design/devices/index.ts";
  import { STILL_MS, displayLevel, ringFrame, type Effect } from "../../../design/ring.ts";
  import Ring from "./Ring.svelte";

  let {
    model,
    effect: shown,
    width,
    label,
    halo = false,
  }: {
    model: string;
    effect: Effect;
    /** Rendered width in px; the flat Ring fallback is a little smaller. */
    width: number;
    label: string;
    /** Light the surface behind with the ring's colour. */
    halo?: boolean;
  } = $props();

  // `npm run dev` against a PC's pinnad (model ""): draw the Dot, which is
  // what the dashboard is designed around.
  const drawing = $derived(drawingFor(model || (import.meta.env.DEV ? "echo-dot-2" : "")));
  const id = `dev${Math.random().toString(36).slice(2, 8)}`;
  // Built once per model; the effect below lights it.
  const svg = $derived(drawing?.svg(id) ?? "");
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
  const STATIC: ReadonlySet<Effect> = new Set(["idle", "muted"]);

  let root = $state<HTMLElement>();

  $effect(() => {
    const el = root;
    const e = shown;
    const glow = drawing?.glow ?? 0.6;
    void svg;
    if (!el) return;
    const leds = [...el.querySelectorAll<SVGPathElement>("[data-led]")];
    const glows = [...el.querySelectorAll<SVGPathElement>("[data-glow]")];
    const started = performance.now();
    let raf = 0;
    const draw = () => {
      const frame = ringFrame(e, reduceMotion.matches ? (STILL_MS[e] ?? 0) : performance.now() - started);
      let sum = 0;
      let rgb: readonly number[] | null = null;
      frame.forEach((p, i) => {
        const stroke = p.c ? `rgb(${p.c.join(",")})` : "none";
        const o = p.c ? displayLevel(p.level) : 0;
        leds[i]?.setAttribute("stroke", stroke);
        leds[i]?.setAttribute("stroke-opacity", o.toFixed(3));
        glows[i]?.setAttribute("stroke", stroke);
        glows[i]?.setAttribute("stroke-opacity", (o * glow).toFixed(3));
        sum += o;
        rgb ??= p.c;
      });
      if (halo) {
        el.style.setProperty("--ring-rgb", (rgb ?? [72, 185, 255]).join(", "));
        el.style.setProperty("--ring-a", (sum / frame.length).toFixed(3));
      }
      if (!reduceMotion.matches && !STATIC.has(e)) raf = requestAnimationFrame(draw);
    };
    draw();
    return () => cancelAnimationFrame(raf);
  });
</script>

{#if drawing}
  <span
    bind:this={root}
    class="pn-drawing"
    class:has-halo={halo}
    style:width="{width}px"
    role="img"
    aria-label="{drawing.name}. {label}"
  >
    {@html svg}
  </span>
{:else}
  <Ring effect={shown} size={Math.round(width * 0.7)} {label} />
{/if}
