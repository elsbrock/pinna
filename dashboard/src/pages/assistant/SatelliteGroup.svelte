<!-- Assistant › Satellite group (ADR 0026): the switch, the members, how this
     satellite takes part, and the satellites the search found, with Add and
     Remove. Shown in both modes: a member in Home Assistant mode takes
     messages and control, except timers, and sends nothing. Read every 30 s
     while it shows, and after each change. -->
<script lang="ts">
  import { tick } from "svelte";

  import { isStandalone } from "../../lib/assistant.ts";
  import Confirm from "../../lib/Confirm.svelte";
  import { device, errorSentence } from "../../lib/device.svelte.ts";
  import External from "../../lib/External.svelte";
  import Group from "../../lib/Group.svelte";
  import { satelliteGroup as group } from "../../lib/group.svelte.ts";
  import {
    candidateOffer,
    groupLists,
    keyed,
    memberLine,
    noCandidates,
    ownName,
    refusalElsewhere,
    rejoinOffer,
    removedNote,
    removeEndsGroup,
    rowButton,
    sameMac,
    type Search,
  } from "../../lib/group.ts";
  import Icon from "../../lib/Icon.svelte";
  import SwitchRow from "../../lib/SwitchRow.svelte";
  import { satelliteLink, windowLeft } from "../../lib/view.ts";
  import { windows } from "../../lib/windows.svelte.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const view = $derived(group.view);
  const standalone = $derived(isStandalone(s));
  const inGroup = $derived(!!view?.on && view.inGroup);
  // A member outside the group shows once, under Members.
  const lists = $derived(groupLists(view ?? { members: [], candidates: [] }));
  const candidates = $derived(lists.candidates);
  const memberRows = $derived(keyed(lists.members, (row) => row.member.mac.toLowerCase()));
  const candidateRows = $derived(
    keyed(candidates, (c) => (c.satellite?.mac || c.satellite?.name || "").toLowerCase()),
  );
  const others = $derived((view?.members ?? []).filter((m) => !m.thisSatellite));
  const selfName = $derived(ownName(view ?? { members: [] }));
  const refused = $derived(group.refused);

  $effect(() => group.watch());
  $effect(() => windows.watch());
  // With a password, another satellite may add this one only after the owner allows it
  // here (ADR 0029). It is added from the other satellite, and only while in no group.
  const joinLeft = $derived(windows.groupJoinLeft);
  // Back after the satellite couldn't be reached: read it again at once.
  let wasLive = device.link === "live";
  $effect(() => {
    if (live && !wasLive) void group.load();
    wasLive = live;
  });

  const here = { protocol: location.protocol, hostname: location.hostname };

  // Turning the switch off in a group leaves it: that asks first.
  let leaving = $state(false);
  function toggleOn(on: boolean) {
    if (!on && inGroup) leaving = true;
    else void group.setOn(on);
  }

  // The member Remove asked about; it goes once the dialog confirms.
  let removing = $state({ mac: "", name: "", ends: false });
  let removeOpen = $state(false);
  function askRemove(mac: string, name: string) {
    if (group.running || !view) return;
    removing = { mac, name, ends: removeEndsGroup(view, mac) };
    removeOpen = true;
  }

  // Add and Remove take away the row whose button had focus. Unless the
  // owner moved on meanwhile, focus goes where the change shows: the new
  // member's Remove, or the Members title (Satellite group's once the group
  // ended).
  let groupCard = $state<ReturnType<typeof Group>>();
  let membersCard = $state<ReturnType<typeof Group>>();
  const focusLost = () => !document.activeElement || document.activeElement === document.body;
  async function add(mac: string, name: string) {
    if (!(await group.add(mac, name))) return;
    await tick();
    if (!focusLost()) return;
    const buttons = document.querySelectorAll<HTMLButtonElement>("button[data-remove]");
    [...buttons].find((b) => sameMac(b.dataset.remove ?? "", mac))?.focus();
  }
  async function remove(mac: string, name: string) {
    if (!(await group.remove(mac, name))) return;
    await tick();
    if (focusLost()) (membersCard ?? groupCard)?.focusTitle();
  }

  /** The refusal of an Add or Remove of `mac`. */
  const refusedFor = (kind: "add" | "remove", mac: string) =>
    refused?.change.kind === kind && sameMac(refused.change.mac, mac) ? refused.sentence : null;
  /** A refusal whose row or card the view read after it took away. */
  const refusedElsewhere = $derived(view ? refusalElsewhere(refused, view) : null);

  // With no satellite listed, the search says why, as the satellite switcher
  // reads it: read again with each view that lists none.
  let search = $state<Search>({ state: "looking" });
  const none = $derived(noCandidates(search, inGroup, group.removed?.name ?? null));
  /** Whether `mac` is the member this page just removed. */
  const justRemoved = (mac: string) => !!group.removed && sameMac(group.removed.mac, mac);
  // The member just removed, while the search doesn't list it yet and others
  // are listed: an empty list says so in its own line.
  const removedAbsent = $derived(
    group.removed && candidates.length && !candidates.some((c) => justRemoved(c.satellite?.mac ?? ""))
      ? removedNote(group.removed.name)
      : null,
  );
  $effect(() => {
    if (!view?.on || candidates.length) return;
    let current = true;
    let waited = false;
    let again: ReturnType<typeof setTimeout> | undefined;
    const look = () =>
      device.listSatellites().then(
        (r) => {
          if (!current) return;
          // pinnad has only just started: ask again until its first search
          // is in, then read the group again for what it found.
          if (!r.searched) {
            search = { state: "looking" };
            waited = true;
            again = setTimeout(look, 1000);
            return;
          }
          search = { state: "done", onNetwork: r.onNetwork };
          if (waited) void group.load();
        },
        (e) => {
          if (current) search = { state: "failed", why: errorSentence(e) };
        },
      );
    void look();
    return () => {
      current = false;
      clearTimeout(again);
    };
  });
</script>

{#if view && s}
  {#snippet homeAssistantFoot()}
    In Home Assistant mode, this satellite takes messages and control from the other members, but not timers: Home
    Assistant sets them. It sends none, because Home Assistant answers its requests.
  {/snippet}
  <Group
    bind:this={groupCard}
    title="Satellite group"
    foot={view.on && !standalone ? homeAssistantFoot : undefined}
  >
    {#if refusedElsewhere}
      <div class="pn-row">
        <span class="pn-field-error" role="alert"><Icon name="alert" />{refusedElsewhere}</span>
      </div>
    {/if}
    <SwitchRow
      id="group-on"
      label="Satellite group"
      checked={view.on}
      disabled={!live}
      busy={!!group.running}
      hint="Lets this satellite join a group of satellites that pass messages to each other, such as “tell the kitchen that dinner is ready”."
      toggle={toggleOn}
    />
    {#if view.on && !view.inGroup}
      <div class="pn-row">
        <span class="pn-row-hint">
          In no group yet. Add a satellite below to start one, or add this one from another satellite's dashboard.
        </span>
      </div>
    {/if}
    {#if view.on && !view.inGroup && windows.passwordSet}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Allow adding to a group</span>
          <span class="pn-row-hint" role="status">
            {#if joinLeft}
              Another satellite can add this one now. {windowLeft(joinLeft)}.
            {:else}
              This satellite has a dashboard password, so it refuses to be added to a group until you allow it here. Allow
              it, then add this satellite from another satellite's dashboard within 10 minutes.
            {/if}
          </span>
          {#if windows.error}
            <span class="pn-field-error" role="alert"><Icon name="alert" />{windows.error}</span>
          {/if}
        </div>
        <button class="pn-btn" disabled={!live || windows.opening} onclick={() => windows.allowGroupJoin()}>
          Allow adding to a group
        </button>
      </div>
    {/if}
    {#if group.running?.kind === "on" && inGroup}
      <div class="pn-row">
        <span class="pn-row-hint" role="status">Leaving the group. The other members are told first.</span>
      </div>
    {/if}
    {#if refused?.change.kind === "on"}
      <div class="pn-row">
        <span class="pn-field-error" role="alert"><Icon name="alert" />{refused.sentence}</span>
      </div>
    {/if}
  </Group>

  {#if inGroup}
    <Group bind:this={membersCard} title="Members">
      {#snippet aside()}{lists.count} {lists.count === 1 ? "satellite" : "satellites"}{/snippet}
      {#snippet foot()}
        A member in Home Assistant mode takes messages and control, but not timers, and sends none.
      {/snippet}
      {#each memberRows as { key, item: { member: m, outside } } (key)}
        {@const name = m.name || m.mac}
        {@const button = rowButton(live, group.running, "remove", m.mac)}
        {@const again = outside ? rejoinOffer(outside) : null}
        {@const addButton = rowButton(live, group.running, "add", m.mac)}
        {@const why = refusedFor("remove", m.mac) ?? refusedFor("add", m.mac)}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{name}</span>
            <span class="pn-row-hint" role={addButton.working ? "status" : undefined}>
              {#if addButton.working}
                Adding it to the group. This can take up to 15 seconds.
              {:else}
                {memberLine(m, s.mode, selfName, device.now, outside?.state)}
              {/if}
            </span>
            {#if again?.text || again?.link}
              <span class="pn-row-hint">
                {again.text}
                {#if again.link && outside?.satellite}
                  <External href={satelliteLink(outside.satellite, here).href}>Open its dashboard</External>
                {/if}
              </span>
            {/if}
            {#if why}<span class="pn-field-error" role="alert"><Icon name="alert" />{why}</span>{/if}
          </div>
          {#if m.thisSatellite}
            <span class="pn-sat-here"><Icon name="check" />This satellite</span>
          {:else}
            <div class="actions">
              {#if again?.add}
                <button
                  class="pn-btn pn-btn-sm"
                  type="button"
                  disabled={addButton.disabled}
                  aria-disabled={addButton.working || undefined}
                  aria-label={addButton.working ? `Adding ${name} again` : `Add ${name} again`}
                  onclick={() => {
                    if (!addButton.working) void add(m.mac, name);
                  }}>{addButton.working ? "Adding…" : "Add again"}</button
                >
              {/if}
              <button
                class="pn-btn pn-btn-quiet pn-btn-sm"
                type="button"
                data-remove={m.mac}
                disabled={button.disabled}
                aria-disabled={button.working || undefined}
                aria-label={button.working ? `Removing ${name}` : `Remove ${name}`}
                onclick={() => {
                  if (!button.working) askRemove(m.mac, name);
                }}>{button.working ? "Removing…" : "Remove"}</button
              >
            </div>
          {/if}
        </div>
      {/each}
    </Group>
  {/if}

  {#if view.on}
    <Group title="From the group">
      <SwitchRow
        id="group-messages"
        label="Group messages"
        checked={s.settings?.groupMessages ?? true}
        disabled={!live}
        busy={device.busy}
        hint="Says the messages the other members send. Off, new messages are refused, and the ones held still play. Home Assistant has this switch too."
        toggle={(on) => device.updateSettings({ groupMessages: on })}
      />
      <SwitchRow
        id="group-control"
        label="Control from the group"
        checked={view.control}
        disabled={!live}
        busy={!!group.running}
        hint={standalone
          ? "Lets the other members' brains control this satellite, such as its volume, music and timers."
          : "Lets the other members' brains control this satellite, such as its volume and music. Its timers are Home Assistant's."}
        toggle={(on) => group.setControl(on)}
      />
      {#if refused?.change.kind === "control"}
        <div class="pn-row">
          <span class="pn-field-error" role="alert"><Icon name="alert" />{refused.sentence}</span>
        </div>
      {/if}
    </Group>

    <Group title="Other satellites">
      {#snippet foot()}
        A satellite can be added while its Satellite group switch is on and it is in no group. The search looks for
        satellites on the same subnet about once a minute.
      {/snippet}
      {#if removedAbsent}
        <div class="pn-row"><span class="pn-row-hint">{removedAbsent}</span></div>
      {/if}
      {#each candidateRows as { key, item: c } (key)}
        {@const sat = c.satellite!}
        {@const name = sat.friendlyName || sat.name}
        {@const offer = candidateOffer(c, justRemoved(sat.mac))}
        {@const button = rowButton(live, group.running, "add", sat.mac)}
        {@const why = refusedFor("add", sat.mac)}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{name}</span>
            <span class="pn-row-hint" role={button.working ? "status" : undefined}>
              {#if button.working}
                Adding it to the group. This can take up to 15 seconds.
              {:else}
                {offer.text}
                {#if offer.link}<External href={satelliteLink(sat, here).href}>Open its dashboard</External>{/if}
              {/if}
            </span>
            {#if why}<span class="pn-field-error" role="alert"><Icon name="alert" />{why}</span>{/if}
          </div>
          {#if offer.add}
            <button
              class="pn-btn pn-btn-sm"
              type="button"
              disabled={button.disabled}
              aria-disabled={button.working || undefined}
              aria-label={button.working ? `Adding ${name}` : `Add ${name}`}
              onclick={() => {
                if (!button.working) void add(sat.mac, name);
              }}>{button.working ? "Adding…" : "Add"}</button
            >
          {/if}
        </div>
      {:else}
        <div class="pn-row">
          {#if none.tone === "alert"}
            <span class="pn-field-error" role="alert"><Icon name="alert" />{none.text}</span>
          {:else}
            <span class="pn-row-hint" role={none.tone === "status" ? "status" : undefined}>{none.text}</span>
          {/if}
        </div>
      {/each}
    </Group>
  {/if}

  <Confirm
    bind:open={removeOpen}
    title="Remove {removing.name} from the group?"
    action="Remove"
    danger
    confirm={() => remove(removing.mac, removing.name)}
  >
    <p>
      {removing.name} leaves the group and takes no more messages from it. Every member is told, {removing.name} too.
    </p>
    {#if removing.ends}<p>This satellite is then the only member, so the group ends.</p>{/if}
    <p>
      To add it again, select Add next to it under Other satellites. It shows there again after the next search, within
      about a minute.
    </p>
  </Confirm>

  <Confirm
    bind:open={leaving}
    title="Turn Satellite group off?"
    action="Turn off"
    danger
    confirm={() => group.setOn(false)}
  >
    <p>This satellite leaves its group; the other members are told. The messages it holds are dropped.</p>
    {#if others.length === 1}
      {@const other = others[0].name || others[0].mac}
      <p>{other} is then the only member, so the group ends.</p>
      <p>To join again, turn Satellite group back on, then select Add next to this satellite on {other}'s dashboard.</p>
    {:else}
      <p>To join again, turn Satellite group back on, then select Add next to this satellite on a member's dashboard.</p>
    {/if}
  </Confirm>
{:else if group.error}
  <div class="pn-notice" data-tone="danger" role="alert">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">The satellite group couldn't be read</p>
      <p>{group.error}</p>
      <div><button class="pn-btn" type="button" onclick={() => group.load()}>Try again</button></div>
    </div>
  </div>
{:else}
  <span class="pn-skeleton" style:height="72px" aria-hidden="true"></span>
{/if}

<style>
  /* Add again and Remove side by side; on a narrow row, one under the other. */
  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
