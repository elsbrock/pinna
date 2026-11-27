<!-- The satellite switcher (ADR 0021): the other satellites on the network,
     each by its drawing and name, a link to its own dashboard. pinnad looks
     for them in the background; opening the list reads what it last found,
     without asking the network. From the sidebar, the device box opens into
     the list: it covers the box, with this satellite's row where the box was
     and the others above it. From the app bar, it drops below the name, with
     this satellite first. The button it opened from gets the focus back on
     Escape and on a choice. -->
<script lang="ts">
  import { onMount, type Snippet } from "svelte";

  import type { Satellite } from "../gen/pinna/v1/device_pb.ts";
  import Device from "./Device.svelte";
  import { device, errorSentence } from "./device.svelte.ts";
  import Icon from "./Icon.svelte";
  import { displayName, satelliteLink, satelliteModel } from "./view.ts";

  let {
    anchor,
    placement,
    current,
    overview = false,
    onclose,
  }: {
    /** The button that opened it. */
    anchor: HTMLElement;
    /** Over the sidebar's device box, growing upward from it, or below the app bar's name. */
    placement: "box" | "down";
    /** This satellite's row in the box: what the device box shows. */
    current?: Snippet;
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
    if (placement === "box") {
      // The box's own place and width, from its bottom edge up.
      place = `left:${r.left}px;width:${r.width}px;bottom:${innerHeight - r.bottom}px;max-height:${Math.max(160, r.bottom - 16)}px`;
      return;
    }
    const width = Math.min(340, innerWidth - 16);
    const left = Math.max(8, Math.min(r.left, innerWidth - width - 8));
    place = `left:${left}px;top:${r.bottom + 8}px;max-height:${Math.max(160, innerHeight - r.bottom - 16)}px`;
  }

  // Out of the device box, the list grows from the box's height to its own,
  // and again whenever its height changes (the search's answer replaces the
  // placeholders): its edges and shadow move as the box's do. While it
  // grows, its rows stay at the bottom with this satellite's.
  let growing = $state(false);
  let shown = 0;
  $effect(() => {
    void found;
    if (placement === "box" && pop) grow(pop);
  });
  function grow(el: HTMLElement) {
    const running = el.getAnimations();
    const from = running.length ? el.offsetHeight : shown || anchor.offsetHeight;
    for (const a of running) a.cancel();
    const to = el.offsetHeight;
    shown = to;
    if (from === to || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    growing = true;
    el.animate([{ height: `${from}px` }, { height: `${to}px` }], { duration: 160, easing: "ease-out" }).finished.then(
      () => (growing = false),
      () => {},
    );
  }

  let alive = true;
  let again: ReturnType<typeof setTimeout> | undefined;
  function look() {
    found = { state: "looking" };
    device
      .listSatellites()
      .then((r) => {
        if (!alive) return;
        // pinnad has only just started: ask again until its first search is in.
        if (!r.searched) again = setTimeout(look, 1000);
        else found = { state: "done", list: r.satellites, onNetwork: r.onNetwork };
      })
      .catch((e) => alive && (found = { state: "failed", why: errorSentence(e) }));
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
      alive = false;
      clearTimeout(again);
      removeEventListener("pointerdown", outside, true);
      removeEventListener("keydown", escape);
      removeEventListener("resize", away);
      removeEventListener("hashchange", away);
    };
  });

  // Focus moving out of it closes it; Tab past either end goes back to the
  // button, which follows the popover in the tab order. The arrow keys move
  // between the rows.
  function leaving(e: FocusEvent) {
    const to = e.relatedTarget as Node | null;
    if (to && !pop?.contains(to) && !anchor.contains(to)) close(false);
  }
  function keys(e: KeyboardEvent) {
    if (!pop) return;
    const stops = [...pop.querySelectorAll<HTMLElement>("a, button")];
    if (e.key === "Tab") {
      const at = stops.indexOf(document.activeElement as HTMLElement);
      if (e.shiftKey ? at <= 0 : at === stops.length - 1) {
        e.preventDefault();
        close(true);
      }
      return;
    }
    const moves = ["ArrowDown", "ArrowUp", "Home", "End"];
    if (!moves.includes(e.key) || !stops.length) return;
    e.preventDefault();
    const at = stops.indexOf(document.activeElement as HTMLElement);
    const next =
      e.key === "Home" ? 0 : e.key === "End" ? stops.length - 1 : (at + (e.key === "ArrowDown" ? 1 : -1) + stops.length) % stops.length;
    stops[next].focus();
  }

  const here = { protocol: location.protocol, hostname: location.hostname };
</script>

<div
  bind:this={pop}
  class="pn-pop"
  class:pn-pop-box={placement === "box"}
  class:is-growing={growing}
  role="dialog"
  aria-label="Satellites"
  tabindex="-1"
  style={place}
  onkeydown={keys}
  onfocusout={leaving}
>
  {#if s && placement === "down"}
    <div class="pn-sat" aria-current="true">
      <span class="pn-sat-pic" aria-hidden="true"><Device model={s.model} effect="idle" width={48} label="" /></span>
      <span class="pn-sat-name">{displayName(s)}<span class="sr">, this satellite</span></span>
      <Icon name="check" class="pn-icon pn-sat-check" />
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
      <a class="pn-sat" href={satelliteLink(sat, here).href} onclick={() => close(true)}>
        <span class="pn-sat-pic" aria-hidden="true"><Device model={satelliteModel(sat.board)} effect="idle" width={48} label="" /></span>
        <span class="pn-sat-name">{sat.friendlyName || sat.name}</span>
      </a>
    {/each}
  {/if}

  {#if placement === "box" && current}
    <hr class="pn-pop-sep" />
    <button class="pn-ident" type="button" aria-expanded="true" onclick={() => close(true)}>{@render current()}</button>
  {/if}

  {#if overview}
    <hr class="pn-pop-sep" />
    <a class="pn-sat" href="#/" onclick={() => close(true)}>
      <span class="pn-sat-pic" aria-hidden="true"><Icon name="overview" /></span>
      <span class="pn-sat-name">Overview</span>
    </a>
  {/if}
</div>

<style>
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
