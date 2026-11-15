<!-- A titled group of settings rows on one raised surface (pn-group). -->
<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    title,
    aside,
    foot,
    children,
  }: { title?: string; aside?: Snippet; foot?: Snippet; children: Snippet } = $props();

  const id = `grp-${Math.random().toString(36).slice(2, 8)}`;
</script>

<section class="pn-group" aria-labelledby={title ? id : undefined}>
  {#if title}
    <div class="pn-group-head">
      <h2 {id} class="pn-group-title">{title}</h2>
      {#if aside}<span class="pn-group-aside">{@render aside()}</span>{/if}
    </div>
  {/if}
  <div class="pn-group-body">{@render children()}</div>
  {#if foot}<p class="pn-group-foot">{@render foot()}</p>{/if}
</section>
