<!-- The design system's stroked icons (24px grid, currentColor). -->
<script lang="ts">
  let { name, class: className = "pn-icon" }: { name: "alert" | "info" | "external"; class?: string } = $props();
</script>

<svg class={className} viewBox="0 0 24 24" aria-hidden="true">
  {#if name === "alert"}
    <path d="M12 3.5 2.8 19.5h18.4Z" /><path d="M12 10v4" /><path d="M12 17h.01" />
  {:else if name === "external"}
    <path d="M15 3h6v6" /><path d="M10 14 21 3" /><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
  {:else}
    <circle cx="12" cy="12" r="9" /><path d="M12 11v5" /><path d="M12 8h.01" />
  {/if}
</svg>
