<!-- Assistant › Tools: what the brain and MCP clients may do on the
     satellite, the owner's radio stations, and Pinna's own MCP server. The
     switches apply to the brain and to /mcp alike. -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { McpServer } from "../../gen/pinna/v1/assistant_pb.ts";
  import { assistant } from "../../lib/assistant.svelte.ts";
  import {
    BRAIN_TOOLS,
    brainToolsOn,
    delegation,
    isStandalone,
    mcpAddress,
    mcpStatus,
    mcpToolOn,
    toolLabel,
    toolOn,
    toolReach,
  } from "../../lib/assistant.ts";
  import Confirm from "../../lib/Confirm.svelte";
  import { copyText } from "../../lib/copy.ts";
  import { device } from "../../lib/device.svelte.ts";
  import Group from "../../lib/Group.svelte";
  import Icon from "../../lib/Icon.svelte";
  import { network } from "../../lib/network.svelte.ts";
  import SwitchRow from "../../lib/SwitchRow.svelte";
  import { toasts } from "../../lib/toast.svelte.ts";
  import { shortUrl, when } from "../../lib/view.ts";
  import McpServerForm from "./McpServerForm.svelte";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const view = $derived(assistant.view);
  const standalone = $derived(isStandalone(s));

  onMount(() => void assistant.load());
  $effect(() => network.watch());

  // An owner's MCP tool, see mcpToolOn; Pinna's own, see toolOn.
  const isOn = (name: string) => mcpToolOn(view?.tools ?? {}, name);
  function setTool(name: string, on: boolean) {
    return assistant.update({ tools: { on: { ...view?.tools, [name]: on } } }, on ? "Tool on" : "Tool off");
  }
  /** Every tool `srv` listed on, or off, in one change. */
  function setAll(srv: McpServer, on: boolean) {
    const next = { ...view?.tools };
    for (const t of srv.listed) next[t.name] = on;
    return assistant.update({ tools: { on: next } }, `${srv.name}'s tools ${on ? "on" : "off"}`);
  }
  const brainTools = $derived(view ? brainToolsOn(view) : 0);

  // The stations are saved as a whole list.
  let stationName = $state("");
  let stationUrl = $state("");
  let stationError = $state<string | null>(null);
  async function saveStations(stations: { name: string; url: string }[], done: string) {
    stationError = await assistant.update({ stations: { stations } }, done);
    return !stationError;
  }
  async function addStation() {
    const list = (view?.stations ?? []).map(({ name, url }) => ({ name, url }));
    const added = { name: stationName.trim(), url: stationUrl.trim() };
    if (await saveStations([...list, added], `${added.name} added`)) {
      stationName = "";
      stationUrl = "";
    }
  }
  // The station Remove asked about; it goes once the dialog confirms.
  let removing = $state("");
  let removeOpen = $state(false);
  function removeStation(name: string) {
    const list = (view?.stations ?? []).filter((x) => x.name !== name).map(({ name, url }) => ({ name, url }));
    return saveStations(list, `${name} removed`);
  }

  // The owner's MCP servers: offered only to a brain the satellite runs the
  // tools for, not to an agent, which has its own.
  const agentBrain = $derived(!!view && delegation(view) === "hand-off");
  /** The servers whose tools are unfolded. */
  let shownTools = $state(new Set<string>());
  function toggleTools(id: string) {
    const next = new Set(shownTools);
    if (!next.delete(id)) next.add(id);
    shownTools = next;
  }
  /** The server being changed, "new" while one is added, or null. */
  let editing = $state<string | null>(null);
  let serverError = $state<string | null>(null);
  async function setEnabled(id: string, revision: bigint, name: string, enabled: boolean) {
    serverError = await assistant.setMcpServer({ id, revision, name, enabled }, enabled ? `${name} on` : `${name} off`);
  }

  const address = $derived(s ? mcpAddress(location, network.ipv4 ?? undefined, s.name) : "");
  async function copyAddress() {
    await copyText(address);
    toasts.show("Copied the address");
  }
</script>

{#if view && s}
  <Group title="Pinna's tools">
    {#snippet foot()}
      Each switch applies to the brain and to MCP clients alike. A tool that is off isn't offered, and is refused if
      asked for anyway.{agentBrain ? " The brain is an agent and calls none of them: only MCP clients do." : ""}
    {/snippet}
    {#each view.deviceTools as t (t.name)}
      <SwitchRow
        id="tool-{t.name}"
        label={toolLabel(t.name)}
        checked={toolOn(view.tools, t)}
        disabled={!live}
        busy={assistant.busy}
        hint={[t.description, toolReach(t)].filter(Boolean).join(" ")}
        toggle={(on) => setTool(t.name, on)}
      />
    {/each}
  </Group>

  {#snippet serversFoot()}
    The brain can call the tools of MCP servers on your network next to Pinna's. Saving one asks it for its tools; a server
    that doesn't answer is left out of a request. At most 8.
  {/snippet}
  <Group title="Your MCP servers" foot={agentBrain ? undefined : serversFoot}>
    {#if agentBrain}
      <div class="pn-row">
        <span class="pn-row-hint">
          The brain is an agent: it brings tools of its own, so the satellite offers it none of these. Its MCP servers
          are set up on the agent.
        </span>
      </div>
    {:else}
      {#if brainTools > BRAIN_TOOLS}
        <div class="pn-row">
          <span class="pn-row-hint is-caution" role="status">
            {brainTools} tools are on, Pinna's and your servers'. The brain takes {BRAIN_TOOLS}: Pinna's first, then the
            servers in this order, so the last {brainTools - BRAIN_TOOLS} are left out. Switch some off.
          </span>
        </div>
      {/if}
      {#each view.mcpServers as srv (srv.id)}
        {#if editing === srv.id}
          <McpServerForm server={srv} onclose={() => (editing = null)} />
        {:else}
          <div class="pn-row">
            <div class="pn-row-text">
              <label id="mcp-{srv.id}-label" class="pn-row-label server-name" for="mcp-{srv.id}">{srv.name}</label>
              <span id="mcp-{srv.id}-hint" class="pn-row-hint"
                >On {srv.host} · <span class={mcpStatus(srv, view.tools).failed ? "problem" : undefined}
                  >{mcpStatus(srv, view.tools).text}</span
                ></span
              >
            </div>
            <button
              class="pn-btn pn-btn-sm"
              type="button"
              disabled={!live || editing !== null}
              aria-label="Change {srv.name}"
              onclick={() => {
                serverError = null;
                editing = srv.id;
              }}>Change</button
            >
            <button
              id="mcp-{srv.id}"
              class="pn-switch-control"
              role="switch"
              aria-checked={srv.enabled}
              aria-labelledby="mcp-{srv.id}-label"
              aria-describedby="mcp-{srv.id}-hint"
              disabled={!live}
              aria-disabled={assistant.busy || undefined}
              onclick={() => {
                if (!assistant.busy) void setEnabled(srv.id, srv.revision, srv.name, !srv.enabled);
              }}
            ></button>
          </div>
          {#if srv.enabled && srv.listed.length}
            {@const open = shownTools.has(srv.id)}
            {@const on = srv.listed.filter((t) => isOn(t.name)).length}
            <button
              class="pn-row is-nested"
              type="button"
              aria-expanded={open}
              aria-controls="mcp-{srv.id}-tools"
              onclick={() => toggleTools(srv.id)}
            >
              <span class="pn-row-text">
                <span class="pn-row-label">Its tools</span>
                <span class="pn-row-hint">
                  {on ? "Turn off the ones the brain shouldn't call." : "Turn on the ones the brain may call."}
                </span>
              </span>
              <Icon name={open ? "chevron-down" : "chevron-right"} class="pn-icon pn-row-chev" />
            </button>
            {#if open}
              <div id="mcp-{srv.id}-tools" class="pn-row-children">
                <div class="pn-row is-actions">
                  <button
                    class="pn-btn pn-btn-sm"
                    type="button"
                    disabled={!live || assistant.busy || on === srv.listed.length}
                    onclick={() => void setAll(srv, true)}>All on</button
                  >
                  <button
                    class="pn-btn pn-btn-sm"
                    type="button"
                    disabled={!live || assistant.busy || on === 0}
                    onclick={() => void setAll(srv, false)}>All off</button
                  >
                </div>
                {#each srv.listed as t (t.name)}
                  <SwitchRow
                    id="tool-{t.name}"
                    label={t.name}
                    checked={isOn(t.name)}
                    disabled={!live}
                    busy={assistant.busy}
                    hint={t.description}
                    toggle={(on) => setTool(t.name, on)}
                  />
                {/each}
              </div>
            {/if}
          {/if}
        {/if}
      {/each}
      {#if serverError}
        <div class="pn-row">
          <span class="pn-field-error" role="alert"><Icon name="alert" />{serverError}</span>
        </div>
      {/if}
      {#if editing === "new"}
        <McpServerForm onclose={() => (editing = null)} />
      {:else if view.mcpServers.length < 8}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{view.mcpServers.length ? "Add another server" : "Add a server"}</span>
            <span class="pn-row-hint">By its address, which may carry a token</span>
          </div>
          <button
            class="pn-btn pn-btn-sm"
            type="button"
            disabled={!live || editing !== null}
            onclick={() => {
              serverError = null;
              editing = "new";
            }}>Add…</button
          >
        </div>
      {/if}
    {/if}
  </Group>

  <Group title="Radio stations">
    {#snippet foot()}
      The brain plays these by name (“play the jazz station”) and can search for others on radio-browser.info. At
      most 50.
    {/snippet}
    {#each view.stations as st (st.name)}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">{st.name}</span>
          <span class="pn-row-hint url" title={st.url}>{shortUrl(st.url)}</span>
        </div>
        <button
          class="pn-btn pn-btn-quiet pn-btn-sm"
          type="button"
          disabled={!live}
          aria-disabled={assistant.busy || undefined}
          aria-label="Remove {st.name}"
          onclick={() => {
            if (assistant.busy) return;
            removing = st.name;
            removeOpen = true;
          }}>Remove</button
        >
      </div>
    {/each}
    <form
      class="pn-row is-stack"
      aria-label="Add a station"
      onsubmit={(e) => {
        e.preventDefault();
        void addStation();
      }}
    >
      <div class="station">
        <div class="pn-field">
          <label class="pn-field-label" for="station-name">Name</label>
          <input
            id="station-name"
            class="pn-input"
            autocomplete="off"
            maxlength="60"
            placeholder="Radio Eins"
            bind:value={stationName}
            disabled={!live}
            oninput={() => (stationError = null)}
          />
        </div>
        <div class="pn-field">
          <label class="pn-field-label" for="station-url">Stream address</label>
          <input
            id="station-url"
            class="pn-input is-mono"
            type="text"
            inputmode="url"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            placeholder="https://example.org/stream.mp3"
            bind:value={stationUrl}
            disabled={!live}
            oninput={() => (stationError = null)}
          />
        </div>
      </div>
      {#if stationError}<span class="pn-field-error" role="alert"><Icon name="alert" />{stationError}</span>{/if}
      <div>
        <button
          class="pn-btn pn-btn-primary"
          type="submit"
          disabled={!live || assistant.busy || !stationName.trim() || !stationUrl.trim()}
        >
          Add station
        </button>
      </div>
    </form>
  </Group>
  <Confirm
    bind:open={removeOpen}
    title="Remove {removing}?"
    action="Remove"
    danger
    confirm={() => removeStation(removing)}
  >
    <p>The brain can no longer play it by name. To get it back, add it again with its address.</p>
  </Confirm>

  <Group title="Pinna as an MCP server">
    {#snippet foot()}
      Agents and apps that speak MCP can use Pinna's tools over your network, without a password, as this dashboard
      is. Pinna answers them in standalone mode only.
    {/snippet}
    <div class="pn-row is-stack">
      <div class="pn-row-text">
        <span class="pn-row-label">Address</span>
        <div class="address">
          <span class="is-mono">{address}</span>
          <button
            class="pn-btn pn-btn-quiet pn-btn-sm"
            type="button"
            onclick={copyAddress}
            aria-label="Copy the MCP address"
          >
            <Icon name="copy" />Copy
          </button>
        </div>
        <span class="pn-row-hint">
          {standalone ? "Enter it in the MCP client's settings." : "It answers once standalone mode is on."}
        </span>
      </div>
    </div>
    {#each view.agents as a (`${a.name} ${a.address}`)}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">{a.name || "A client without a name"}</span>
          <span class="pn-row-hint">From {a.address}, last called</span>
        </div>
        <span class="pn-row-value">{when(Number(a.lastSeenUnixMs), new Date(device.now))}</span>
      </div>
    {:else}
      <div class="pn-row">
        <span class="pn-row-hint">No MCP client has called since the satellite started.</span>
      </div>
    {/each}
  </Group>
{:else if assistant.error}
  <div class="pn-notice" data-tone="danger" role="alert">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">The tools couldn't be read</p>
      <p>{assistant.error}</p>
      <div><button class="pn-btn" type="button" onclick={() => assistant.load()}>Try again</button></div>
    </div>
  </div>
{:else}
  <span class="pn-skeleton" style:height="320px"></span>
{/if}

<style>
  .problem {
    color: var(--danger);
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .station {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-3) var(--space-4);
  }
  @media (min-width: 640px) {
    .station {
      grid-template-columns: minmax(0, 1fr) minmax(0, 2fr);
    }
  }
  .url {
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
  }
  .server-name {
    font-family: var(--font-mono);
  }
  .address {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-1) var(--space-3);
  }
  .is-mono {
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 14px;
    font-weight: 500;
    overflow-wrap: anywhere;
  }
</style>
