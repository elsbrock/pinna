<!-- The shell: a sidebar with the satellite and the sections from 760px up; an
     app bar and a tab bar on phones. -->
<script lang="ts">
  import Device from "./lib/Device.svelte";
  import { device } from "./lib/device.svelte.ts";
  import External from "./lib/External.svelte";
  import Icon from "./lib/Icon.svelte";
  import Logo from "./lib/Logo.svelte";
  import Ring from "./lib/Ring.svelte";
  import { SECTIONS, pageOf, router } from "./lib/router.svelte.ts";
  import Toasts from "./lib/Toasts.svelte";
  import { DOCS, health, healthWord, ringEffect } from "./lib/view.ts";
  import HomeAssistant from "./pages/HomeAssistant.svelte";
  import Logs from "./pages/Logs.svelte";
  import Network from "./pages/Network.svelte";
  import Overview from "./pages/Overview.svelte";
  import System from "./pages/System.svelte";
  import Voice from "./pages/Voice.svelte";

  const s = $derived(device.status);
  const line = $derived(health(s, device.link, device.offlineSince, device.restarting));
  const ring = $derived(ringEffect(s, device.link));
  const page = $derived(pageOf(router.path));
  const groups = [...new Set(SECTIONS.map((x) => x.group))];

  $effect(() => {
    const where = page.path === "/" ? "" : `${page.title} · `;
    document.title = s ? `${where}${s.name} · Pinna` : "Pinna";
  });
</script>

<div class="shell">
  <aside class="side">
    <a class="home" href="#/" aria-label="Pinna, overview"><Logo /></a>
    {#if s}
      <div class="pn-ident">
        <Ring effect={ring} size={32} label="Ring: {ring.replace('-', ' ')}" />
        <span class="pn-ident-name">{s.name}</span>
        <span class="pn-health" data-tone={line.tone}>{line.text}</span>
      </div>
    {:else}
      <span class="pn-skeleton ident-skeleton"></span>
    {/if}
    <nav class="pn-nav" aria-label="Sections">
      {#each groups as g (g)}
        <span class="pn-nav-label">{g}</span>
        {#each SECTIONS.filter((x) => x.group === g) as x (x.path)}
          <a class="pn-nav-item" href="#{x.path}" aria-current={page.section.path === x.path ? "page" : undefined}>
            <Icon name={x.icon} />{x.title}
          </a>
        {/each}
      {/each}
    </nav>
    <div class="side-foot">
      <span>Pinna{s ? ` ${s.version}` : ""}</span>
      <External href={DOCS}>Docs</External>
      <External href="https://github.com/elsbrock/pinna">Source</External>
    </div>
  </aside>

  <header class="appbar">
    {#if s}
      <Ring effect={ring} size={32} label="Ring: {ring.replace('-', ' ')}" />
      <a class="appbar-name" href="#/">{s.name}</a>
      <span class="pn-health" data-tone={line.tone} title={line.text}>{healthWord(line)}</span>
    {:else}
      <a class="home" href="#/" aria-label="Pinna, overview"><Logo /></a>
    {/if}
  </header>

  <div class="main">
    {#if device.restarting && device.link !== "live"}
      <div class="pn-banner" data-tone="neutral" role="status">
        <Icon name="restart" />
        <p>Restarting. The satellite is back in a minute or two; this page reconnects on its own.</p>
      </div>
    {:else if device.link === "offline"}
      <div class="pn-banner" role="status">
        <Icon name="alert" />
        <p>
          {line.text}. Check that it has power and Wi-Fi; over USB it is at
          <code class="pn-code">http://10.42.0.1/</code>. This page keeps trying.
        </p>
      </div>
    {/if}

    <main class="content">
      {#if page.path !== "/"}
        <header class="pn-pagehead">
          <div>
            {#if page.section.path !== page.path}
              <a class="pn-crumb" href="#{page.section.path}"><Icon name="chevron-left" />{page.section.title}</a>
            {/if}
            <h1 class="pn-type-display-m">{page.title}</h1>
            <p>{page.description}</p>
          </div>
        </header>
      {/if}
      {#if !s}
        {#if device.link === "offline"}
          <div class="pn-empty">
            <Device model="" effect="idle" width={112} label="Ring off" />
            <span class="pn-empty-title">Waiting for the satellite</span>
            <p>This page fills in as soon as it answers.</p>
          </div>
        {:else}
          <p class="sr" role="status">Connecting…</p>
          <div class="skeleton" aria-hidden="true">
            {#if page.path === "/"}
              <span class="pn-skeleton" style:height="220px"></span>
              <span class="tiles">
                {#each [0, 1, 2, 3] as i (i)}<span class="pn-skeleton" style:height="92px"></span>{/each}
              </span>
            {:else}
              {#each [0, 1] as i (i)}<span class="pn-skeleton" style:height="140px"></span>{/each}
            {/if}
          </div>
        {/if}
      {:else if page.path === "/"}
        <Overview />
      {:else}
        {#if page.path === "/voice"}<Voice />
        {:else if page.path === "/home-assistant"}<HomeAssistant />
        {:else if page.path === "/network"}<Network />
        {:else if page.path === "/system"}<System />
        {:else if page.path === "/system/logs"}<Logs />{/if}
      {/if}
    </main>
  </div>

  <nav class="pn-tabbar tabbar" aria-label="Sections">
    {#each SECTIONS as x (x.path)}
      <a class="pn-tab" href="#{x.path}" aria-current={page.section.path === x.path ? "page" : undefined}>
        <span class="pn-tab-icon"><Icon name={x.icon} /></span>{x.short}
      </a>
    {/each}
  </nav>
</div>

<Toasts />

<style>
  .shell {
    min-height: 100dvh;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .home {
    border-radius: var(--radius-sm);
    text-decoration: none;
  }
  .home:focus-visible,
  .appbar-name:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }

  /* Phones: app bar on top, tab bar at the bottom. */
  .side {
    display: none;
  }
  .appbar {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    height: var(--bar-h);
    padding: env(safe-area-inset-top, 0px) max(var(--space-4), env(safe-area-inset-right, 0px)) 0
      max(var(--space-4), env(safe-area-inset-left, 0px));
    background: var(--surface-raised);
    border-bottom: 1px solid var(--line);
  }
  .appbar-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    border-radius: var(--radius-xs);
    color: var(--ink);
    font-size: 16px;
    font-weight: 700;
    text-decoration: none;
  }
  .appbar .pn-health {
    flex: none;
    margin-left: auto;
    font-size: 13px;
  }
  .tabbar {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 2;
    padding-bottom: env(safe-area-inset-bottom, 0px);
  }
  .main {
    padding-bottom: calc(var(--tabbar-h) + env(safe-area-inset-bottom, 0px));
  }
  :global(.pn-toasts) {
    bottom: calc(var(--tabbar-h) + var(--space-3) + env(safe-area-inset-bottom, 0px));
  }
  .content {
    box-sizing: border-box;
    width: 100%;
    max-width: 1000px;
    margin: 0 auto;
    padding: var(--space-6) max(var(--space-4), env(safe-area-inset-right, 0px)) var(--space-8)
      max(var(--space-4), env(safe-area-inset-left, 0px));
    display: flex;
    flex-direction: column;
    gap: var(--space-6);
  }
  .skeleton {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-3);
  }

  /* From 760px: the sidebar. */
  @media (min-width: 760px) {
    .shell {
      display: grid;
      grid-template-columns: var(--nav-w) minmax(0, 1fr);
    }
    .side {
      position: sticky;
      top: 0;
      height: 100dvh;
      overflow-y: auto;
      box-sizing: border-box;
      display: flex;
      flex-direction: column;
      gap: var(--space-1);
      padding: var(--space-4) var(--space-3) var(--space-3);
      background: var(--surface-raised);
      border-right: 1px solid var(--line);
      box-shadow: var(--shadow-raised);
    }
    .side .home {
      align-self: flex-start;
      padding: 2px var(--space-2) 14px;
    }
    .side :global(.pn-logo-word) {
      font-size: 18px;
    }
    .side :global(.mark) {
      width: 24px;
      height: 24px;
    }
    .pn-ident {
      margin-bottom: 6px;
    }
    .ident-skeleton {
      height: 64px;
      margin-bottom: 6px;
    }
    .side-foot {
      margin-top: auto;
      display: flex;
      flex-wrap: wrap;
      gap: var(--space-1) 14px;
      padding: var(--space-3) 10px 2px;
      border-top: 1px solid var(--line);
      font-size: 13px;
      line-height: 18px;
      color: var(--ink-muted);
    }
    .side-foot :global(.ext) {
      color: inherit;
    }
    .appbar,
    .tabbar {
      display: none;
    }
    .main {
      padding-bottom: 0;
    }
    :global(.pn-toasts) {
      bottom: calc(var(--space-6) + env(safe-area-inset-bottom, 0px));
      /* Centred on the content, not on the sidebar too. */
      padding-left: calc(var(--nav-w) + var(--space-4));
    }
    .content {
      padding: var(--space-8);
      gap: 28px;
    }
    .tiles {
      grid-template-columns: repeat(4, minmax(0, 1fr));
    }
  }
</style>
