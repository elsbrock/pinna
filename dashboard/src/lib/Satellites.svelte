<!-- The satellite switcher (ADR 0021): this satellite, then the others on the
     network, each a link to its own dashboard. pinnad looks for them in the
     background; opening the list reads what it last found, without asking the
     network. It floats over the page, placed from the button it opened from,
     which gets the focus back on Escape and on a choice. -->
<script lang="ts">
  import { onMount } from "svelte";

  import type { Satellite } from "../gen/pinna/v1/device_pb.ts";
  import Device from "./Device.svelte";
  import { device, errorSentence } from "./device.svelte.ts";
  import Icon from "./Icon.svelte";
  import { displayName, satelliteFallbacks, satelliteLink, satelliteModel } from "./view.ts";

  let {
    anchor,
    placement,
    overview = false,
    onclose,
  }: {
    /** The button that opened it. */
    anchor: HTMLElement;
    /** Above the button (the sidebar's, at the bottom) or below it (the app bar's). */
    placement: "up" | "down";
    /** Offer a link to the overview: on phones the name that opens this was that link. */
    overview?: boolean;
    onclose: () => void;
  } = $props();

  type Found =
    | { state: "looking" }
    | { state: "done"; list: Satellite[]; onNetwork: boolean }
    | { state: "failed"; why: string };

  const s = $derived(device.status);
  let found = $state<Found>({ state: "looking" });
  let pop = $state<HTMLElement>();
  let place = $state("");

  function close(restoreFocus: boolean) {
    if (restoreFocus) anchor.focus();
    onclose();
  }

  function position() {
    const r = anchor.getBoundingClientRect();
    const width = Math.min(340, innerWidth - 16);
    const left = Math.max(8, Math.min(r.left, innerWidth - width - 8));
    place =
      placement === "up"
        ? `left:${left}px;bottom:${innerHeight - r.top + 8}px;max-height:${Math.max(160, r.top - 16)}px`
        : `left:${left}px;top:${r.bottom + 8}px;max-height:${Math.max(160, innerHeight - r.bottom - 16)}px`;
  }

  let current = true;
  let again: ReturnType<typeof setTimeout> | undefined;
  function look() {
    found = { state: "looking" };
    device
      .listSatellites()
      .then((r) => {
        if (!current) return;
        // pinnad has only just started: ask again until its first search is in.
        if (!r.searched) again = setTimeout(look, 1000);
        else found = { state: "done", list: r.satellites, onNetwork: r.onNetwork };
      })
      .catch((e) => current && (found = { state: "failed", why: errorSentence(e) }));
  }

  onMount(() => {
    position();
    pop?.focus();
    look();
    const outside = (e: PointerEvent) => {
      const t = e.target as Node;
      if (!pop?.contains(t) && !anchor.contains(t)) close(false);
    };
    const escape = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      close(true);
    };
    // A resize moves the button; another page (Back, a link) leaves it behind.
    const away = () => close(false);
    addEventListener("pointerdown", outside, true);
    addEventListener("keydown", escape);
    addEventListener("resize", away);
    addEventListener("hashchange", away);
    return () => {
      current = false;
      clearTimeout(again);
      removeEventListener("pointerdown", outside, true);
      removeEventListener("keydown", escape);
      removeEventListener("resize", away);
      removeEventListener("hashchange", away);
    };
  });

  // Focus moving out of it closes it; Tab past either end goes back to the
  // button, which follows the popover in the tab order. The arrow keys move
  // between the links.
  function leaving(e: FocusEvent) {
    const to = e.relatedTarget as Node | null;
    if (to && !pop?.contains(to) && !anchor.contains(to)) close(false);
  }
  function keys(e: KeyboardEvent) {
    if (!pop) return;
    if (e.key === "Tab") {
      const stops = [...pop.querySelectorAll<HTMLElement>("a, button")];
      const at = stops.indexOf(document.activeElement as HTMLElement);
      if (e.shiftKey ? at <= 0 : at === stops.length - 1) {
        e.preventDefault();
        close(true);
      }
      return;
    }
    const moves = ["ArrowDown", "ArrowUp", "Home", "End"];
    if (!moves.includes(e.key)) return;
    const links = [...pop.querySelectorAll<HTMLElement>("a")];
    if (!links.length) return;
    e.preventDefault();
    const at = links.indexOf(document.activeElement as HTMLElement);
    const next =
      e.key === "Home" ? 0 : e.key === "End" ? links.length - 1 : (at + (e.key === "ArrowDown" ? 1 : -1) + links.length) % links.length;
    links[next].focus();
  }

  const here = { protocol: location.protocol, hostname: location.hostname };
</script>

<div
  bind:this={pop}
  class="pn-pop"
  role="dialog"
  aria-label="Satellites"
  tabindex="-1"
  style={place}
  onkeydown={keys}
  onfocusout={leaving}
>
  {#if s}
    <div class="pn-sat" aria-current="true">
      <span class="pn-sat-pic" aria-hidden="true"><Device model={s.model} effect="idle" width={48} label="" /></span>
      <span class="pn-sat-text">
        <span class="pn-sat-name">{displayName(s)}</span>
        <span class="pn-sat-here"><Icon name="check" />This satellite</span>
      </span>
    </div>
  {/if}

  {#if found.state === "looking"}
    <p class="pn-pop-note" role="status">Looking for other satellites…</p>
    {#each [0, 1] as k (k)}<span class="pn-skeleton" style:height="56px" aria-hidden="true"></span>{/each}
  {:else if found.state === "failed"}
    <p class="pn-pop-note" role="alert"><Icon name="alert" />{found.why}</p>
  {:else if !found.onNetwork}
    <p class="pn-pop-note">This satellite isn't on a network yet.</p>
  {:else if !found.list.length}
    <p class="pn-pop-note">No other satellites on this network.</p>
  {:else}
    {#each found.list as sat (sat.mac || sat.name)}
      {@const to = satelliteLink(sat, here)}
      <div>
        <a class="pn-sat" href={to.href} onclick={() => close(true)}>
          <span class="pn-sat-pic" aria-hidden="true"><Device model={satelliteModel(sat.board)} effect="idle" width={48} label="" /></span>
          <span class="pn-sat-text">
            <span class="pn-sat-name">{sat.friendlyName || sat.name}</span>
            <span class="pn-sat-host">{to.host}</span>
          </span>
        </a>
        {#if sat.url}
          <p class="pn-sat-alt">
            or
            {#each satelliteFallbacks(sat) as alt, i (alt.href)}
              {#if i}&nbsp;·&nbsp;{/if}<a href={alt.href} onclick={() => close(true)}>{alt.host}</a>
            {/each}
          </p>
        {/if}
      </div>
    {/each}
  {/if}

  {#if overview}
    <hr class="pn-pop-sep" />
    <a class="pn-sat" href="#/" onclick={() => close(true)}>
      <span class="pn-sat-pic" aria-hidden="true"><Icon name="overview" /></span>
      <span class="pn-sat-name">Overview</span>
    </a>
  {/if}
</div>
