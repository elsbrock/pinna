<!-- The mark (design/mark.svg: the ring's boot frame) and the wordmark. -->
<script lang="ts">
  import mark from "../../../design/mark.svg?raw";

  // Inline, the mark takes its colours from the tokens; its own <style> only serves the favicon.
  const svg = mark
    .replace(/<title>.*?<\/title>/, "")
    .replace(/<style>.*?<\/style>/, "")
    .replace('class="puck"', 'class="pn-mark-puck"')
    .replace('width="100" height="100"', 'width="100%" height="100%" aria-hidden="true" focusable="false"');
</script>

<span class="pn-logo">
  <span class="mark">{@html svg}</span>
  <span class="pn-logo-word">Pinna</span>
</span>

<style>
  .mark {
    display: inline-block;
    width: 28px;
    height: 28px;
    line-height: 0;
  }
  .pn-logo-word {
    font-size: 22px;
  }
  :global(.pn-mark-puck) {
    fill: var(--puck);
    stroke: var(--puck-rim);
  }
  /* The rim brightens in dark so the disc keeps its edge (as on the website). */
  @media (prefers-color-scheme: dark) {
    :global(.pn-mark-puck) {
      stroke: var(--line-strong);
    }
  }
</style>
