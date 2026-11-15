<!-- The design system's stroked icons (design/icons.ts): 24px grid, currentColor. -->
<script lang="ts">
  import { ICONS, type IconName } from "../../../design/icons.ts";

  let { name, class: className = "pn-icon" }: { name: IconName; class?: string } = $props();
</script>

<svg class={className} viewBox="0 0 24 24" aria-hidden="true">{@html ICONS[name]}</svg>
