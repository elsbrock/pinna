<script lang="ts">
  import Confirm from "../lib/Confirm.svelte";
  import { onMount } from "svelte";
  import { client, device, errorSentence } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import type { GetUpdatesResponse } from "../gen/pinna/v1/device_pb.ts";
  import { security } from "../lib/security.svelte.ts";
  import Select from "../lib/Select.svelte";
  import { toasts } from "../lib/toast.svelte.ts";
  import { displayName, lastStart, recordHint, updateSummary, uptime } from "../lib/view.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const up = $derived(s ? Number(s.uptimeSeconds) + Math.floor((device.now - device.receivedAt) / 1000) : 0);
  let confirming = $state(false);
  let resetting = $state(false);
  let typed = $state("");
  const resetName = $derived(s ? displayName(s) : "");
  const typedRight = $derived(
    !!typed.trim() && [resetName, s?.name ?? ""].some((n) => n && typed.trim().toLowerCase() === n.toLowerCase()),
  );

  async function factoryReset() {
    const failed = await device.factoryReset(typed);
    typed = "";
    if (failed) toasts.show(failed, "danger");
  }

  // The name field follows the satellite until someone types in it.
  let name = $state("");
  let editing = $state(false);
  let nameError = $state<string | null>(null);
  const current = $derived(s?.settings?.friendlyName ?? "");
  $effect(() => {
    if (!editing) name = current;
  });

  async function rename() {
    nameError = await device.saveSettings({ friendlyName: name }, "Renamed");
    if (!nameError) editing = false;
  }

  // The time zone (ADR 0007), from the zones the satellite has.
  let zones = $state<string[]>([]);
  let zonesError = $state<string | null>(null);
  let zoneError = $state<string | null>(null);
  const zone = $derived(s?.settings?.timeZone || "UTC");
  const browserZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const zoneOptions = $derived((zones.length ? zones : [zone]).map((z) => ({ id: z, label: z })));
  onMount(async () => {
    try {
      zones = await device.listTimeZones();
    } catch (e) {
      zonesError = errorSentence(e);
    }
  });
  // A failed save redraws the select, so it shows the zone in use again.
  let zoneRedraw = $state(0);
  async function setZone(next: string) {
    zoneError = await device.saveSettings({ timeZone: next }, `Time zone set to ${next}`);
    if (zoneError) zoneRedraw++;
  }

  // The Access and Software rows summarize their pages (ADR 0029, ADR 0022).
  onMount(() => {
    void security.refresh();
  });
  const passwordSet = $derived(security.state?.passwordSet);
  const record = $derived(recordHint(passwordSet, !!s?.recording));

  // GetUpdates once on mount and when the link comes back live; every 5 s while
  // an install runs, so the row turns from "Installing" without a reload. A
  // failed read counts as no answer.
  let updates = $state.raw<GetUpdatesResponse | null>(null);
  async function readUpdates() {
    try {
      updates = await client.getUpdates({}, { timeoutMs: 10_000 });
    } catch {
      updates = null;
    }
  }
  onMount(() => {
    void readUpdates();
  });
  $effect(() => {
    if (!updates?.installing) return;
    const timer = setInterval(() => void readUpdates(), 5_000);
    return () => clearInterval(timer);
  });
  let wasLive = device.link === "live";
  $effect(() => {
    if (live && !wasLive) void readUpdates();
    wasLive = live;
  });
  const summary = $derived(updateSummary(updates, s?.version ?? "", s?.settings));
  const start = $derived(lastStart(s?.lastBoot));

  async function restart() {
    const failed = await device.restart();
    if (failed) toasts.show(failed, "danger");
  }
</script>

{#if s}
  <Group title="Name and time zone">
    {#snippet foot()}
      Home Assistant takes a new name the next time it connects, once the satellite is idle. Its entity IDs keep the
      name the satellite had when you added it.
    {/snippet}
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void rename();
      }}
    >
      <div class="pn-field" data-invalid={nameError ? "" : undefined}>
        <label class="pn-field-label" for="friendly-name">Name</label>
        <div class="inline">
          <input
            id="friendly-name"
            class="pn-input"
            autocomplete="off"
            spellcheck="false"
            bind:value={name}
            disabled={!live}
            aria-describedby={s.settings?.defaultName ? "friendly-name-hint" : undefined}
            aria-invalid={nameError ? "true" : undefined}
            oninput={() => {
              editing = true;
              nameError = null;
            }}
          />
          <button
            class="pn-btn pn-btn-primary"
            type="submit"
            disabled={!live || device.busy || !name.trim() || name.trim() === current}
          >
            Save
          </button>
        </div>
        {#if nameError}<span class="pn-field-error" role="alert"><Icon name="alert" />{nameError}</span>{/if}
        {#if s.settings?.defaultName}
          <span id="friendly-name-hint" class="pn-field-hint">
            Name it before you add it to Home Assistant.
          </span>
        {/if}
      </div>
    </form>
    {#key zoneRedraw}
      <Select
        id="time-zone"
        label="Time zone"
        hint="For the satellite's own clock and logs. UTC until you choose one."
        value={zone}
        options={zoneOptions}
        disabled={!live || device.busy || !zones.length}
        choose={setZone}
      />
    {/key}
    {#if zonesError || zoneError}
      <div class="pn-row">
        <span class="pn-field-error" role="alert"><Icon name="alert" />{zonesError ?? zoneError}</span>
      </div>
    {/if}
    {#if zones.includes(browserZone) && browserZone !== zone}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-hint">This browser is in {browserZone}.</span>
        </div>
        <button class="pn-btn pn-btn-sm" disabled={!live || device.busy} onclick={() => setZone(browserZone)}>
          Use {browserZone}
        </button>
      </div>
    {/if}
  </Group>

  <Group title="Access">
    <a class="pn-row" href="#/system/password">
      <span class="pn-row-text">
        <span class="pn-row-label">Password and tokens</span>
        {#if passwordSet === undefined}
          <span class="pn-row-hint">Who can use the dashboard, and tokens for Prometheus and agents</span>
        {:else if passwordSet}
          <span class="pn-row-hint">Signing in, and tokens for Prometheus and agents</span>
        {:else}
          <span class="pn-row-hint is-caution">Anyone on your network can control this satellite.</span>
        {/if}
      </span>
      {#if passwordSet !== undefined}<span class="pn-row-value">{passwordSet ? "Password set" : "No password"}</span>{/if}
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <Group title="Software">
    <a class="pn-row" href="#/system/updates">
      <span class="pn-row-text">
        <span class="pn-row-label">Updates</span>
        <span
          class="pn-row-hint"
          class:is-danger={summary.tone === "danger"}
          class:is-caution={summary.tone === "caution"}>{summary.hint}</span
        >
      </span>
      <span class="pn-row-value is-mono">{summary.value}</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
    <a class="pn-row" href="#/system/record">
      <span class="pn-row-text">
        <span class="pn-row-label">Record audio</span>
        <span class="pn-row-hint" class:is-caution={record.caution}>{record.hint}</span>
      </span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
    <a class="pn-row" href="#/system/logs">
      <span class="pn-row-text">
        <span class="pn-row-label">Logs</span>
        <span class="pn-row-hint" class:is-danger={start.danger}>{start.hint}</span>
      </span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <Group title="Power">
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Restart</span>
        <span class="pn-row-hint">The satellite stops listening for a minute or two.</span>
      </div>
      <button
        class="pn-btn"
        disabled={!live || device.busy || device.restarting}
        onclick={() => (confirming = true)}
      >
        <Icon name="restart" />Restart…
      </button>
    </div>
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Factory reset</span>
        <span class="pn-row-hint">
          Forget everything set on this satellite, and restart it as it was installed.
        </span>
      </div>
      <button
        class="pn-btn pn-btn-danger"
        disabled={!live || device.busy || device.restarting}
        onclick={() => {
          typed = "";
          resetting = true;
        }}
      >
        Reset…
      </button>
    </div>
  </Group>

  <Group title="Details">
    {#each [["Node name", s.name], ["MAC", s.mac.toLowerCase()], ["Uptime", live ? uptime(up) : "—"], ["Audio backend", s.audioBackend], ["LED backend", s.ledBackend]] as [label, value] (label)}
      <div class="pn-row">
        <div class="pn-row-text"><span class="pn-row-label">{label}</span></div>
        <span class="pn-row-value is-mono">{value}</span>
      </div>
    {/each}
  </Group>

  <Confirm
    bind:open={resetting}
    title="Reset {resetName}?"
    action="Reset"
    danger
    ready={typedRight}
    confirm={factoryReset}
  >
    <p>
      The satellite forgets its name, its Wi-Fi networks{s.apiKeyFromImage ? "" : ", its Home Assistant encryption key"}
      and every setting made here or in Home Assistant, then restarts as it was installed. Its log stays{s.apiKeyFromImage
        ? ", and so does the encryption key its image carries"
        : ""}.
    </p>
    <p>
      If its image doesn't set up Wi-Fi, it is then reachable over USB only, at
      <code class="pn-code">http://10.42.0.1/</code>.
      {#if !s.apiKeyFromImage}Home Assistant shows it as unavailable until you add it again.{/if}
    </p>
    <div class="pn-field">
      <label class="pn-field-label" for="reset-name">Type <strong>{resetName}</strong> to confirm</label>
      <input id="reset-name" class="pn-input" autocomplete="off" spellcheck="false" bind:value={typed} />
    </div>
  </Confirm>

  <Confirm bind:open={confirming} title="Restart {displayName(s)}?" action="Restart" confirm={restart}>
    <p>It stops listening and goes dark for a minute or two. This page reconnects on its own.</p>
  </Confirm>
{/if}

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .inline {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .inline input {
    flex: 1 1 220px;
    width: auto;
  }
</style>
