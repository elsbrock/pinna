<!-- A text field that suggests values: any text goes, and a list in the
     design system's pn-pop offers what fits what was typed (the WAI-ARIA
     combobox with a list popup). The arrow keys or the button open it, typing
     narrows it, Enter takes the active suggestion and Escape closes it. A
     suggestion shows its label, and its id under it when they differ (an
     ElevenLabs voice by name, with the ID the field takes). `load` is called
     when the list first opens, and again when `source` changes. -->
<script lang="ts">
  import { onDestroy, tick } from "svelte";

  import Icon from "./Icon.svelte";
  import { type ListOption, matching, step } from "./listbox.ts";

  let {
    id,
    value = $bindable(""),
    source,
    load,
    oninput,
    placeholder,
    disabled = false,
    describedby,
  }: {
    id: string;
    value?: string;
    /** Names where the suggestions come from (the connection and the kind). */
    source: string;
    /** The suggestions, fetched once the list first opens; none is fine. */
    load: () => Promise<ListOption[]>;
    /** Called on every change, typed or picked. */
    oninput?: () => void;
    placeholder?: string;
    disabled?: boolean;
    describedby?: string;
  } = $props();

  let input = $state<HTMLInputElement>();
  let list = $state<HTMLUListElement>();
  let open = $state(false);
  let active = $state(-1);
  let place = $state("");
  let all = $state<ListOption[] | null>(null);
  let loading = $state(false);
  /** Why the service gave no list, when it couldn't. */
  let problem = $state<string | null>(null);
  // Typing narrows the list; opening it with the button or an arrow shows
  // everything again.
  let filter = $state("");
  const shown = $derived(all ? matching(all, filter) : []);
  const optionId = (i: number) => `${id}-suggestion-${i}`;

  // Another connection is asked again.
  $effect(() => {
    void source;
    all = null;
    problem = null;
  });

  function position() {
    if (!input) return;
    const r = input.getBoundingClientRect();
    const width = Math.min(Math.max(r.width, 240), innerWidth - 16);
    const left = Math.max(8, Math.min(r.left, innerWidth - width - 8));
    const below = innerHeight - r.bottom - 16;
    const above = r.top - 16;
    place =
      below >= Math.min(320, above)
        ? `left:${left}px;top:${r.bottom + 4}px;width:${width}px;max-height:${Math.max(120, below)}px`
        : `left:${left}px;bottom:${innerHeight - r.top + 4}px;width:${width}px;max-height:${Math.max(120, above)}px`;
  }

  async function show(narrow: boolean) {
    if (disabled) return;
    filter = narrow ? value : "";
    active = -1;
    position();
    open = true;
    addEventListener("pointerdown", outside, true);
    addEventListener("resize", hide);
    addEventListener("scroll", away, true);
    addEventListener("hashchange", hide);
    if (all === null && !loading) {
      loading = true;
      const asked = source;
      let found: ListOption[] = [];
      let why: string | null = null;
      try {
        found = await load();
      } catch (e) {
        why = e instanceof Error ? e.message : String(e);
      }
      // The connection may have changed while it answered. A failure is
      // asked again the next time the list opens.
      if (asked === source) {
        all = why ? null : found;
        problem = why;
      }
      loading = false;
      await tick();
      position();
    }
  }

  function hide() {
    open = false;
    active = -1;
    removeEventListener("pointerdown", outside, true);
    removeEventListener("resize", hide);
    removeEventListener("scroll", away, true);
    removeEventListener("hashchange", hide);
  }
  onDestroy(hide);

  function outside(e: PointerEvent) {
    const t = e.target as Node;
    if (!list?.contains(t) && !input?.parentElement?.contains(t)) hide();
  }
  function away(e: Event) {
    if (list?.contains(e.target as Node)) return;
    hide();
  }

  function pick(i: number) {
    const o = shown[i];
    if (!o) return;
    value = o.id;
    hide();
    input?.focus();
    oninput?.();
  }

  async function move(to: number) {
    active = to;
    await tick();
    if (to >= 0) list?.querySelector(`#${CSS.escape(optionId(to))}`)?.scrollIntoView({ block: "nearest" });
  }

  function keydown(e: KeyboardEvent) {
    if (!open) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        void show(false);
      }
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      hide();
    } else if (e.key === "Enter" && active >= 0) {
      // Takes the suggestion instead of submitting the form.
      e.preventDefault();
      pick(active);
    } else if (e.key === "Tab") {
      hide();
    } else if (["ArrowDown", "ArrowUp", "Home", "End", "PageDown", "PageUp"].includes(e.key)) {
      // Home and End stay the text field's own.
      if (e.key === "Home" || e.key === "End") return;
      e.preventDefault();
      void move(step(shown, active, e.key));
    }
  }
</script>

<span class="pn-suggest">
  <input
    bind:this={input}
    {id}
    class="pn-input is-mono"
    type="text"
    role="combobox"
    autocomplete="off"
    autocapitalize="off"
    spellcheck="false"
    aria-autocomplete="list"
    aria-expanded={open}
    aria-controls="{id}-suggestions"
    aria-activedescendant={open && active >= 0 ? optionId(active) : undefined}
    aria-describedby={describedby}
    {placeholder}
    {disabled}
    bind:value
    oninput={() => {
      oninput?.();
      if (open) {
        filter = value;
        active = -1;
      } else void show(true);
    }}
    onkeydown={keydown}
    onblur={(e) => {
      const to = e.relatedTarget as Node | null;
      if (open && !list?.contains(to) && !input?.parentElement?.contains(to)) hide();
    }}
  />
  <button
    class="pn-suggest-toggle"
    type="button"
    tabindex="-1"
    aria-label="Show what the service offers"
    {disabled}
    onpointerdown={(e) => e.preventDefault()}
    onclick={() => {
      input?.focus();
      if (open) hide();
      else void show(false);
    }}
  >
    <Icon name="chevron-down" class="pn-icon" />
  </button>
  {#if open}
    <ul bind:this={list} id="{id}-suggestions" class="pn-pop pn-listbox" role="listbox" style={place} tabindex="-1">
      {#if loading}
        <li class="pn-pop-note" role="presentation">Asking the service…</li>
      {:else if problem}
        <li class="pn-pop-note" role="presentation">It couldn't list them: {problem} Type the name.</li>
      {:else if !shown.length}
        <li class="pn-pop-note" role="presentation">
          {all?.length ? "Nothing it offers matches. Any name it knows works." : "The service lists none here. Type the name."}
        </li>
      {:else}
        {#each shown as o, i (o.id)}
          <!-- The field handles the keys (aria-activedescendant); a pointer picks here. -->
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <li
            id={optionId(i)}
            class="pn-option"
            class:is-active={i === active}
            role="option"
            aria-selected={o.id === value}
            onpointermove={() => (active = i)}
            onpointerdown={(e) => e.preventDefault()}
            onclick={() => pick(i)}
          >
            <span class="pn-option-text">
              <span class="pn-option-label">{o.label}</span>
              {#if o.label !== o.id}<span class="pn-option-id">{o.id}</span>{/if}
            </span>
          </li>
        {/each}
      {/if}
    </ul>
  {/if}
</span>
