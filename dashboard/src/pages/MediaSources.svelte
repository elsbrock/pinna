<!-- Voice & sound › Media sources: what may play music to the satellite
     (ADR 0011, ADR 0027). Each source is off until switched on here or in Home
     Assistant; while it is on, the page says whether it actually runs. -->
<script lang="ts">
  import { AirPlayState, BluetoothState, MediaState, SendspinState } from "../gen/pinna/v1/device_pb.ts";
  import Confirm from "../lib/Confirm.svelte";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import { network } from "../lib/network.svelte.ts";
  import SwitchRow from "../lib/SwitchRow.svelte";

  const s = $derived(device.status);
  const set = $derived(s?.settings);
  const live = $derived(device.link === "live");
  const airplay = $derived(s?.airplay);
  const unavailable = $derived(airplay?.state === AirPlayState.AIRPLAY_STATE_UNAVAILABLE);
  const hint = $derived(
    unavailable
      ? "This satellite has no working speaker, so it can't receive AirPlay. Logs, under System, says why."
      : "Play from an iPhone, iPad, Mac or Music Assistant. While it is on, anyone on your network can play to it without a password, and replace what it is playing.",
  );

  const bt = $derived(s?.bluetooth);
  const btUnavailable = $derived(bt?.state === BluetoothState.UNAVAILABLE);
  const btReady = $derived(bt?.state === BluetoothState.READY);
  const btFailed = $derived(bt?.state === BluetoothState.FAILED);
  const btHint = $derived(
    btUnavailable
      ? "This satellite has no Bluetooth, or no working speaker. Logs, under System, says why."
      : "Play from a phone or tablet nearby. Only phones paired with this satellite can connect.",
  );
  // Seconds left in the pairing window, counted from when the Status arrived,
  // as timers are: the satellite's clock and this one's don't matter.
  const pairingLeft = $derived(
    Math.max(0, (bt?.pairingSecondsLeft ?? 0) - Math.floor((device.now - device.receivedAt) / 1000)),
  );
  const pairing = $derived(btReady && pairingLeft > 0);
  const mmss = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  const phones = $derived(bt?.devices.length ?? 0);
  // Bluetooth shares the antenna with the Wi-Fi; on 2.4 GHz that shows.
  $effect(() => {
    if (set?.bluetooth) return network.watch();
  });
  const mhz = $derived(network.wireless?.wifi?.frequencyMhz ?? 0);
  const onTwoFour = $derived(mhz > 0 && mhz < 3000);
  // What a screen reader hears: once per change, not every second.
  const btSpoken = $derived(
    !set?.bluetooth || !bt
      ? ""
      : pairing
        ? `Ready to pair as “${bt.name}”.`
        : btReady
          ? `Ready as “${bt.name}”. ${phones === 1 ? "One phone" : `${phones} phones`} paired.`
          : btFailed
            ? "Bluetooth isn't running."
            : "Bluetooth is starting.",
  );
  // systemd's results (exit-code, signal, …) mean nothing to people.
  const SYSTEMD = ["exit-code", "signal", "core-dump", "watchdog", "start-limit-hit", "timeout", "resources", "systemctl"];
  function btReason(r: string): string {
    if (!r || SYSTEMD.includes(r)) return "Bluetooth stopped unexpectedly.";
    const t = r.charAt(0).toUpperCase() + r.slice(1);
    return /[.!?]$/.test(t) ? t : `${t}.`;
  }
  // Two quick presses would open the window and close it again.
  let lastPress = 0;
  function pressPairing() {
    const now = Date.now();
    if (device.busy || now - lastPress < 1000) return;
    lastPress = now;
    void (pairing ? device.stopBluetoothPairing() : device.startBluetoothPairing());
  }
  const ss = $derived(s?.sendspin);
  const ssUnavailable = $derived(ss?.state === SendspinState.UNAVAILABLE);
  const ssHint = $derived(
    ssUnavailable
      ? "This satellite has no working speaker, so it can't play from Music Assistant. Logs, under System, says why."
      : "Play from Music Assistant 2.10.7 or later, in step with the other speakers in a group. While it is on, anyone on your network can play to it, and replace what it is playing.",
  );
  // Without a hardware clock to follow, it plays, but not in step (ADR 0027 §12).
  const ssNotInStep = $derived(
    !!ss &&
      !ss.exactTiming &&
      (ss.state === SendspinState.READY || ss.state === SendspinState.CONNECTED || ss.state === SendspinState.PLAYING),
  );
  // pinnad's reasons ("port 8928 is in use") start in lower case, without a full stop.
  function sentence(r: string): string {
    const t = r.charAt(0).toUpperCase() + r.slice(1);
    return /[.!?]$/.test(t) ? t : `${t}.`;
  }
  // Play a URL, as Home Assistant's "Play media" does.
  let url = $state("");
  let urlError = $state<string | null>(null);
  const canPlay = $derived(!!s?.media?.available);
  // The URL last sent, and what became of it: the play starts after the reply.
  let sent = $state<string | null>(null);
  const media = $derived(s?.media);
  const sentPlays = $derived(
    !!sent && media?.url === sent && (media.state === MediaState.PLAYING || media.state === MediaState.PAUSED),
  );
  // A play that fails leaves what played before playing (ADR 0011).
  const sentFailed = $derived(!!sent && media?.errorUrl === sent && media.error ? media.error : null);
  async function play() {
    const u = url.trim();
    urlError = await device.playMedia(u);
    sent = urlError ? null : u;
  }
  let forgetting = $state<{ address: string; name: string } | null>(null);
  let forgetOpen = $state(false);
  async function forget() {
    if (!forgetting) return;
    const { address, name } = forgetting;
    await device.forgetBluetoothDevice(address, name);
    // Its row is gone: keep the focus on the page.
    document.getElementById("bt-pair")?.focus();
  }
</script>

{#if s && set}
  <Group title="AirPlay">
    {#snippet foot()}
      It plays on its own: it can't join a group with HomePods or other AirPlay 2 speakers. The phone or computer sets
      the music's volume; Volume, on Voice &amp; sound, sets how loud the satellite is overall. AirPlay is also a
      switch in Home Assistant, on this satellite's device page.
    {/snippet}
    <SwitchRow
      id="airplay"
      label="AirPlay"
      checked={set.airplay}
      disabled={!live || unavailable}
      busy={device.busy}
      {hint}
      toggle={(on) => device.updateSettings({ airplay: on })}
    />
    {#if set.airplay && airplay}
      <div class="pn-row" aria-live="polite">
        <div class="pn-row-text">
          {#if airplay.state === AirPlayState.AIRPLAY_STATE_READY}
            <span class="pn-row-label">Ready as “{airplay.name}”</span>
            <span class="pn-row-hint">
              On an iPhone or Mac, open AirPlay and choose “{airplay.name}”. Not in the list? The phone has to be on
              the same network as the satellite: a guest Wi-Fi or a separate VLAN hides it.
            </span>
          {:else if airplay.state === AirPlayState.AIRPLAY_STATE_FAILED}
            <span class="pn-row-label">Not running</span>
            <span class="pn-row-hint is-danger">AirPlay didn't start. Logs, under System, says why.</span>
          {:else}
            <span class="pn-row-label">Starting…</span>
            <span class="pn-row-hint">The receiver takes a few seconds to start.</span>
          {/if}
        </div>
      </div>
    {/if}
  </Group>

  <Group title="Bluetooth">
    {#snippet foot()}
      The phone sets the music's volume; Volume, on Voice &amp; sound, sets how loud the satellite is overall. Bluetooth
      audio and Bluetooth pairing are also switches in Home Assistant, on this satellite's device page.
    {/snippet}
    <SwitchRow
      id="bluetooth"
      label="Bluetooth"
      checked={set.bluetooth}
      disabled={!live || btUnavailable}
      busy={device.busy}
      hint={btHint}
      toggle={(on) => device.updateSettings({ bluetooth: on })}
    />
    <p class="sr" role="status">{btSpoken}</p>
    {#if set.bluetooth && bt}
      <div class="pn-row">
        <div class="pn-row-text">
          {#if pairing}
            <span class="pn-row-label">Ready to pair as “{bt.name}”</span>
            <span id="bt-hint" class="pn-row-hint">
              On the phone, open Bluetooth settings and choose “{bt.name}”. Open for {mmss(pairingLeft)} more, or until a
              phone pairs. Anyone nearby can pair while it is open.
            </span>
          {:else if btReady}
            <span class="pn-row-label">Ready as “{bt.name}”</span>
            <span id="bt-hint" class="pn-row-hint">
              {#if phones === 0}
                No phone paired yet. Phones can't see “{bt.name}” until you select Pair a phone.
              {:else}
                A paired phone connects from its Bluetooth settings. To add another, select Pair a phone.
              {/if}
            </span>
          {:else if btFailed}
            <span class="pn-row-label">Not running</span>
            <span id="bt-hint" class="pn-row-hint is-danger">{btReason(bt.reason)} Logs, under System, shows why.</span>
          {:else}
            <span class="pn-row-label">Starting…</span>
            <span class="pn-row-hint">Bluetooth takes a few seconds to start.</span>
          {/if}
        </div>
        {#if btReady}
          <!-- aria-disabled, not disabled: disabling the pressed button would drop the focus. -->
          <button
            id="bt-pair"
            class="pn-btn"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            aria-describedby="bt-hint"
            onclick={pressPairing}>{pairing ? "Stop pairing" : "Pair a phone"}</button
          >
        {:else if btFailed}
          <button
            class="pn-btn"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            aria-describedby="bt-hint"
            onclick={() => {
              if (!device.busy) void device.updateSettings({ bluetooth: true });
            }}>Try again</button
          >
        {/if}
      </div>
      {#if onTwoFour}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">2.4 GHz Wi-Fi</span>
            <span class="pn-row-hint">
              Bluetooth shares the satellite's antenna with the Wi-Fi: while music plays over Bluetooth, voice replies
              start a little later and updates can stall. A 5 GHz Wi-Fi avoids that.
            </span>
          </div>
        </div>
      {/if}
      {#each bt.devices as d (d.address)}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{d.name}</span>
            <span class="pn-row-hint">{d.connected ? "Connected" : "Paired"}</span>
          </div>
          <button
            class="pn-btn pn-btn-quiet pn-btn-sm"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            onclick={() => {
              if (device.busy) return;
              forgetting = { address: d.address, name: d.name };
              forgetOpen = true;
            }}
            aria-label="Forget {d.name}"
          >
            Forget
          </button>
        </div>
      {/each}
    {/if}
  </Group>

  <Group title="Sendspin">
    {#snippet aside()}Experimental{/snippet}
    {#snippet foot()}
      It stays experimental while the Sendspin specification is a draft. Music Assistant connects without pairing, so
      someone who can intercept your network's traffic could read or change the music. Music Assistant sets the music's
      volume; Volume, on Voice &amp; sound, sets how loud the satellite is overall. Sendspin is also a switch in Home
      Assistant, on this satellite's device page.
    {/snippet}
    <SwitchRow
      id="sendspin"
      label="Sendspin"
      checked={set.sendspin}
      disabled={!live || ssUnavailable}
      busy={device.busy}
      hint={ssHint}
      toggle={(on) => device.updateSettings({ sendspin: on })}
    />
    {#if set.sendspin && ss}
      <div class="pn-row" aria-live="polite">
        <div class="pn-row-text">
          {#if ss.state === SendspinState.READY}
            <span class="pn-row-label">Ready as “{ss.name}”</span>
            {#if ss.reason}
              <span class="pn-row-hint">Music Assistant lists it as “{ss.name}”.</span>
              <span class="pn-row-hint is-danger">A server couldn't connect. {sentence(ss.reason)}</span>
            {:else}
              <span class="pn-row-hint">
                Music Assistant lists it as “{ss.name}”. Not in the list? Music Assistant has to be on the same network
                as the satellite: a guest Wi-Fi or a separate VLAN hides it.
              </span>
            {/if}
          {:else if ss.state === SendspinState.CONNECTED}
            <span class="pn-row-label">Connected to {ss.server || "Music Assistant"}</span>
            <span class="pn-row-hint">Nothing plays. In Music Assistant, play to “{ss.name}”.</span>
          {:else if ss.state === SendspinState.PLAYING}
            <span class="pn-row-label">Playing</span>
            <span class="pn-row-hint">
              From {ss.server || "Music Assistant"}. Now playing, on the Overview, shows what plays.
            </span>
          {:else if ss.state === SendspinState.FAILED}
            <span class="pn-row-label">Not running</span>
            <span class="pn-row-hint is-danger">
              {ss.reason ? sentence(ss.reason) : "Sendspin didn't start."} Logs, under System, says more.
            </span>
          {:else}
            <span class="pn-row-label">Starting…</span>
            <span class="pn-row-hint">The player takes a moment to start.</span>
          {/if}
        </div>
      </div>
      {#if ssNotInStep}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">Not in step</span>
            <span class="pn-row-hint">
              It plays, but not in step with the other speakers in a group: on a virtual device or a computer, there is
              no hardware clock to follow.
            </span>
          </div>
        </div>
      {/if}
    {/if}
  </Group>
  <Group title="Play a URL">
    {#snippet foot()}
      It plays like music from Home Assistant, and stops what was playing. Volume, on Voice &amp; sound, sets how loud;
      the Overview shows what plays.
    {/snippet}
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void play();
      }}
    >
      <div class="pn-field" data-invalid={urlError ? "" : undefined}>
        <label class="pn-field-label" for="media-url">URL</label>
        <div class="inline">
          <input
            id="media-url"
            class="pn-input is-mono"
            type="text"
            inputmode="url"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            placeholder="https://example.org/stream.mp3"
            bind:value={url}
            disabled={!live || !canPlay}
            aria-describedby="media-url-hint"
            aria-invalid={urlError ? "true" : undefined}
            oninput={() => {
              urlError = null;
              sent = null;
            }}
          />
          <button class="pn-btn pn-btn-primary" type="submit" disabled={!live || !canPlay || device.busy || !url.trim()}>
            Play
          </button>
        </div>
        {#if urlError}<span class="pn-field-error" role="alert"><Icon name="alert" />{urlError}</span>
        {:else if sentFailed}<span class="pn-field-error" role="alert"
            ><Icon name="alert" />It didn't play: {sentFailed}</span
          >
        {:else if sentPlays}<span class="pn-field-hint" role="status"
            >Playing. <b>Now playing</b>, on the Overview, pauses and stops it.</span
          >{/if}
        <span id="media-url-hint" class="pn-field-hint">
          {canPlay
            ? "MP3, FLAC, Ogg or WAV over http or https. AAC, HLS and playlist files (.m3u, .pls) don't play."
            : "This satellite has no working speaker, so it can't play media. Logs, under System, says why."}
        </span>
      </div>
    </form>
  </Group>
  <Confirm bind:open={forgetOpen} title="Forget {forgetting?.name}?" action="Forget" danger confirm={forget}>
    <p>
      It can't connect or play any more. To pair it again, also remove “{bt?.name}” from its Bluetooth settings, then
      select Pair a phone.
    </p>
  </Confirm>
{:else}
  <span class="pn-skeleton" style:height="120px"></span>
{/if}

<style>
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
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
  .is-danger {
    color: var(--danger);
    font-weight: 650;
  }
</style>
