<!-- Assistant: who answers requests (ADR 0017). The mode card first; then, in
     Home Assistant mode, how Home Assistant reaches the satellite, and in
     standalone mode, what answers and how the last request went. -->
<script lang="ts">
  import { onMount } from "svelte";

  import { Mode } from "../../gen/pinna/v1/device_pb.ts";
  import { assistant } from "../../lib/assistant.svelte.ts";
  import { isStandalone, slotLines } from "../../lib/assistant.ts";
  import Confirm from "../../lib/Confirm.svelte";
  import { device } from "../../lib/device.svelte.ts";
  import Group from "../../lib/Group.svelte";
  import Icon from "../../lib/Icon.svelte";
  import HomeAssistant from "../HomeAssistant.svelte";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const standalone = $derived(isStandalone(s));
  const ready = $derived(!!s?.assistant?.ready);
  const lines = $derived(slotLines(s?.assistant, assistant.view));
  const brain = $derived(lines.find((l) => l.key === "brain"));

  onMount(() => void assistant.load());

  // The mode the confirmation asks about.
  let asking = $state<Mode>(Mode.HOME_ASSISTANT);
  let confirming = $state(false);
  let modeError = $state<string | null>(null);

  function choose(mode: Mode) {
    if (!live || device.busy || mode === s?.mode) return;
    if (mode === Mode.STANDALONE && !ready) return;
    modeError = null;
    asking = mode;
    confirming = true;
  }

  async function switchMode() {
    const to = asking;
    modeError = await device.saveSettings(
      { mode: to },
      to === Mode.STANDALONE ? "Standalone mode is on" : "Home Assistant mode is on",
    );
  }
</script>

{#if s}
  <Group title="Mode">
    <div class="pn-row is-actions">
      <div class="pn-row-text">
        <span id="mode-label" class="pn-row-label">Runs requests with</span>
        <span id="mode-hint" class="pn-row-hint">
          {#if standalone && ready}
            The satellite answers by itself, with the services you set up. Home Assistant can still play music on it
            and control its light.
          {:else if standalone}
            Standalone mode is on, but the satellite can't answer until every service below passes its check.
          {:else if !s.haConnected}
            Waiting for Home Assistant. Once it connects, its voice assistant hears each request and answers it.
          {:else}
            Home Assistant's voice assistant hears each request and answers it.
          {/if}
        </span>
      </div>
      <div class="pn-segmented" role="group" aria-labelledby="mode-label" aria-describedby="mode-hint">
        <button
          type="button"
          aria-pressed={!standalone}
          disabled={!live}
          aria-disabled={device.busy || undefined}
          onclick={() => choose(Mode.HOME_ASSISTANT)}>Home Assistant</button
        >
        <button
          type="button"
          aria-pressed={standalone}
          disabled={!live}
          aria-disabled={device.busy || (!standalone && !ready) || undefined}
          aria-describedby={!standalone && !ready ? "standalone-why" : undefined}
          onclick={() => choose(Mode.STANDALONE)}>Standalone</button
        >
      </div>
    </div>
    {#if modeError}
      <div class="pn-row">
        <span class="pn-field-error" role="alert"><Icon name="alert" />{modeError}</span>
      </div>
    {/if}
    {#if !ready}
      {#if !standalone}
        <div class="pn-row">
          <span id="standalone-why" class="pn-row-hint">
            To switch to standalone, set up speech-to-text, the brain and text-to-speech under Services and check them
            first.
          </span>
        </div>
      {/if}
      {#each lines as l (l.key)}
        <a class="pn-row" href="#/assistant/services">
          <span class="pn-row-text">
            <span class="pn-row-label">{l.name}</span>
            <span class="pn-row-hint">{l.detail}</span>
          </span>
          <span class="pn-row-value"><span class="pn-health" data-tone={l.tone}>{l.word}</span></span>
          <Icon name="chevron-right" class="pn-icon pn-row-chev" />
        </a>
      {/each}
    {/if}
  </Group>

  {#if standalone}
    <Group title="Standalone">
      {#if brain?.ready}
        <div class="pn-row">
          <div class="pn-row-text"><span class="pn-row-label">Brain</span></div>
          <span class="pn-row-value">{brain.detail}</span>
        </div>
      {/if}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Last request</span>
          {#if s.assistant?.lastError}
            <span class="pn-row-hint is-danger">Failed: {s.assistant.lastError}</span>
          {:else}
            <span class="pn-row-hint">No request has failed since the satellite started.</span>
          {/if}
        </div>
      </div>
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Home Assistant</span>
          <span class="pn-row-hint">
            It can still play music on the satellite and control its light, but it doesn't answer requests.
          </span>
        </div>
        <span class="pn-row-value">
          {#if !live}
            <span class="pn-health" data-tone="danger">Not reachable</span>
          {:else if s.haConnected}
            <span class="pn-health" data-tone="ok">Connected</span>
          {:else}
            Not connected
          {/if}
        </span>
      </div>
    </Group>
  {:else}
    <HomeAssistant />
  {/if}

  {#snippet setupFoot()}
    Everything here can be set up while Home Assistant answers. Switch above once all three services pass their check.
  {/snippet}
  <Group title={standalone ? "Setup" : "Set up standalone"} foot={standalone ? undefined : setupFoot}>
    <a class="pn-row" href="#/assistant/services">
      <span class="pn-row-text">
        <span class="pn-row-label">Services</span>
        <span class="pn-row-hint">Speech-to-text, the brain and text-to-speech, with their keys</span>
      </span>
      <span class="pn-row-value">{lines.filter((l) => l.ready).length} of 3 ready</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
    <a class="pn-row" href="#/assistant/conversation">
      <span class="pn-row-text">
        <span class="pn-row-label">Conversation</span>
        <span class="pn-row-hint">Language, place, the prompt, follow-ups, and a box to type a question</span>
      </span>
      {#if assistant.view?.language}<span class="pn-row-value is-mono">{assistant.view.language}</span>{/if}
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <Confirm
    bind:open={confirming}
    title={asking === Mode.STANDALONE ? "Switch to standalone?" : "Switch back to Home Assistant?"}
    action="Switch"
    confirm={switchMode}
  >
    {#if asking === Mode.STANDALONE}
      <p>
        The satellite answers requests itself, with the services set up here. Home Assistant's voice assistant stops
        hearing it, and the timers that run now are cancelled.
      </p>
      <p>Home Assistant can still play music on it and control its light.</p>
    {:else}
      <p>
        Home Assistant's voice assistant answers requests again once it is connected. The timers that run now are
        cancelled.
      </p>
    {/if}
  </Confirm>
{/if}

<style>
  .is-danger {
    color: var(--danger);
    font-weight: 650;
  }
</style>
