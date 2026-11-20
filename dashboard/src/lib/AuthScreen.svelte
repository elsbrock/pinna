<!-- A page of its own for what comes before the dashboard: the sign-in form and
     the offer of a password (ADR 0029). Nothing else loads behind it. -->
<script lang="ts">
  import type { Snippet } from "svelte";

  import { device, savedName } from "./device.svelte.ts";
  import Logo from "./Logo.svelte";
  import { displayName } from "./view.ts";

  let { title, children }: { title: string; children: Snippet } = $props();

  // Which satellite this is: the address of the page always, and the name when this
  // browser knows one (the one it last saw, which a rename elsewhere can leave behind).
  const name = $derived(device.status ? displayName(device.status) : savedName());

  $effect(() => {
    document.title = `${title} · Pinna`;
  });
</script>

<main class="auth">
  <Logo />
  <section class="card">
    <h1 class="pn-type-display-m">{title}</h1>
    <p class="where">{name ? `${name} · ` : ""}{location.host}</p>
    {@render children()}
  </section>
</main>

<style>
  .auth {
    box-sizing: border-box;
    min-height: 100dvh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-6);
    padding: var(--space-6) var(--space-4);
  }
  .card {
    box-sizing: border-box;
    width: min(100%, 440px);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    padding: var(--space-6);
    background: var(--surface-raised);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
  }
  h1 {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .where {
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 18px;
    color: var(--ink-muted);
    overflow-wrap: anywhere;
  }
  .card :global(p) {
    margin: 0;
  }
</style>
