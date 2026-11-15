<script lang="ts">
  import { device } from "./lib/device.svelte.ts";
  import External from "./lib/External.svelte";
  import Logo from "./lib/Logo.svelte";
  import { PAGES, router } from "./lib/router.svelte.ts";
  import { DOCS, health } from "./lib/view.ts";
  import Logs from "./pages/Logs.svelte";
  import Network from "./pages/Network.svelte";
  import Overview from "./pages/Overview.svelte";
  import Settings from "./pages/Settings.svelte";
  import Updates from "./pages/Updates.svelte";

  const s = $derived(device.status);
  const line = $derived(health(s, device.link, device.offlineSince, device.restarting));
  const page = $derived(PAGES.find((p) => p.path === router.path)!);

  $effect(() => {
    const where = page.path === "/" ? "" : `${page.title} · `;
    document.title = s ? `${where}${s.name} · Pinna` : "Pinna";
  });
</script>

<header class="top">
  <div class="bar">
    <a class="home" href="#/" aria-label="Pinna, overview"><Logo /></a>
    <nav aria-label="Sections">
      {#each PAGES as p (p.path)}
        <a href="#{p.path}" aria-current={p.path === router.path ? "page" : undefined}>{p.title}</a>
      {/each}
    </nav>
    {#if s}
      <span class="pill pn-health" data-tone={line.tone} title={line.text}>
        <span class="pn-type-data">{s.name}</span>
      </span>
    {/if}
  </div>
</header>

{#if device.restarting && device.link !== "live"}
  <div class="strip neutral" role="status">
    <span class="wrap">Restarting. The satellite is back in a minute or two; this page reconnects on its own.</span>
  </div>
{:else if device.link === "offline"}
  <div class="strip" role="status">
    <span class="wrap">{line.text}. Trying again…</span>
  </div>
{/if}

<main class="wrap">
  {#if !s}
    <p class="connecting" role="status">
      {device.link === "offline" ? "Can't reach the satellite. Trying again…" : "Connecting…"}
    </p>
  {:else if page.path === "/"}
    <Overview />
  {:else}
    <h1 class="pn-type-display-m title">{page.title}</h1>
    {#if page.path === "/settings"}<Settings />
    {:else if page.path === "/updates"}<Updates />
    {:else if page.path === "/network"}<Network />
    {:else if page.path === "/logs"}<Logs />{/if}
  {/if}
</main>

<footer class="wrap">
  <div class="foot">
    <span>Pinna{s ? ` ${s.version}` : ""}</span>
    <External href={DOCS}>Docs</External>
    <External href="https://github.com/elsbrock/pinna">Source</External>
  </div>
</footer>

<style>
  .wrap,
  .bar {
    box-sizing: border-box;
    width: 100%;
    max-width: var(--page-max);
    margin: 0 auto;
    padding-left: var(--space-4);
    padding-right: var(--space-4);
  }
  .top {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--surface-raised);
    border-bottom: 1px solid var(--line);
  }
  .bar {
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-areas: "home pill" "nav nav";
    align-items: center;
    column-gap: var(--space-6);
    padding-top: var(--space-3);
  }
  .home {
    grid-area: home;
    border-radius: var(--radius-sm);
    text-decoration: none;
  }
  .home:focus-visible,
  nav a:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }
  nav {
    grid-area: nav;
    display: flex;
    gap: var(--space-1);
    overflow-x: auto;
    scrollbar-width: none;
    margin: var(--space-2) calc(-1 * var(--space-4)) 0;
    padding: 0 var(--space-4);
  }
  nav a {
    flex: none;
    display: flex;
    align-items: center;
    height: var(--control-lg);
    padding: 0 var(--space-3);
    border-bottom: 2px solid transparent;
    color: var(--ink-muted);
    font-weight: 650;
    text-decoration: none;
    transition: color 0.12s, border-color 0.12s;
  }
  nav a:hover {
    color: var(--ink);
  }
  nav a[aria-current="page"] {
    color: var(--ink);
    border-bottom-color: var(--signal);
  }
  .pill {
    grid-area: pill;
    justify-self: end;
    font-size: 13px;
    min-width: 0;
  }
  .pill .pn-type-data {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .strip {
    background: var(--danger-soft);
    color: var(--danger);
    font-weight: 650;
    padding: var(--space-2) 0;
  }
  .strip.neutral {
    background: var(--surface-sunken);
    color: var(--ink);
  }
  main {
    display: flex;
    flex-direction: column;
    gap: var(--space-6);
    padding-top: var(--space-6);
    padding-bottom: var(--space-8);
  }
  .title {
    margin: 0;
  }
  .connecting {
    margin: var(--space-8) 0;
    color: var(--ink-muted);
  }
  .foot {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
    padding-top: var(--space-4);
    padding-bottom: var(--space-8);
    border-top: 1px solid var(--line);
    font-size: 13px;
    line-height: 18px;
    color: var(--ink-muted);
  }
  .foot :global(.ext) {
    color: inherit;
  }
  @media (min-width: 900px) {
    .wrap,
    .bar {
      padding-left: var(--space-8);
      padding-right: var(--space-8);
    }
    .bar {
      grid-template-columns: auto 1fr auto;
      grid-template-areas: "home nav pill";
      padding-top: 0;
    }
    nav {
      margin: 0;
      padding: 0;
    }
    nav a {
      height: 64px;
    }
  }
</style>
