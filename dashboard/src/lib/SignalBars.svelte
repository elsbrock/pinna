<!-- Wi-Fi strength as four bars, at the thresholds signalWord() uses. Always
     next to the word: the bars are decoration. -->
<script lang="ts">
  let { dbm }: { dbm: number } = $props();
  const lit = $derived(dbm >= -55 ? 4 : dbm >= -67 ? 3 : dbm >= -75 ? 2 : 1);
</script>

<svg class="pn-bars" viewBox="0 0 16 14" aria-hidden="true">
  {#each [0, 1, 2, 3] as i (i)}
    <rect class:is-on={i < lit} x={i * 4.33} y={10 - i * 3} width="3" height={4 + i * 3} rx="1" />
  {/each}
</svg>
