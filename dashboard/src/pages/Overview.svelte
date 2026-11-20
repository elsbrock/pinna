<script lang="ts">
  import { drawingFor } from "../../../design/devices/index.ts";
  import type { GetUpdatesResponse } from "../gen/pinna/v1/device_pb.ts";
  import { brainLabel, failureText, isStandalone } from "../lib/assistant.ts";
  import Controls from "../lib/Controls.svelte";
  import Device from "../lib/Device.svelte";
  import { client, device, errorSentence } from "../lib/device.svelte.ts";
  import Icon from "../lib/Icon.svelte";
  import { network } from "../lib/network.svelte.ts";
  import PhaseBadge from "../lib/PhaseBadge.svelte";
  import { recorder } from "../lib/recorder.svelte.ts";
  import { security } from "../lib/security.svelte.ts";
  import SignalBars from "../lib/SignalBars.svelte";
  import StatTile from "../lib/StatTile.svelte";
  import NowPlaying from "../lib/NowPlaying.svelte";
  import Timers from "../lib/Timers.svelte";
  import { toasts } from "../lib/toast.svelte.ts";
  import VirtualDevice from "../lib/VirtualDevice.svelte";
  import {
    badge,
    band,
    confirmationWarning,
    displayName,
    health,
    heldText,
    homeAssistantBlocked,
    openNotice,
    recordingWarning,
    ringEffect,
    secondsUntil,
    signalWord,
    since,
    slotWord,
    uptime,
    windowLeft,
    wifiConnected,
  } from "../lib/view.ts";
  import { windows } from "../lib/windows.svelte.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const ring = $derived(ringEffect(s, device.link, device.testStep));
  const line = $derived(health(s, device.link, device.offlineSince, device.restarting));
  const modelName = $derived(drawingFor(s?.model ?? "")?.name);
  const standalone = $derived(isStandalone(s));
  // While no password is set, anyone on the network can control the satellite (ADR 0029).
  const notice = $derived(openNotice(security.state, standalone));
  // A confirmation another browser started waits for a press of Action (ADR 0029): the
  // owner who didn't start it can stop it. Asked often enough to notice while it waits.
  const WATCH_MS = 5_000;
  $effect(() => {
    const timer = setInterval(() => void security.refresh(), WATCH_MS);
    return () => clearInterval(timer);
  });
  const waiting = $derived(confirmationWarning(security.state, security.confirmingHere));
  const waitingLeft = $derived(waiting ? secondsUntil(security.receivedAt + waiting.left * 1000, device.now) : 0);
  let stopping = $state(false);
  async function stopIt() {
    stopping = true;
    const failed = await security.stopConfirmation();
    stopping = false;
    if (failed) toasts.show(failed, "danger");
  }
  // A recording another dashboard runs (ADR 0030); the one that records says so on its own page.
  const recording = $derived(recordingWarning(s, security.state, recorder.here));
  const recordingLeft = $derived(recording ? secondsUntil(device.receivedAt + recording.left * 1000, device.now) : 0);
  let stoppingRecording = $state(false);
  async function stopRecording() {
    stoppingRecording = true;
    try {
      await client.stopRecording({});
    } catch (e) {
      toasts.show(errorSentence(e), "danger");
    }
    stoppingRecording = false;
  }
  // With a password and no key, the satellite refuses Home Assistant until the owner opens a window.
  $effect(() => windows.watch());
  const blocked = $derived(homeAssistantBlocked(security.state, s));
  const haLeft = $derived(windows.homeAssistantLeft);
  // Messages from the satellite group that wait to be said (ADR 0026).
  const held = $derived(heldText(s?.heldMessages ?? 0, !!s?.muted));
  // Uptime only arrives with a change; count on between messages.
  const up = $derived(s ? Number(s.uptimeSeconds) + Math.floor((device.now - device.receivedAt) / 1000) : 0);

  // Narrow layouts get a smaller drawing, so Volume and Microphones stay near the top.
  const phone = matchMedia("(max-width: 999px)");
  let small = $state(phone.matches);
  $effect(() => {
    const update = () => (small = phone.matches);
    phone.addEventListener("change", update);
    return () => phone.removeEventListener("change", update);
  });

  $effect(() => network.watch());
  const wifi = $derived(network.wireless);
  const address = $derived(network.ipv4 ?? location.hostname);

  // The running slot, for the Software tile; read once.
  let updates = $state<GetUpdatesResponse | null>(null);
  $effect(() => {
    client.getUpdates({}, { timeoutMs: 10_000 }).then(
      (u) => (updates = u),
      () => {},
    );
  });
  const booted = $derived(updates?.slots.find((x) => x.booted));

  // After Clear, the notice stays in place for a moment, so a second tap
  // doesn't land on the control that would move up under it.
  let cleared = $state(false);
  async function clearNotification() {
    await device.clearNotification();
    if (device.status?.notification) return;
    cleared = true;
    setTimeout(() => (cleared = false), 2_000);
  }
</script>

{#if s}
  <section class="hero">
    <Device
      model={s.model}
      effect={ring}
      frame={device.ring}
      width={small ? 200 : 250}
      label="Ring: {ring.replace('-', ' ')}"
      halo
    />
    <div class="who">
      {#if modelName}<span class="pn-eyebrow">{modelName}</span>{/if}
      <h1 class="pn-type-display-l">{displayName(s)}</h1>
      <div class="state">
        {#if live}<PhaseBadge badge={badge(s)} />{/if}
        <span class="pn-health" data-tone={line.tone}>{line.text}</span>
        {#if live && held}
          <span class="held">{held}</span>
        {/if}
      </div>
      <span class="meta">{address} · Pinna {s.version}</span>
      <div>
        <button
          class="pn-btn pn-btn-sm"
          disabled={!live}
          title="Run the ring's test pattern, to find this satellite"
          onclick={() => device.identify()}
        >
          <Icon name="sun" />Identify
        </button>
      </div>
    </div>
  </section>

  {#if waiting && waitingLeft > 0}
    <div class="pn-notice" data-tone="caution" role="alert">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">
          {#if waiting.recording}
            A browser at {waiting.from} is waiting for a press of Action to record from the microphones, {waitingLeft}
            s left. Don't press it unless that was you.
          {:else}
            A browser at {waiting.from} is waiting for a press of Action, {waitingLeft} s left. Don't press it unless
            that was you.
          {/if}
        </p>
        <div>
          <button class="pn-btn" disabled={stopping || !live} onclick={stopIt}>Stop it</button>
        </div>
      </div>
    </div>
  {/if}

  {#if recording && recordingLeft > 0}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">
          A browser at {recording.from} is recording from the microphones: {recordingLeft} s left.
        </p>
        <div>
          <button class="pn-btn" disabled={stoppingRecording || !live} onclick={stopRecording}>Stop it</button>
        </div>
      </div>
    </div>
  {/if}

  {#if notice}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">{notice.join(" ")}</p>
        <p><a href="#/system/password">Set a password under System › Password and tokens</a></p>
      </div>
    </div>
  {/if}

  {#if live && !s.haConnected && !standalone}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="info" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">Add this satellite to Home Assistant</p>
        {#if s.settings?.defaultName}
          <p>
            First <a href="#/system">give it a name</a>: Home Assistant's entity IDs keep the name it has when you add
            it.
          </p>
        {/if}
        {#if blocked}
          {#if haLeft > 0}
            <p>Home Assistant can add this satellite now, {windowLeft(haLeft)}. Add it in Home Assistant.</p>
          {:else}
            <p>
              Home Assistant can't add this satellite until you let it: it has a dashboard password and no key from
              Home Assistant yet, so it refuses Home Assistant's connections.
            </p>
            <div>
              <button class="pn-btn" disabled={windows.opening} onclick={() => windows.openHomeAssistant()}>
                Let Home Assistant add this satellite
              </button>
            </div>
          {/if}
          {#if windows.error}<p class="pn-field-error" role="alert"><Icon name="alert" />{windows.error}</p>{/if}
        {/if}
        <p>
          Home Assistant usually finds the satellite on its own under Settings › Devices &amp; services.
          <a href="#/assistant">How to add it by hand</a>
        </p>
        <p>
          {#if s.assistant?.ready}
            Without Home Assistant, the satellite can answer by itself: its services are ready to
            <a href="#/assistant">switch to standalone mode</a>.
          {:else}
            Without Home Assistant, the satellite can answer by itself with services you choose:
            <a href="#/assistant/services">set up standalone mode</a>.
          {/if}
        </p>
      </div>
    </div>
  {/if}

  {#if s.notification || cleared}
    <div class="pn-notice" role="status">
      <Icon name="info" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">{cleared ? "Notification cleared" : "A notification is waiting"}</p>
        <p>
          {standalone ? "" : "Home Assistant has a notification for you. "}The ring pulses yellow when nothing else is
          showing. Clear it here, or press the Action button while the ring is yellow.
        </p>
        <div>
          <button
            class="pn-btn"
            disabled={cleared || device.link !== "live" || device.busy}
            onclick={clearNotification}
          >
            {cleared ? "Cleared" : "Clear"}
          </button>
        </div>
      </div>
    </div>
  {/if}

  <div class="stats">
    {#if standalone}
      <!-- Standalone mode: the brain that answers, and how its last failure went. -->
      <StatTile
        href="#/assistant"
        icon="assistant"
        label="Standalone"
        value={!live ? "—" : brainLabel(s.assistant?.label ?? "") || "No brain yet"}
      >
        {#if !live}
          Not reachable
        {:else if !s.assistant?.ready}
          <span class="pn-health" data-tone="caution">Services not ready</span>
        {:else if s.assistant.lastError}
          <span class="pn-health" data-tone="danger" title={failureText(s.assistant)}>Last request failed</span>
        {:else}
          <span class="pn-health" data-tone="ok">Ready</span>
        {/if}
      </StatTile>
    {:else}
      <StatTile
        href="#/assistant"
        icon="house"
        label="Home Assistant"
        value={!live ? "—" : s.haConnected ? "Connected" : "Not connected"}
      >
        {#if !live}
          Not reachable
        {:else if !s.haConnected}
          How to add it
        {:else if s.voiceSubscribed}
          <span class="pn-health" data-tone="ok">Voice pipeline ready</span>
        {:else}
          <span class="pn-health" data-tone="caution">No voice pipeline</span>
        {/if}
      </StatTile>
    {/if}
    <StatTile
      href="#/network"
      icon="wifi"
      label="Wi-Fi"
      value={!live || !network.current
        ? "—"
        : (wifi?.wifi?.ssid ?? (wifiConnected(wifi) ? "Connected" : wifi ? "Not connected" : "No Wi-Fi"))}
    >
      {#if !live}
        Not reachable
      {:else if wifi?.wifi}
        <SignalBars dbm={wifi.wifi.signalDbm} />{signalWord(wifi.wifi.signalDbm)}{wifi.wifi.frequencyMhz
          ? ` · ${band(wifi.wifi.frequencyMhz)}`
          : ""}
      {:else if wifiConnected(wifi)}
        No signal details
      {:else}
        &nbsp;
      {/if}
    </StatTile>
    <StatTile href="#/system/updates" icon="download" label="Software" value={s.version} mono>
      {#if !live}
        Not reachable
      {:else if booted}
        Slot {booted.name.toUpperCase()} · {slotWord(booted).toLowerCase()}
      {:else if updates && !updates.available}
        Updates not available here
      {:else}
        &nbsp;
      {/if}
    </StatTile>
    <StatTile href="#/system" icon="clock" label="Uptime" value={live ? uptime(up) : "—"} mono>
      {#if live}Since {since(new Date(device.now), up)}{:else}Not reachable{/if}
    </StatTile>
  </div>

  <div class="cols">
    {#if s.virtualDevice}<VirtualDevice />{/if}
    <Controls />
    <!-- One column: beside the tall controls on a wide screen, so Now
         playing is in view without scrolling. -->
    <div class="stack">
      <Timers />
      <NowPlaying />
    </div>
  </div>
{/if}

<style>
  .hero {
    position: relative;
    overflow: hidden;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    justify-items: center;
    gap: var(--space-3);
    padding: 20px var(--space-4) 22px;
    text-align: center;
    background: var(--surface-raised);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
  }
  .who {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  h1 {
    margin: 0;
    overflow-wrap: break-word;
  }
  .state {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: var(--space-2) var(--space-4);
  }
  .held {
    color: var(--ink-muted);
    font-weight: 650;
  }
  .meta {
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 18px;
    font-weight: 500;
    color: var(--ink-muted);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-3);
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-6);
    align-items: start;
  }
  .stack {
    display: grid;
    gap: var(--space-6);
    min-width: 0;
  }
  /* Two columns once the content (beside the sidebar) has room for the name. */
  @media (min-width: 1000px) {
    .hero {
      grid-template-columns: auto minmax(0, 1fr);
      justify-items: start;
      align-items: center;
      gap: 36px;
      padding: 28px 36px;
      text-align: left;
    }
    .who {
      align-items: flex-start;
    }
    .state {
      justify-content: flex-start;
    }
  }
  @media (min-width: 1000px) {
    .stats {
      grid-template-columns: repeat(4, minmax(0, 1fr));
      gap: var(--space-4);
    }
  }
  @media (min-width: 1240px) {
    .cols {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
