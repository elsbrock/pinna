<!-- The shell: a sidebar with the satellite and the sections from 760px up; an
     app bar and a tab bar on phones. -->
<script lang="ts">
  import Device from "./lib/Device.svelte";
  import { isStandalone } from "./lib/assistant.ts";
  import { device } from "./lib/device.svelte.ts";
  import External from "./lib/External.svelte";
  import Icon from "./lib/Icon.svelte";
  import Logo from "./lib/Logo.svelte";
  import Ring from "./lib/Ring.svelte";
  import Satellites from "./lib/Satellites.svelte";
  import { SECTIONS, pageOf, router, subpagesOf } from "./lib/router.svelte.ts";
  import Toasts from "./lib/Toasts.svelte";
  import { wifi } from "./lib/wifi.svelte.ts";
  import WifiTrial from "./lib/WifiTrial.svelte";
  import { displayName, DOCS, health, healthWord, ringEffect } from "./lib/view.ts";
  import Assistant from "./pages/assistant/Assistant.svelte";
  import Conversation from "./pages/assistant/Conversation.svelte";
  import Services from "./pages/assistant/Services.svelte";
  import Tools from "./pages/assistant/Tools.svelte";
  import LightRing from "./pages/LightRing.svelte";
  import Logs from "./pages/Logs.svelte";
  import Network from "./pages/Network.svelte";
  import Overview from "./pages/Overview.svelte";
  import System from "./pages/System.svelte";
  import MediaSources from "./pages/MediaSources.svelte";
  import Voice from "./pages/Voice.svelte";
  import WakeWord from "./pages/WakeWord.svelte";
  import WakeWords from "./pages/WakeWords.svelte";

  const s = $derived(device.status);
  const line = $derived(health(s, device.link, device.offlineSince, device.restarting));
  const ring = $derived(ringEffect(s, device.link, device.testStep));
  const page = $derived(pageOf(router.path));
  const groups = [...new Set(SECTIONS.map((x) => x.group))];

  // The satellite switcher (ADR 0021), opened from the sidebar's device box or,
  // on phones, the name in the app bar.
  let switcher = $state<"side" | "bar" | null>(null);
  let sideButton = $state<HTMLButtonElement>();
  let barButton = $state<HTMLButtonElement>();
  const toggle = (from: "side" | "bar") => (switcher = switcher === from ? null : from);

  // A Wi-Fi network on trial shows on every page (ADR 0008).
  $effect(() => wifi.watch());

  $effect(() => {
    const where = page.path === "/" ? "" : `${page.title} · `;
    document.title = s ? `${where}${displayName(s)} · Pinna` : "Pinna";
  });
</script>

<div class="shell">
  <aside class="side">
    <a class="home" href="#/" aria-label="Pinna, overview">
      {#if s}
        <Logo><Ring effect={ring} frame={device.ring} size={24} label="Ring: {ring.replace('-', ' ')}" /></Logo>
      {:else}
        <Logo />
      {/if}
    </a>
    <nav class="pn-nav" aria-label="Sections">
      {#each groups as g (g)}
        <span class="pn-nav-label">{g}</span>
        {#each SECTIONS.filter((x) => x.group === g) as x (x.path)}
          {@const open = page.section.path === x.path}
          <!-- The open section: "page" on its own page, "true" while one of its sub-pages is. -->
          <a class="pn-nav-item" href="#{x.path}" aria-current={open ? (page.path === x.path ? "page" : "true") : undefined}>
            <Icon name={x.icon} />{x.title}
          </a>
          {#if open}
            {#each subpagesOf(x.path) as sub (sub.path)}
              <a class="pn-nav-sub" href="#{sub.path}" aria-current={page.path === sub.path ? "page" : undefined}>
                {sub.title}
              </a>
            {/each}
          {/if}
        {/each}
      {/each}
    </nav>
    {#if s}
      <button
        bind:this={sideButton}
        class="pn-ident"
        type="button"
        aria-label="Switch satellite: {displayName(s)}, {healthWord(line)}"
        aria-haspopup="dialog"
        aria-expanded={switcher === "side"}
        onclick={() => toggle("side")}
      >
        <Device model={s.model} effect={ring} frame={device.ring} width={48} label="Ring: {ring.replace('-', ' ')}" />
        <span class="pn-ident-name">{displayName(s)}</span>
        <span class="pn-health" data-tone={line.tone} title={line.text}
          >{isStandalone(s) ? "Standalone" : "Home Assistant"}</span
        >
        <Icon name="chevron-down" class="pn-icon pn-ident-chev" />
      </button>
    {:else}
      <span class="pn-skeleton ident-skeleton"></span>
    {/if}
    <div class="side-foot">
      <span>Pinna{s ? ` ${s.version}` : ""}</span>
      <External href={DOCS}>Docs</External>
      <External href="https://github.com/elsbrock/pinna">Source</External>
    </div>
  </aside>

  <header class="appbar">
    {#if s}
      <Ring effect={ring} frame={device.ring} size={32} label="Ring: {ring.replace('-', ' ')}" />
      <button
        bind:this={barButton}
        class="appbar-name"
        type="button"
        aria-label="Switch satellite: {displayName(s)}"
        aria-haspopup="dialog"
        aria-expanded={switcher === "bar"}
        onclick={() => toggle("bar")}
      >
        <span class="appbar-text">{displayName(s)}</span>
        <Icon name="chevron-down" />
      </button>
      <span class="pn-health" data-tone={line.tone} title={line.text}>{healthWord(line)}</span>
    {:else}
      <a class="home" href="#/" aria-label="Pinna, overview"><Logo /></a>
    {/if}
  </header>

  <div class="main">
    {#if device.restarting && device.resetting && device.link !== "live"}
      <div class="pn-banner" data-tone="neutral" role="status">
        <Icon name="restart" />
        <p>
          Resetting. The satellite restarts as it was installed. If its image doesn't set up Wi-Fi, it is then at
          <code class="pn-code">http://10.42.0.1/</code> over USB only; otherwise this page reconnects on its own.
        </p>
      </div>
    {:else if device.restarting && device.link !== "live"}
      <div class="pn-banner" data-tone="neutral" role="status">
        <Icon name="restart" />
        <p>Restarting. The satellite is back in a minute or two; this page reconnects on its own.</p>
      </div>
    {:else if device.link === "offline" && wifi.lost}
      <div class="pn-banner" data-tone="neutral" role="status">
        <Icon name="wifi" />
        <p>
          The satellite is probably on {wifi.lost} now. Within 5 minutes, connect to {wifi.lost} and open
          <code class="pn-code">http://{s?.name ?? "pinna"}.local/</code> or its address from your router: it then
          stays there. Otherwise it comes back here, and this page finds it again.
        </p>
      </div>
    {:else if device.link === "offline" && device.misdirected}
      <div class="pn-banner" role="status">
        <Icon name="alert" />
        <p>
          This satellite no longer answers to the name {location.hostname}: it isn't its dashboard address any more. Open
          it by its IP address or its .local name, and set {location.hostname} as the dashboard address again under
          Network.
        </p>
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
        <WifiTrial />
        <Overview />
      {:else}
        <WifiTrial />
        {#if page.path === "/voice"}<Voice />
        {:else if page.path === "/voice/wake-word"}<WakeWord />
        {:else if page.path === "/voice/wake-words"}<WakeWords />
        {:else if page.path === "/voice/media-sources"}<MediaSources />
        {:else if page.path === "/voice/ring"}<LightRing />
        {:else if page.path === "/assistant"}<Assistant />
        {:else if page.path === "/assistant/services"}<Services />
        {:else if page.path === "/assistant/conversation"}<Conversation />
        {:else if page.path === "/assistant/tools"}<Tools />
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

{#if switcher && (switcher === "side" ? sideButton : barButton)}
  <Satellites
    anchor={(switcher === "side" ? sideButton : barButton)!}
    placement={switcher === "side" ? "up" : "down"}
    overview={switcher === "bar"}
    onclose={() => (switcher = null)}
  />
{/if}

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
  /* The name opens the satellite switcher; the overview link is in it. */
  .appbar-name {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
    border-radius: var(--radius-xs);
    background: none;
    color: var(--ink);
    font: inherit;
    font-size: 16px;
    font-weight: 700;
    cursor: pointer;
  }
  .appbar-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .appbar-name :global(.pn-icon) {
    flex: none;
    width: 16px;
    height: 16px;
    color: var(--ink-muted);
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
    /* The satellite sits at the bottom, above the links. */
    .pn-ident,
    .ident-skeleton {
      margin-top: auto;
      margin-bottom: 6px;
    }
    .ident-skeleton {
      height: 64px;
    }
    .side-foot {
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
