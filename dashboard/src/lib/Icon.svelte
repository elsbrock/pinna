<!-- The design system's stroked icons (24px grid, currentColor). -->
<script lang="ts">
  let { name, class: className = "pn-icon" }: { name: "alert" | "info"; class?: string } = $props();
</script>

<svg class={className} viewBox="0 0 24 24" aria-hidden="true">
  {#if name === "alert"}
    <path d="M12 3.5 2.8 19.5h18.4Z" /><path d="M12 10v4" /><path d="M12 17h.01" />
  {:else}
    <circle cx="12" cy="12" r="9" /><path d="M12 11v5" /><path d="M12 8h.01" />
  {/if}
</svg>
