<!-- The toasts (toast.svelte.ts), bottom centre. One live region for all. -->
<script lang="ts">
  import Icon from "./Icon.svelte";
  import { toasts } from "./toast.svelte.ts";
</script>

<div class="pn-toasts" role="status" aria-live="polite">
  {#each toasts.list as t (t.id)}
    <div class="pn-toast" data-tone={t.tone}>
      <Icon name={t.tone === "ok" ? "check" : t.tone === "danger" ? "alert" : "info"} />
      <span>{t.text}</span>
    </div>
  {/each}
</div>
