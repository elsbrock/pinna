<!-- Updates: the update channel and the release its check found (ADR 0022),
     the A/B slots, and installing. pinnad runs the install, so it survives a
     closed tab; this page follows it with WatchInstall and shows its stages as
     steps. A manifest URL is the way for satellites without internet access. -->
<script lang="ts">
  import { ConnectError } from "@connectrpc/connect";
  import { tick } from "svelte";

  import type { GetUpdatesResponse } from "../gen/pinna/v1/device_pb.ts";
  import Confirm from "./Confirm.svelte";
  import { client, device } from "./device.svelte.ts";
  import External from "./External.svelte";
  import Group from "./Group.svelte";
  import Icon from "./Icon.svelte";
  import Select from "./Select.svelte";
  import SwitchRow from "./SwitchRow.svelte";
  import { toasts } from "./toast.svelte.ts";
  import { windows } from "./windows.svelte.ts";
  import {
    ACTION,
    CHANNEL,
    type ChannelName,
    channelName,
    checkStatus,
    DOCS,
    downloadProgress,
    INSTALL,
    type InstallName,
    installName,
    installSteps,
    OLDER_SLOT_WARNING,
    olderSlotWarning,
    slotWord,
    switchTarget,
    targetSlot,
    updatePhase,
    windowLength,
  } from "./view.ts";

  const STORE = "pinna.manifest";
  const REFRESH_MS = 10_000;

  let u = $state<GetUpdatesResponse | null>(null);
  let error = $state<string | null>(null);
  // Someone installed from a manifest URL here before: keep that way open.
  let manualOpen = $state(read() !== "");
  let manifest = $state(read());
  // The suggested address goes in once; after that the field is the owner's.
  let suggested = false;
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
  let checking = $state(false);
  let checkError = $state<string | null>(null);
  let confirmingOlder = $state(false);
  let switchButton = $state<HTMLButtonElement>();
  let startInput = $state<HTMLInputElement>();
  let endInput = $state<HTMLInputElement>();
  let windowError = $state<string | null>(null);
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
    if (!suggested && !manifest && u.suggestedManifest) manifest = u.suggestedManifest;
    suggested = true;
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

  // The release the last check found, by the version this page shows (ADR 0022).
  async function installRelease(older = false) {
    const r = u?.release;
    if (!r) return;
    starting = true;
    lines = [];
    stage = 0;
    progress = null;
    result = null;
    error = null;
    try {
      await client.installUpdate({ version: r.version, older });
      await refresh();
      if (r.action === ACTION.start) toasts.show(`${r.version} starts on the next restart.`, "ok");
      // The steps sit below the release: bring them into view.
      await tick();
      document.querySelector(".pn-steps")?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    } catch (e) {
      error = ConnectError.from(e).rawMessage;
    } finally {
      starting = false;
    }
  }

  async function checkNow() {
    checking = true;
    checkError = null;
    try {
      // pinnad answers when the check has finished.
      await client.checkForUpdates({}, { timeoutMs: 200_000 });
    } catch (e) {
      checkError = ConnectError.from(e).rawMessage;
    } finally {
      checking = false;
      await refresh();
    }
  }

  async function setChannel(id: string) {
    const name = id as ChannelName;
    const failed = await device.saveSettings(
      { updateChannel: CHANNEL[name] },
      name === "off" ? "Updates off" : `Updates from ${name}`,
    );
    if (failed) toasts.show(failed, "danger");
    // A new channel is checked at once.
    else await refresh();
  }

  async function setInstall(id: string) {
    const name = id as InstallName;
    const failed = await device.saveSettings(
      { installUpdates: INSTALL[name] },
      name === "automatically" ? "Updates install automatically" : "Updates install when you ask",
    );
    if (failed) toasts.show(failed, "danger");
  }

  async function setStatistics(on: boolean) {
    const failed = await device.saveSettings(
      { includeInStatistics: on },
      on ? "Included in the statistics" : "Left out of the statistics",
    );
    if (failed) toasts.show(failed, "danger");
  }

  // Both times go together: pinnad checks the window as a whole, so a swap works.
  async function setWindow() {
    if (!startInput || !endInput) return;
    if (!startInput.value || !endInput.value) {
      windowError = "The window needs a start time and an end time.";
      return putBackWindow();
    }
    windowError = await device.saveSettings(
      { installWindowStart: startInput.value, installWindowEnd: endInput.value },
      "Install window saved",
    );
    if (windowError) putBackWindow();
  }

  // The fields show what the satellite has saved, not what it refused.
  function putBackWindow() {
    if (startInput) startInput.value = savedStart;
    if (endInput) endInput.value = savedEnd;
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
  // While pinnad checks on its own (a new channel), look again soon.
  $effect(() => {
    if (!u?.check?.checking) return;
    const timer = setTimeout(() => void refresh(), 2_000);
    return () => clearTimeout(timer);
  });
  // Whether a password is set, for the rollback warning (ADR 0029).
  $effect(() => windows.watch());
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
  const channel = $derived(channelName(device.status?.settings?.updateChannel));
  const off = $derived(!u?.manifestOverride && channel === "off");
  const installMode = $derived(installName(device.status?.settings?.installUpdates));
  const zone = $derived(device.status?.settings?.timeZone || "UTC");
  const zoneSet = $derived(!!device.status?.settings?.timeZone);
  const savedStart = $derived(device.status?.settings?.installWindowStart ?? "03:00");
  const savedEnd = $derived(device.status?.settings?.installWindowEnd ?? "05:00");
  const windowSpan = $derived(windowLength(savedStart, savedEnd));
  const channelLabel = $derived(channel === "beta" ? "Beta" : "Stable");
  const release = $derived(u?.release);
  // Going back: start the other slot when it holds that version, else install it.
  const olderInSlot = $derived(
    release && other?.version === release.version && other.state !== "unbootable" ? other : undefined,
  );
  // An image from before the password doesn't check it: say so before starting an older one (ADR 0029).
  const switchWarning = $derived(olderSlotWarning(windows.passwordSet, other));
  const pendingWarning = $derived(olderSlotWarning(windows.passwordSet, next));
  const olderRelease = $derived(
    windows.passwordSet && release?.action === ACTION.older ? OLDER_SLOT_WARNING : null,
  );
  const INSTALL_CHOICES = [
    { id: "manually", label: "Manually" },
    { id: "automatically", label: "Automatically" },
  ];
  const CHANNELS = [
    { id: "stable", label: "Stable" },
    { id: "beta", label: "Beta" },
    { id: "off", label: "Off" },
  ];
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
  <Group title="Updates">
    {#if u.manifestOverride}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Updates</span>
          <span class="pn-row-hint">
            From <code class="pn-code">[update] manifest</code> in pinnad.toml, instead of a channel.
          </span>
        </div>
        <span class="pn-row-value is-mono url">{u.manifestOverride}</span>
      </div>
    {:else}
      <Select
        id="update-channel"
        label="Updates"
        hint={u.developmentBuild
          ? "This development build installs only images signed with your own key: releases from the channels don't install on it."
          : "Stable has finished releases. Beta also has releases before they are finished. The satellite checks updates.getpinna.dev once a day."}
        value={channel}
        options={CHANNELS}
        disabled={!live || device.busy}
        choose={setChannel}
      />
      <Select
        id="install-updates"
        label="Install updates"
        hint="Manually: the satellite installs a release when you ask. Automatically: it installs a new release at a random time in the window below and restarts when it is idle, in the window. A release that needs a new boot image never installs by itself.{channel === 'off' ? ' Updates is off, so nothing installs by itself.' : ''}"
        value={installMode}
        options={INSTALL_CHOICES}
        disabled={!live || device.busy}
        choose={setInstall}
      />
      {#if installMode === "automatically"}
        <div class="pn-row">
          <div class="pn-row-text">
            <span id="install-window-label" class="pn-row-label">Install window</span>
            <span id="install-window-hint" class="pn-row-hint">
              {#if zoneSet}
                Local time, in the time zone {zone}.
              {:else}
                No time zone is set, so the window is in UTC.
                <a class="pn-btn pn-btn-quiet pn-btn-sm" href="#/system">Set the time zone</a>
              {/if}
              The window may cross midnight, for example 23:00 to 01:00.
            </span>
          </div>
          <div class="times" role="group" aria-labelledby="install-window-label" aria-describedby="install-window-hint">
            <input
              bind:this={startInput}
              id="install-window-start"
              class="pn-input is-mono time"
              type="time"
              step="60"
              aria-label="Window start"
              value={savedStart}
              disabled={!live || device.busy}
              onchange={setWindow}
            />
            <span aria-hidden="true">to</span>
            <input
              bind:this={endInput}
              id="install-window-end"
              class="pn-input is-mono time"
              type="time"
              step="60"
              aria-label="Window end"
              value={savedEnd}
              disabled={!live || device.busy}
              onchange={setWindow}
            />
            {#if windowSpan}<span class="span">{windowSpan}</span>{/if}
          </div>
        </div>
        {#if windowError}
          <div class="pn-row">
            <p class="pn-field-error" role="alert"><Icon name="alert" />{windowError}</p>
          </div>
        {/if}
      {/if}
      <SwitchRow
        id="include-in-statistics"
        label="Include in the statistics"
        checked={device.status?.settings?.includeInStatistics ?? true}
        disabled={!live}
        busy={device.busy}
        hint="Once a day the update check also names this satellite's hardware, so that getpinna.dev/statistics counts satellites by hardware, version, channel and country. The counts hold no address or identifier.{off ? ' With Updates off, nothing is sent.' : ''}"
        toggle={setStatistics}
      />
    {/if}
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Check for updates</span>
        <span class="pn-row-hint">{checkStatus(u.check, off, new Date(device.now))}</span>
      </div>
      <button
        class="pn-btn"
        disabled={!live || off || checking || !!u.check?.checking || running}
        onclick={checkNow}
      >
        <Icon name="restart" />{checking || u.check?.checking ? "Checking…" : "Check now"}
      </button>
    </div>
    {#if (checkError || u.check?.error) && !off}
      <div class="pn-row">
        <p class="pn-field-error" role="alert"><Icon name="alert" />{checkError ?? u.check?.error}</p>
      </div>
    {/if}

    {#if release && !off && release.action !== ACTION.restart}
      <div class="pn-row is-stack">
        <div class="release">
          <div class="pn-row-text">
            <span class="pn-row-label">
              {#if release.action === ACTION.upToDate}
                Up to date
              {:else if release.action === ACTION.failed}
                Pinna {release.version} failed to start
              {:else if release.action === ACTION.otherModel}
                Pinna {release.version} is for another model
              {:else}
                Pinna {release.version}
              {/if}
            </span>
            {#if release.action === ACTION.upToDate}
              <span class="pn-row-hint">
                Pinna {release.version} is the newest release {u.manifestOverride ? "at that address" : `on the ${channelLabel} channel`}.
              </span>
            {:else if release.action === ACTION.otherModel}
              <span class="pn-row-hint">{release.note} It isn't offered on this satellite.</span>
            {:else if release.action === ACTION.failed}
              <span class="pn-row-hint is-danger">
                {release.note} The satellite runs {u.bootedVersion}. A newer release is offered when there is one.
              </span>
            {:else if release.action === ACTION.older}
              <span class="pn-row-hint">
                Older than the running {u.bootedVersion}. The satellite never goes back on its own; you can.
              </span>
            {:else if release.summary}
              <span class="pn-row-hint">{release.summary}</span>
            {/if}
            {#if release.notesUrl && release.action !== ACTION.upToDate && release.action !== ACTION.otherModel}
              <span class="pn-row-hint"><External href={release.notesUrl}>Release notes</External></span>
            {/if}
          </div>
          {#if release.action === ACTION.install}
            <button class="pn-btn pn-btn-primary" disabled={!live || running || device.busy} onclick={() => installRelease()}>
              <Icon name="download" />Install
            </button>
          {:else if release.action === ACTION.start}
            <button class="pn-btn pn-btn-primary" disabled={!live || running || device.busy} onclick={() => installRelease()}>
              Start on next restart
            </button>
          {:else if release.action === ACTION.older && phase === "ready"}
            <button
              class="pn-btn"
              disabled={!live || running || device.busy}
              onclick={() => {
                switchError = null;
                if (olderInSlot) confirmingSwitch = true;
                else confirmingOlder = true;
              }}
            >
              {olderInSlot ? `Start ${release.version}…` : `Install ${release.version} (older)…`}
            </button>
          {/if}
        </div>
        {#if release.action === ACTION.blocked}
          {#if !u.developmentOverlay && phase !== "trial"}
            <p class="pn-field-hint is-caution note"><Icon name="alert" />{release.note}</p>
          {/if}
        {:else if release.action === ACTION.install}
          <p class="pn-field-hint">
            Installs into slot {target?.name.toUpperCase() ?? "?"} and starts after a restart. {u.bootedVersion} stays in
            slot {booted?.name.toUpperCase()}, and the satellite goes back to it if the new version doesn't come up.
          </p>
        {:else if release.action === ACTION.start}
          <p class="pn-field-hint">
            It is in slot {target?.name.toUpperCase() ?? "?"} already, from an update you held back. It starts on the next
            restart.
          </p>
        {/if}
      </div>
    {:else if u.check?.noRelease && !off}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">No release yet</span>
          <span class="pn-row-hint">
            {u.manifestOverride
              ? "Nothing answers at that address. Check that the release folder is served there."
              : `There is no release on the ${channelLabel} channel yet.`}
          </span>
        </div>
      </div>
    {/if}
  </Group>

  {#if u.developmentOverlay}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">A development overlay is active</p>
        <p>
          Programs from <code class="pn-code">just push</code> are in /var/lib/extensions/pinna-dev. They would hide an
          update's programs, so installing waits until they are gone. Remove them with
          <code class="pn-code">scripts/push-dev.sh --remove</code>, on your computer.
        </p>
      </div>
    </div>
  {/if}

  {#if release && release.action === ACTION.older && !olderInSlot}
    <Confirm
      bind:open={confirmingOlder}
      title="Go back to {release.version}?"
      action="Install {release.version}"
      confirm={() => installRelease(true)}
    >
      <p>
        The satellite installs {release.version} into slot {target?.name.toUpperCase() ?? "?"}, which replaces what is
        there. It starts after a restart, and {u.bootedVersion} stays in slot {booted?.name.toUpperCase()} as the
        fallback.
      </p>
      <p>Your settings stay. Settings that only {u.bootedVersion} knows come back when you update again.</p>
      {#if olderRelease}<p class="is-caution">{olderRelease}</p>{/if}
    </Confirm>
  {/if}

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
    {#if other && release?.action !== ACTION.start}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Switch to slot {other.name.toUpperCase()}</span>
          <span class="pn-row-hint">Restart into {other.version}, on trial.</span>
          {#if switchWarning}<span class="pn-row-hint is-caution">{switchWarning}</span>{/if}
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
      {#if switchWarning}<p class="is-caution">{switchWarning}</p>{/if}
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
        {#if pendingWarning}<p>{pendingWarning}</p>{/if}
        <p class="actions">{@render pendingActions()}</p>
        {#if restartError}<p class="pn-field-error" role="alert"><Icon name="alert" />{restartError}</p>{/if}
      </div>
    </div>
  {/if}

  {#if phase === "ready" || phase === "installing" || lines.length || result}
    <Group title={running ? "Installing" : lines.length || result ? "Last install" : "Other releases"}>
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
              <Icon name="alert" />The update was not installed: {(u.lastInstall?.lastLine || "see the output").replace(/\.+$/, "")}. The
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
        <details class="manual" bind:open={manualOpen}>
          <summary>Install from a manifest URL</summary>
        <form
          novalidate
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
              For a satellite without internet access, or a build of your own. Copy a release folder to a computer on
              your network and serve it, for example with
              <code class="pn-code">python3 -m http.server 8000 --directory pinna-0.4.0</code> on that computer, with the
              release's folder in place of <code class="pn-code">pinna-0.4.0</code>. The
              satellite installs it when its signature checks out, an older version too. Installing writes slot
              {target?.name.toUpperCase() ?? "?"}; {u.bootedVersion} stays in slot {booted?.name.toUpperCase()} as the
              fallback.
            </span>
          </div>
        </form>
        </details>
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
  .release {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2) var(--space-3);
  }
  .url {
    overflow-wrap: anywhere;
    max-width: 50%;
  }
  .times {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .time {
    width: auto;
  }
  .span {
    color: var(--ink-muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .note {
    display: flex;
    gap: var(--space-2);
  }
  .manual summary {
    padding: var(--space-3) var(--space-4);
    cursor: pointer;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-muted);
  }
  .manual summary:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--focus);
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
