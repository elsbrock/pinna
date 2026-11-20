<!-- The mark (design/mark.svg) and the wordmark. A child (the live ring) takes
     the mark's place. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  // An image, not inline: the mark's colours are fixed, and two inline copies
  // on a page would share gradient ids.
  import markUrl from "../../../design/mark.svg?url";

  let { children }: { children?: Snippet } = $props();
</script>

<span class="pn-logo">
  <span class="mark">{#if children}{@render children()}{:else}<img src={markUrl} alt="" width="28" height="28" />{/if}</span>
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
</style>
