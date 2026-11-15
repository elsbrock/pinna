<!-- The satellite's LED ring, drawn the way the device draws it (design/ring.ts,
     a port of pinna-hal's anim.rs). One still frame under reduced motion. -->
<script lang="ts">
  import {
    RING_PIXELS,
    STILL_MS,
    displayLevel,
    pixelAngle,
    ringFrame,
    segmentPath,
    type Effect,
  } from "../../../design/ring.ts";

  let {
    effect: shown,
    value,
    size,
    label,
    still = false,
  }: {
    effect: Effect;
    value?: number;
    size: number;
    label: string;
    /** One still frame, as under reduced motion: for a ring that isn't a satellite's. */
    still?: boolean;
  } = $props();

  const segments = Array.from({ length: RING_PIXELS }, (_, i) => segmentPath(pixelAngle(i)));
  const glow = $derived(size >= 40);
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");

  // Effects that do not move are drawn once.
  const STATIC: ReadonlySet<Effect> = new Set(["idle", "muted", "timer"]);

  let frame = $state(ringFrame("idle", 0));

  $effect(() => {
    const e = shown;
    const v = value;
    const fixed = still || reduceMotion.matches;
    const started = performance.now();
    let raf = 0;
    const draw = () => {
      frame = ringFrame(e, fixed ? (STILL_MS[e] ?? 0) : performance.now() - started, v);
      if (!fixed && !STATIC.has(e)) raf = requestAnimationFrame(draw);
    };
    draw();
    return () => cancelAnimationFrame(raf);
  });

  const stroke = (i: number) => {
    const c = frame[i].c;
    return c ? `rgb(${c.join(",")})` : "none";
  };
  const opacity = (i: number) => (frame[i].c ? displayLevel(frame[i].level) : 0);
</script>

<span class="pn-ring" style:width="{size}px" style:height="{size}px" role="img" aria-label={label}>
  <svg viewBox="0 0 100 100" aria-hidden="true">
    <circle class="pn-ring-puck" cx="50" cy="50" r="49" />
    {#each segments as d (d)}
      <path class="pn-ring-off" {d} />
    {/each}
    {#if glow}
      {#each segments as d, i (d)}
        <path class="pn-ring-glow" {d} stroke={stroke(i)} stroke-opacity={opacity(i) * 0.4} />
      {/each}
    {/if}
    {#each segments as d, i (d)}
      <path class="pn-ring-led" {d} stroke={stroke(i)} stroke-opacity={opacity(i)} />
    {/each}
  </svg>
</span>
