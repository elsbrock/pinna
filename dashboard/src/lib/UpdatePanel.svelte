<!-- The A/B slots and installing an update: Software, then Install an update.
     pinnad runs the install, so it survives a closed tab; this page follows it
     with WatchInstall and shows its stages as steps. -->
<script lang="ts">
  import { ConnectError } from "@connectrpc/connect";
  import { tick } from "svelte";

  import type { GetUpdatesResponse } from "../gen/pinna/v1/device_pb.ts";
  import Confirm from "./Confirm.svelte";
  import { client, device } from "./device.svelte.ts";
  import External from "./External.svelte";
  import Group from "./Group.svelte";
  import Icon from "./Icon.svelte";
  import { toasts } from "./toast.svelte.ts";
  import {
    DOCS,
    downloadProgress,
    installSteps,
    slotWord,
    switchTarget,
    targetSlot,
    updatePhase,
  } from "./view.ts";

  const STORE = "pinna.manifest";
  const REFRESH_MS = 10_000;

  let u = $state<GetUpdatesResponse | null>(null);
  let error = $state<string | null>(null);
  let manifest = $state(read());
  let urlError = $state<string | null>(null);
  let lines = $state<string[]>([]);
  let stage = $state(0);
  let progress = $state<{ text: string; fraction: number } | null>(null);
  let result = $state<{ ok: boolean } | null>(null);
  let starting = $state(false);
  let restartError = $state<string | null>(null);
  let confirmingSwitch = $state(false);
  let switchError = $state<string | null>(null);
  let keeping = $state(false);
  let switchButton = $state<HTMLButtonElement>();
  let watching: AbortController | null = null;
  // The install whose output is on screen, by when it finished.
  let shown: bigint | null = null;

  function read(): string {
    try {
      return localStorage.getItem(STORE) ?? "";
    } catch {
      return "";
    }
  }

  async function refresh() {
    try {
      u = await client.getUpdates({}, { timeoutMs: 10_000 });
      error = null;
    } catch (e) {
      error = ConnectError.from(e).rawMessage;
      return;
    }
    if (!manifest && u.suggestedManifest) manifest = u.suggestedManifest;
    if (watching) return;
    if (u.installing) {
      void watch();
    } else if (u.lastInstall && u.lastInstall.finishedUnixMs !== shown) {
      // A finished install: replay its output once, unless this page saw it live.
      shown = u.lastInstall.finishedUnixMs;
      if (!result) void watch();
    }
  }

  async function watch() {
    const abort = (watching = new AbortController());
    const seen: string[] = [];
    let live = false;
    stage = 0;
    progress = null;
    try {
      for await (const m of client.watchInstall({}, { signal: abort.signal })) {
        if (m.done) {
          result = { ok: m.ok };
          break;
        }
        seen.push(m.line);
        lines = [...seen];
        if (m.stage > stage) stage = m.stage;
        progress = downloadProgress(m.line) ?? progress;
        live ||= !!u?.installing;
      }
    } catch {
      // The page went away or the satellite restarted; GetUpdates tells the rest.
    }
    watching = null;
    // After a live install, read the slots once to show what changed.
    if (live && !abort.signal.aborted) void refresh();
  }

  async function install() {
    const url = manifest.trim();
    if (!/^https?:\/\/\S+$/.test(url)) {
      urlError = "Start the address with http:// or https://.";
      return;
    }
    urlError = null;
    try {
      localStorage.setItem(STORE, url);
    } catch {
      // Private mode: the URL is just not remembered.
    }
    starting = true;
    lines = [];
    stage = 0;
    progress = null;
    result = null;
    error = null;
    try {
      await client.installUpdate({ manifest: url });
      await refresh();
    } catch (e) {
      error = ConnectError.from(e).rawMessage;
    } finally {
      starting = false;
    }
  }

  async function restart() {
    restartError = await device.restart();
  }

  // SetActiveSlot restarts the satellite itself, into the other slot.
  async function switchSlot() {
    if (!other) return;
    const slot = other.name;
    switchError = await device.restart(() => client.setActiveSlot({ slot }));
  }

  // The running slot boots next again; the installed update stays in its slot.
  async function keep() {
    if (!booted || !next) return;
    const kept = next;
    keeping = true;
    error = null;
    try {
      await client.setActiveSlot({ slot: booted.name });
      lines = [];
      stage = 0;
      progress = null;
      result = null;
      await refresh();
      toasts.show(`${kept.version || "The update"} waits in slot ${kept.name.toUpperCase()}. Switch starts it.`, "ok");
      await tick();
      switchButton?.focus();
    } catch (e) {
      error = ConnectError.from(e).rawMessage;
    } finally {
      keeping = false;
    }
  }

  // Read now, every 10 s, and again when the satellite is back after a restart.
  $effect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible" && device.link === "live") void refresh();
    }, REFRESH_MS);
    return () => {
      clearInterval(timer);
      watching?.abort();
    };
  });
  let wasLive = device.link === "live";
  $effect(() => {
    const live = device.link === "live";
    if (live && !wasLive) void refresh();
    wasLive = live;
  });

  const phase = $derived(u ? updatePhase(u) : null);
  const booted = $derived(u?.slots.find((s) => s.booted));
  const next = $derived(u?.slots.find((s) => s.active));
  const target = $derived(u ? targetSlot(u) : undefined);
  const other = $derived(u ? switchTarget(u) : undefined);
  const live = $derived(device.link === "live");
  // While unreachable, the banner says so; a raw transport error adds nothing.
  const shownError = $derived(live ? error : null);
  const running = $derived(phase === "installing" || starting);
  const steps = $derived(installSteps(stage, result));
  const ucfirst = (t: string) => t.charAt(0).toUpperCase() + t.slice(1);
</script>

{#snippet pendingActions()}
  <button class="pn-btn pn-btn-primary" disabled={!live || device.busy || device.restarting || keeping} onclick={restart}>
    <Icon name="restart" />{device.restarting ? "Restarting…" : "Restart now"}
  </button>
  <button class="pn-btn" disabled={!live || device.busy || device.restarting || keeping} onclick={keep}>
    Start it later
  </button>
{/snippet}

{#if !u}
  {#if shownError}
    <p class="pn-field-error" role="alert"><Icon name="alert" />Can't read the update state: {shownError}</p>
  {:else}
    <span class="pn-skeleton" style:height="120px" aria-hidden="true"></span>
  {/if}
{:else if phase === "unavailable"}
  <Group title="Software">
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Updates aren't available here</span>
        <span class="pn-row-hint">
          {ucfirst(u.unavailableReason)}. <External href="{DOCS}update/">How updates work</External>
        </span>
      </div>
    </div>
  </Group>
{:else}
  <Group title="Software">
    {#each u.slots as s (s.name)}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Slot {s.name.toUpperCase()}</span>
          <span class="pn-row-hint" class:is-caution={s.state === "trial" && (s.active || s.booted)} class:is-danger={s.state === "unbootable"}>
            {[slotWord(s), s.booted && "running", s.active && !s.booted && "starts next"].filter(Boolean).join(", ")}
          </span>
        </div>
        <span class="pn-row-value is-mono">{s.version || "—"}</span>
      </div>
    {/each}
    {#if other}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Switch to slot {other.name.toUpperCase()}</span>
          <span class="pn-row-hint">Restart into {other.version}, on trial.</span>
        </div>
        <button
          bind:this={switchButton}
          class="pn-btn"
          disabled={!live || device.busy || device.restarting}
          onclick={() => {
            switchError = null;
            confirmingSwitch = true;
          }}
        >
          <Icon name="restart" />Switch…
        </button>
      </div>
      {#if switchError}
        <div class="pn-row"><p class="pn-field-error" role="alert"><Icon name="alert" />{switchError}</p></div>
      {/if}
    {/if}
  </Group>

  {#if other}
    <Confirm
      bind:open={confirmingSwitch}
      title="Switch to {other.version}?"
      action="Switch and restart"
      confirm={switchSlot}
    >
      <p>
        The satellite restarts into slot {other.name.toUpperCase()} and tries {other.version}. It stops listening for a
        minute or two; this page reconnects on its own.
      </p>
      <p>If {other.version} doesn't come up, the satellite goes back to {u.bootedVersion} on its own.</p>
    </Confirm>
  {/if}

  {#if phase === "trial"}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">This version is on trial</p>
        <p>
          It becomes the new fallback once its health check passes, within 5 minutes of starting. You can install
          another update or switch back after that. If it fails, the satellite goes back to the previous version on
          its own.
        </p>
      </div>
    </div>
  {:else if phase === "pending" && !lines.length}
    <div class="pn-notice" data-tone="ok" role="status">
      <Icon name="info" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">Version {next?.version || "?"} is ready in slot {next?.name.toUpperCase()}</p>
        <p>It starts on the next restart, as a trial. If it doesn't come up, the satellite returns to this version.</p>
        <p class="actions">{@render pendingActions()}</p>
        {#if restartError}<p class="pn-field-error" role="alert"><Icon name="alert" />{restartError}</p>{/if}
      </div>
    </div>
  {/if}

  {#if phase === "ready" || phase === "installing" || lines.length || result}
    <Group title="Install an update">
      {#if lines.length || result || running}
        <ol class="pn-steps" aria-live="polite">
          {#each steps as step, i (step.label)}
            <li class="pn-step" data-state={step.state}>
              <span class="pn-step-mark">{#if step.state === "done"}<Icon name="check" />{/if}</span>
              <span class="pn-step-label">{step.label}</span>
              {#if i === 1 && progress && step.state !== "pending"}
                <span class="pn-step-detail">{progress.text}</span>
                {#if step.state === "current"}
                  <span class="pn-step-bar"><span style:--pct="{(progress.fraction * 100).toFixed(0)}%"></span></span>
                {/if}
              {/if}
              {#if i === 3 && step.state === "current"}
                <span class="pn-step-detail">
                  {next?.version || "The new version"} tries slot {next?.name.toUpperCase() ?? "?"} first and falls back to
                  {u.bootedVersion} if it doesn't come up.
                </span>
                {#if result?.ok && phase === "pending"}
                  <span class="actions restart">{@render pendingActions()}</span>
                {/if}
              {/if}
            </li>
          {/each}
        </ol>
        {#if result && !result.ok}
          <div class="pn-row">
            <p class="pn-field-error" role="alert">
              <Icon name="alert" />The update was not installed: {u.lastInstall?.lastLine || "see the output"}. The
              satellite keeps running {u.bootedVersion}.
            </p>
          </div>
        {/if}
        {#if restartError}
          <div class="pn-row"><p class="pn-field-error" role="alert"><Icon name="alert" />{restartError}</p></div>
        {/if}
        <details class="output">
          <summary>Output on the satellite</summary>
          <pre>{lines.join("\n")}</pre>
        </details>
      {/if}

      {#if !running && (phase === "ready" || (result && !result.ok))}
        <form
          class="pn-row is-stack"
          onsubmit={(e) => {
            e.preventDefault();
            void install();
          }}
        >
          {#if target?.state === "unbootable" && target.version}
            <p class="pn-field-hint">
              Slot {target.name.toUpperCase()} holds {target.version} but can't boot: an update there didn't finish or
              didn't pass its check. Installing replaces it.
            </p>
          {/if}
          <div class="pn-field">
            <label class="pn-field-label" for="manifest">Release manifest</label>
            <div class="install">
              <input
                id="manifest"
                class="pn-input is-mono"
                type="url"
                inputmode="url"
                autocomplete="url"
                spellcheck="false"
                placeholder="http://192.168.1.20:8000/manifest.json"
                bind:value={manifest}
                aria-describedby="manifest-hint"
                aria-invalid={urlError ? "true" : undefined}
                oninput={() => (urlError = null)}
              />
              <button class="pn-btn pn-btn-primary" type="submit" disabled={!manifest.trim() || !live || starting}>
                <Icon name="download" />Install
              </button>
            </div>
            {#if urlError}<span class="pn-field-error" role="alert"><Icon name="alert" />{urlError}</span>{/if}
            <span id="manifest-hint" class="pn-field-hint">
              Installing writes slot {target?.name.toUpperCase() ?? "?"}; {u.bootedVersion} stays in slot
              {booted?.name.toUpperCase()} as the fallback. Only images signed with your release key install. Serve a
              release with <code class="pn-code">python3 -m http.server 8000 --directory build/release</code>.
            </span>
          </div>
        </form>
      {/if}
    </Group>
  {/if}

  {#if shownError}
    <p class="pn-field-error" role="alert"><Icon name="alert" />{ucfirst(shownError)}.</p>
  {/if}
{/if}

<style>
  p {
    margin: 0;
  }
  .is-caution {
    color: var(--caution);
    font-weight: 650;
  }
  .is-danger {
    color: var(--danger);
    font-weight: 650;
  }
  .install {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .install input {
    flex: 1 1 260px;
    width: auto;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .restart {
    margin-top: var(--space-2);
  }
  .output {
    border-top: 1px solid var(--line);
  }
  .output summary {
    padding: var(--space-3) var(--space-4);
    cursor: pointer;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-muted);
  }
  .output summary:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--focus);
  }
  .output pre {
    margin: 0;
    max-height: 280px;
    overflow: auto;
    padding: var(--space-3) var(--space-4);
    border-top: 1px solid var(--line);
    background: var(--surface-sunken);
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 20px;
  }
</style>
