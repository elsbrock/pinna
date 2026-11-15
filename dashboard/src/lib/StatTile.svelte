<!-- One fact at a glance that links to where it is managed. -->
<script lang="ts">
  import type { Snippet } from "svelte";

  import type { IconName } from "../../../design/icons.ts";
  import Icon from "./Icon.svelte";

  let {
    href,
    icon,
    label,
    value,
    mono = false,
    children,
  }: { href: string; icon: IconName; label: string; value: string; mono?: boolean; children?: Snippet } = $props();
</script>

<a class="pn-stat" {href}>
  <span class="pn-stat-label"><Icon name={icon} />{label}</span>
  <span class="pn-stat-value" class:is-mono={mono}>{value}</span>
  {#if children}<span class="pn-stat-sub">{@render children()}</span>{/if}
</a>
