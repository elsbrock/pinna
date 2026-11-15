<!-- A link that leaves the dashboard (docs, source): opens in a new tab, says so. -->
<script lang="ts">
  import type { Snippet } from "svelte";

  import Icon from "./Icon.svelte";

  let { href, children }: { href: string; children: Snippet } = $props();
</script>

<a class="ext" {href} target="_blank" rel="noreferrer"
  >{@render children()}<Icon name="external" class="pn-icon ext-icon" /><span class="sr">(opens in a new tab)</span></a
>

<style>
  .ext {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--signal);
    border-radius: var(--radius-xs);
  }
  .ext:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }
  :global(.ext-icon) {
    width: 13px;
    height: 13px;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
