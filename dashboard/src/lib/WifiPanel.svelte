<!-- Wi-Fi networks on the Network page (ADR 0008): the saved ones, which can
     be forgotten; a scan to join one from; a hidden network joined by name.
     A join is a trial: the satellite keeps the network once the dashboard is
     opened at its new address or someone keeps it (WifiTrial, on every page).
     The join's steps show in its form; a failed one keeps the form to retry. -->
<script lang="ts">
  import { tick } from "svelte";

  import type { WifiNetwork } from "../gen/pinna/v1/device_pb.ts";
  import Confirm from "./Confirm.svelte";
  import { device } from "./device.svelte.ts";
  import Group from "./Group.svelte";
  import Dropdown from "./Dropdown.svelte";
  import Icon from "./Icon.svelte";
  import { network } from "./network.svelte.ts";
  import SignalBars from "./SignalBars.svelte";
  import { hasUsb, joinTone, securityWord, unjoinable } from "./view.ts";
  import { wifi } from "./wifi.svelte.ts";

  // WifiSecurity and JoinStage values.
  const OPEN = 1;
  const WPA2 = 2;
  const FAILED = 5;

  const live = $derived(device.link === "live");
  const overUsb = location.hostname === "10.42.0.1";
  const usbModel = $derived(hasUsb(device.status?.model ?? ""));
  // On the setup hotspot (ADR 0028), which goes away while the satellite joins.
  const overHotspot = location.hostname === "10.42.1.1";
  const mac = $derived(network.wireless?.mac?.toLowerCase() ?? "");
  // Its name on any network (ADR 0010): the node name, as mDNS announces it.
  const local = $derived(`${device.status?.name ?? "pinna"}.local`);

  let scanned = $state<WifiNetwork[] | null>(null);
  let scanning = $state(false);
  let scanError = $state<string | null>(null);

  // The network to join: picked from the scan, or typed in (hidden).
  let target = $state<{ ssid: string; security: number; hidden: boolean } | null>(null);
  let password = $state("");
  let showPassword = $state(false);
  let joinError = $state<string | null>(null);
  let nameError = $state<string | null>(null);
  let form = $state<HTMLFormElement>();
  // This page started the join whose outcome shows.
  let mine = $state(false);
  let forgetting = $state<string | null>(null);
  let confirmOpen = $state(false);

  // The scan's rows, with saved and in use from the saved list, which stays current.
  const rows = $derived(
    (scanned ?? []).map((n) => {
      const s = wifi.saved?.find((x) => x.ssid === n.ssid);
      return { ...n, saved: !!s, current: !!s?.current };
    }),
  );
  const busy = $derived(!live || wifi.joining || !!wifi.trial);
  const step = $derived(wifi.steps.at(-1));
  // A join from another page, or before this page loaded.
  const otherJoin = $derived(wifi.joining && wifi.steps.length === 0);
  // How this page's last join ended, once the form is closed. On the setup
  // hotspot also a failed join from before this page: it brought the hotspot back.
  const ended = $derived(
    !wifi.joining && !wifi.trial && !target && (mine || (overHotspot && wifi.last?.stage === FAILED))
      ? wifi.last
      : null,
  );
  const tone = $derived(joinTone(ended));
  const forgettingCurrent = $derived(wifi.saved?.find((n) => n.ssid === forgetting)?.current ?? false);
  const othersSaved = $derived((wifi.saved?.length ?? 0) > 1);

  async function scan() {
    scanning = true;
    scanError = null;
    const r = await wifi.scan();
    if (typeof r === "string") scanError = r;
    else scanned = r;
    scanning = false;
  }

  async function open(next: { ssid: string; security: number; hidden: boolean }) {
    target = next;
    password = "";
    showPassword = false;
    joinError = null;
    nameError = null;
    await tick();
    form?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    form?.querySelector<HTMLInputElement>("input")?.focus();
  }

  const bytes = (s: string) => new TextEncoder().encode(s).length;

  async function join() {
    if (!target) return;
    joinError = null;
    nameError = null;
    if (bytes(target.ssid.trim()) > 32) {
      nameError = "That name is too long for Wi-Fi: 32 bytes at most, and accents and emoji take more than one.";
      return;
    }
    mine = true;
    const refused = await wifi.join({
      ssid: target.hidden ? target.ssid.trim() : target.ssid,
      security: target.security,
      passphrase: target.security === OPEN ? "" : password,
      hidden: target.hidden,
    });
    if (refused) {
      if (/network.s name|network name/i.test(refused)) nameError = refused;
      else joinError = refused;
      return;
    }
    // It didn't work: keep the form, and what was typed, to try again.
    if (step?.stage === FAILED) {
      joinError = step.message;
      return;
    }
    // On trial (its banner takes over), or this page lost the satellite.
    target = null;
    password = "";
    await tick();
    document.querySelector(".wifi-trial")?.scrollIntoView({ behavior: "smooth", block: "center" });
  }
</script>

{#if wifi.absent}
  <div class="pn-notice" data-tone="caution" role="status">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p>{wifi.absent}</p>
      {#if /restart/i.test(wifi.absent)}<p><a href="#/system">Restart it under System</a></p>{/if}
    </div>
  </div>
{:else}
  {#if otherJoin}
    <div class="pn-notice" role="status">
      <Icon name="wifi" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">The satellite is joining a network</p>
        <p>Someone started it from another page. Scanning and joining wait until it has finished.</p>
      </div>
    </div>
  {:else if ended && tone}
    <div class="pn-notice" data-tone={tone === "ok" ? undefined : "caution"} role="status">
      <Icon name={tone === "ok" ? "check" : "alert"} class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body"><p>{ended.message}</p></div>
    </div>
  {/if}

  <Group title="Saved networks">
    {#snippet foot()}
      The satellite joins the first one it finds, top to bottom.
    {/snippet}
    {#each wifi.saved ?? [] as n (n.ssid)}
      <div class="pn-row">
        <span class="pn-row-icon"><Icon name="wifi" /></span>
        <div class="pn-row-text">
          <span class="pn-row-label">{n.ssid}</span>
          <span class="pn-row-hint">{securityWord(n.security)}{n.hidden ? " · Hidden" : ""}</span>
        </div>
        {#if n.current}<span class="pn-health" data-tone="ok">In use</span>{/if}
        <button
          class="pn-btn pn-btn-quiet pn-btn-sm"
          disabled={busy || wifi.busy}
          onclick={() => {
            forgetting = n.ssid;
            confirmOpen = true;
          }}
          aria-label="Forget {n.ssid}"
        >
          Forget
        </button>
      </div>
    {:else}
      <div class="pn-row">
        {#if wifi.error}
          <span class="pn-field-error" role="alert">
            <Icon name="alert" />Couldn't read the saved networks: {wifi.error}
          </span>
        {:else}
          <span class="pn-row-hint">
            {#if !wifi.saved}
              Reading the saved networks…
            {:else if wifi.hotspot}
              No saved networks. The satellite runs its setup hotspot, <strong>{wifi.hotspot}</strong>, with the
              password <code class="pn-code">pinnasetup</code>, until it joins one.
            {:else}
              No saved networks: the satellite is reachable over USB only.
            {/if}
          </span>
        {/if}
      </div>
    {/each}
    {#if wifi.error && wifi.saved?.length}
      <div class="pn-row"><span class="pn-field-error" role="alert"><Icon name="alert" />{wifi.error}</span></div>
    {/if}
  </Group>

  <Group title="Join a network">
    {#snippet aside()}
      <button class="pn-btn pn-btn-sm" disabled={!live || scanning || wifi.joining} onclick={scan}>
        {scanning ? "Scanning…" : scanned ? "Scan again" : "Scan"}
      </button>
    {/snippet}
    {#if scanError}
      <div class="pn-row"><span class="pn-field-error" role="alert"><Icon name="alert" />{scanError}</span></div>
    {/if}
    {#if scanned}
      {#each rows as n (n.ssid)}
        {@const why = unjoinable(n.security)}
        <button
          class="pn-row"
          disabled={!!why || n.current || busy}
          aria-pressed={target?.ssid === n.ssid && !target.hidden}
          onclick={() => open({ ssid: n.ssid, security: n.security, hidden: false })}
        >
          <SignalBars dbm={n.signalDbm} />
          <span class="pn-row-text">
            <span class="pn-row-label">{n.ssid}</span>
            <span class="pn-row-hint">
              {why ?? securityWord(n.security)}{n.current ? " · In use" : n.saved ? " · Saved" : ""}
            </span>
          </span>
          {#if !why && !n.current}<Icon name="chevron-right" class="pn-icon pn-row-chev" />{/if}
        </button>
      {:else}
        <div class="pn-row"><span class="pn-row-hint">No networks in range.</span></div>
      {/each}
    {:else if !scanning}
      <div class="pn-row">
        <span class="pn-row-hint">Scan to see the networks in range.</span>
      </div>
    {/if}
    <button class="pn-row" disabled={busy} onclick={() => open({ ssid: "", security: WPA2, hidden: true })}>
      <span class="pn-row-text">
        <span class="pn-row-label">Join a hidden network</span>
        <span class="pn-row-hint">A network that doesn't show its name: type it in.</span>
      </span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </button>

    {#if target}
      <form
        bind:this={form}
        class="pn-row is-stack join"
        onsubmit={(e) => {
          e.preventDefault();
          void join();
        }}
      >
        <p class="pn-row-label">{target.hidden ? "Join a hidden network" : `Join ${target.ssid}`}</p>
        {#if target.hidden}
          <div class="pn-field" data-invalid={nameError ? "" : undefined}>
            <label class="pn-field-label" for="wifi-ssid">Network name</label>
            <input
              id="wifi-ssid"
              class="pn-input"
              autocomplete="off"
              autocapitalize="off"
              spellcheck="false"
              bind:value={target.ssid}
              disabled={wifi.joining}
              aria-invalid={nameError ? "true" : undefined}
              oninput={() => (nameError = null)}
            />
            {#if nameError}<span class="pn-field-error" role="alert"><Icon name="alert" />{nameError}</span>{/if}
            <span class="pn-field-hint">Exactly as the router has it, capitals included.</span>
          </div>
          <div class="pn-field">
            <label class="pn-field-label" for="wifi-security">Security</label>
            <Dropdown
              id="wifi-security"
              value={String(target.security)}
              options={[
                { id: String(WPA2), label: "WPA2 (or WPA2/WPA3)" },
                { id: String(OPEN), label: "Open, no password" },
              ]}
              disabled={wifi.joining}
              choose={(v) => target && (target.security = Number(v))}
            />
          </div>
        {:else if nameError}
          <span class="pn-field-error" role="alert"><Icon name="alert" />{nameError}</span>
        {/if}
        {#if target.security !== OPEN}
          <div class="pn-field" data-invalid={joinError ? "" : undefined}>
            <label class="pn-field-label" for="wifi-password">Password</label>
            <div class="inline">
              <input
                id="wifi-password"
                class="pn-input"
                type={showPassword ? "text" : "password"}
                autocomplete="off"
                autocapitalize="off"
                spellcheck="false"
                value={password}
                disabled={wifi.joining}
                aria-invalid={joinError ? "true" : undefined}
                oninput={(e) => {
                  password = e.currentTarget.value;
                  joinError = null;
                }}
              />
              <button
                class="pn-btn pn-btn-quiet pn-btn-sm"
                type="button"
                aria-pressed={showPassword}
                onclick={() => (showPassword = !showPassword)}
              >
                {showPassword ? "Hide" : "Show"}
              </button>
            </div>
          </div>
        {/if}
        {#if joinError}<span class="pn-field-error" role="alert"><Icon name="alert" />{joinError}</span>{/if}
        {#if wifi.joining && mine}
          <p class="pn-field-hint" role="status">{step?.message ?? "Starting…"} This takes up to a minute.</p>
        {/if}
        <!-- On the hotspot the page goes away during the join: the address stays in view. -->
        {#if !(wifi.joining && mine) || overHotspot}
          <p class="pn-field-hint">
            {#if overUsb}
              This page stays connected over USB. Once the satellite is on the network, keep it here.
            {:else if overHotspot}
              Note this first: the satellite turns its hotspot off to try the network, so this page loses it. If it
              joins, connect to that network within 5 minutes and open
              <code class="pn-code">http://{local}/</code>{#if mac}
                (on Android, find <code class="pn-code">{mac}</code> in your router and open its address
                instead){/if}: it stays there. If it doesn't join, the hotspot comes back in about two minutes: join it
              again to see why.
            {:else}
              This page may lose the satellite. If it joins, connect to that network within 5 minutes and open
              <code class="pn-code">http://{local}/</code>{#if mac}
                (or find <code class="pn-code">{mac}</code> in your router and open its address){/if}: it stays there.
              Otherwise it comes back here.{#if usbModel}
                With a USB cable, <code class="pn-code">http://10.42.0.1/</code> reaches it either way.{/if}
            {/if}
          </p>
        {/if}
        <div class="actions">
          <button
            class="pn-btn pn-btn-primary"
            type="submit"
            disabled={!live ||
              wifi.joining ||
              !target.ssid.trim() ||
              (target.security !== OPEN && password.length === 0)}
          >
            <Icon name="wifi" />{wifi.joining && mine ? "Joining…" : joinError ? "Try again" : "Join"}
          </button>
          <button class="pn-btn pn-btn-quiet" type="button" disabled={wifi.joining} onclick={() => (target = null)}>
            Cancel
          </button>
        </div>
      </form>
    {/if}
  </Group>

  <Confirm
    bind:open={confirmOpen}
    title="Forget {forgetting}?"
    action="Forget"
    danger
    confirm={() => forgetting && wifi.forget(forgetting)}
  >
    <p>
      The satellite won't join it again until you join it here.
      {#if forgettingCurrent && othersSaved}
        It disconnects from it now and tries the other saved networks, so a page opened over Wi-Fi may lose it.
      {:else if forgettingCurrent && wifi.hasHotspot}
        It disconnects from it now and has no other network then: it runs its setup hotspot, and is reachable over
        USB at <code class="pn-code">http://10.42.0.1/</code>.
      {:else if forgettingCurrent}
        It disconnects from it now and has no other network then: it is reachable over USB only, at
        <code class="pn-code">http://10.42.0.1/</code>.
      {/if}
    </p>
  </Confirm>
{/if}

<style>
  .join {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: var(--space-3);
    text-align: left;
  }
  .join :global(.pn-select-wrap) {
    align-self: flex-start;
  }
  .inline {
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }
  .inline input {
    flex: 1 1 auto;
    min-width: 0;
    width: auto;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  button.pn-row {
    width: 100%;
    text-align: left;
    border: 0;
    background: none;
    font: inherit;
    color: inherit;
  }
  button.pn-row:disabled {
    cursor: default;
    opacity: 0.6;
  }
  button.pn-row[aria-pressed="true"] {
    background: var(--surface-sunken);
  }
</style>
