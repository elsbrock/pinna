<script lang="ts">
  import { ConnectError } from "@connectrpc/connect";

  import type { GetUpdatesResponse } from "../gen/pinna/v1/device_pb.ts";
  import { client, device } from "../lib/device.svelte.ts";
  import External from "../lib/External.svelte";
  import Icon from "../lib/Icon.svelte";
  import { DOCS, slotWord, targetSlot, updatePhase } from "../lib/view.ts";

  const STORE = "pinna.manifest";
  const REFRESH_MS = 10_000;

  let u = $state<GetUpdatesResponse | null>(null);
  let error = $state<string | null>(null);
  let manifest = $state(read());
  let urlError = $state<string | null>(null);
  let lines = $state<string[]>([]);
  let result = $state<{ ok: boolean } | null>(null);
  let starting = $state(false);
  let restartError = $state<string | null>(null);
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
    try {
      for await (const m of client.watchInstall({}, { signal: abort.signal })) {
        if (m.done) {
          result = { ok: m.ok };
          break;
        }
        seen.push(m.line);
        lines = [...seen];
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
  const live = $derived(device.link === "live");
  // While unreachable, the header strip says so; a raw transport error adds nothing.
  const shownError = $derived(live ? error : null);
</script>

{#if !u}
  {#if shownError}
    <p class="pn-field-error" role="alert"><Icon name="alert" />Can't read the update state: {shownError}</p>
  {:else}
    <p class="muted" role="status">Reading the slots…</p>
  {/if}
{:else if phase === "unavailable"}
  <div class="pn-notice" role="status">
    <Icon name="info" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">Updates aren't available here</p>
      <p>
        {u.unavailableReason.charAt(0).toUpperCase() + u.unavailableReason.slice(1)}.
        <External href="{DOCS}update/">How updates work</External>
      </p>
    </div>
  </div>
{:else}
  <section class="panel" aria-labelledby="running-title">
    <h2 id="running-title" class="pn-type-heading">
      Running <span class="pn-type-data">{u.bootedVersion || "?"}</span> from slot {booted?.name ?? "?"}
    </h2>
    <table>
      <thead>
        <tr><th scope="col">Slot</th><th scope="col">Version</th><th scope="col">State</th><th scope="col"></th></tr>
      </thead>
      <tbody>
        {#each u.slots as s (s.name)}
          <tr>
            <td class="mono">{s.name}</td>
            <td class="mono">{s.version || "—"}</td>
            <td data-tone={s.state}>{slotWord(s)}</td>
            <td class="muted">
              {[s.booted && "Running", s.active && !s.booted && "Starts next"].filter(Boolean).join(", ")}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>

  {#if phase === "trial"}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">This version is on trial</p>
        <p>
          It becomes the new fallback once its health check passes, within 5 minutes of starting. You can install
          another update after that. If it fails, the satellite goes back to the previous version on its own.
        </p>
      </div>
    </div>
  {:else if phase === "pending"}
    <div class="pn-notice" data-tone="ok" role="status">
      <Icon name="info" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">
          Version {next?.version || "?"} is ready in slot {next?.name}
        </p>
        <p>It starts on the next restart, as a trial. If it doesn't come up, the satellite returns to this version.</p>
        <p>
          <button
            class="pn-btn pn-btn-primary"
            disabled={!live || device.busy || device.restarting}
            onclick={restart}
          >
            {device.restarting ? "Restarting…" : "Restart now"}
          </button>
        </p>
        {#if restartError}<p class="pn-field-error" role="alert"><Icon name="alert" />{restartError}</p>{/if}
      </div>
    </div>
  {/if}

  {#if phase === "ready" || phase === "installing"}
    <section class="panel" aria-labelledby="install-title">
      <h2 id="install-title" class="pn-type-heading">Install an update</h2>
      {#if target?.state === "unbootable" && target.version}
        <p class="muted">
          Slot {target.name} holds {target.version} but can't boot: an update there didn't finish or didn't pass its
          check. Installing replaces it.
        </p>
      {/if}
      <div class="pn-notice" data-tone="caution">
        <Icon name="alert" class="pn-icon pn-notice-icon" />
        <div class="pn-notice-body">
          <p class="pn-notice-title">Installing writes slot {target?.name ?? "?"}</p>
          <p>
            {u.bootedVersion} stays in slot {booted?.name} as the fallback. If the new version doesn't come up after the
            restart, the satellite goes back to it on its own.
          </p>
        </div>
      </div>
      <form
        class="pn-field"
        onsubmit={(e) => {
          e.preventDefault();
          void install();
        }}
      >
        <label class="pn-field-label" for="manifest">Release manifest</label>
        <input
          id="manifest"
          class="pn-input is-mono"
          type="url"
          inputmode="url"
          autocomplete="url"
          spellcheck="false"
          placeholder="http://192.168.1.20:8000/manifest.json"
          bind:value={manifest}
          disabled={phase === "installing"}
          aria-describedby="manifest-hint"
          aria-invalid={urlError ? "true" : undefined}
          oninput={() => (urlError = null)}
        />
        {#if urlError}<span class="pn-field-error" role="alert"><Icon name="alert" />{urlError}</span>{/if}
        <span id="manifest-hint" class="pn-field-hint">
          Build a release with <code class="pn-code">just release</code>, then serve it from your computer with
          <code class="pn-code">python3 -m http.server 8000 --directory build/release</code>.
        </span>
        <div>
          <button
            class="pn-btn pn-btn-primary"
            type="submit"
            disabled={!manifest.trim() || !live || starting || phase === "installing"}
          >
            {phase === "installing" ? "Installing…" : "Install update"}
          </button>
        </div>
      </form>
    </section>
  {/if}

  {#if lines.length || result}
    <figure class="pn-cmd">
      <figcaption class="pn-cmd-head"><span class="pn-cmd-where">On the satellite</span></figcaption>
      <pre aria-live="polite">{lines.join("\n")}</pre>
    </figure>
    {#if result?.ok && phase === "pending"}
      <!-- the pending notice above says what to do next -->
    {:else if result && !result.ok}
      <p class="pn-field-error" role="alert">
        <Icon name="alert" />The update was not installed: {u.lastInstall?.lastLine || "see the steps above"}. The
        satellite keeps running {u.bootedVersion}.
      </p>
    {/if}
  {/if}

  {#if shownError}
    <p class="pn-field-error" role="alert">
      <Icon name="alert" />{shownError.charAt(0).toUpperCase() + shownError.slice(1)}.
    </p>
  {/if}

  <section class="panel" aria-labelledby="restart-title">
    <h2 id="restart-title" class="pn-type-heading">Restart</h2>
    <p class="muted">
      Restarting takes a minute or two. This page reconnects on its own.
      {#if phase === "trial"}This version is on trial: a restart now uses one of its tries.{/if}
    </p>
    <div>
      <button
        class="pn-btn"
        disabled={!live || device.busy || device.restarting || phase === "installing"}
        onclick={restart}
      >
        {device.restarting ? "Restarting…" : "Restart the satellite"}
      </button>
    </div>
    {#if restartError}<p class="pn-field-error" role="alert"><Icon name="alert" />{restartError}</p>{/if}
  </section>
{/if}

<style>
  p {
    margin: 0;
  }
  .muted {
    color: var(--ink-muted);
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: var(--space-2) var(--space-3) var(--space-2) 0;
    border-top: 1px solid var(--line);
  }
  th {
    font-size: 13px;
    font-weight: 400;
    color: var(--ink-muted);
    border-top: 0;
  }
  .mono {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  td[data-tone="trial"] {
    color: var(--caution);
    font-weight: 650;
  }
  td[data-tone="unbootable"] {
    color: var(--danger);
    font-weight: 650;
  }
  .pn-cmd pre {
    max-height: 320px;
  }
  form {
    max-width: 640px;
  }
</style>
