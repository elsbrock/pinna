<!-- The design system's dropdown: a choice from a fixed list, drawn with the
     tokens instead of the browser's own menu. It follows the WAI-ARIA
     "select-only combobox" pattern: the button keeps the focus and names the
     active option (aria-activedescendant); the list floats over the page as a
     pn-pop below the button, or above it when there is more room there. The
     page owns `value` and reacts to `choose`. -->
<script lang="ts">
  import { onDestroy, tick } from "svelte";

  import Icon from "./Icon.svelte";
  import { type ListOption, step, typeahead } from "./listbox.ts";

  let {
    id,
    value,
    options,
    choose,
    placeholder = "Choose…",
    disabled = false,
    labelledby,
    describedby,
  }: {
    id: string;
    value: string;
    options: ListOption[];
    choose: (id: string) => unknown;
    /** Shown while `value` names no option. */
    placeholder?: string;
    disabled?: boolean;
    /** The visible label's id; a `<label for={id}>` works too. */
    labelledby?: string;
    describedby?: string;
  } = $props();

  let button = $state<HTMLButtonElement>();
  let list = $state<HTMLUListElement>();
  let open = $state(false);
  let active = $state(-1);
  let place = $state("");

  const chosen = $derived(options.findIndex((o) => o.id === value));
  const optionId = (i: number) => `${id}-option-${i}`;

  // What was typed in the last second, for typeahead.
  let typed = "";
  let typedAt = 0;

  function position() {
    if (!button) return;
    const r = button.getBoundingClientRect();
    const width = Math.min(Math.max(r.width, 200), innerWidth - 16);
    const left = Math.max(8, Math.min(r.left, innerWidth - width - 8));
    const below = innerHeight - r.bottom - 16;
    const above = r.top - 16;
    place =
      below >= Math.min(320, above)
        ? `left:${left}px;top:${r.bottom + 4}px;width:${width}px;max-height:${Math.max(120, below)}px`
        : `left:${left}px;bottom:${innerHeight - r.top + 4}px;width:${width}px;max-height:${Math.max(120, above)}px`;
  }

  async function show(at: number) {
    if (disabled) return;
    position();
    active = at;
    open = true;
    await tick();
    reveal();
    addEventListener("pointerdown", outside, true);
    addEventListener("resize", away);
    addEventListener("scroll", away, true);
    addEventListener("hashchange", away);
  }

  function hide() {
    open = false;
    removeEventListener("pointerdown", outside, true);
    removeEventListener("resize", away);
    removeEventListener("scroll", away, true);
    removeEventListener("hashchange", away);
  }
  onDestroy(hide);

  function pick(i: number) {
    const o = options[i];
    hide();
    button?.focus();
    if (o && !o.disabled && o.id !== value) choose(o.id);
  }

  function outside(e: PointerEvent) {
    const t = e.target as Node;
    if (!list?.contains(t) && !button?.contains(t)) hide();
  }
  // The list is placed from the button: when the page moves, it closes. A
  // scroll inside the list is its own.
  function away(e: Event) {
    if (e.type === "scroll" && list?.contains(e.target as Node)) return;
    hide();
  }

  function reveal() {
    if (active >= 0) list?.querySelector(`#${CSS.escape(optionId(active))}`)?.scrollIntoView({ block: "nearest" });
  }

  function move(to: number) {
    active = to;
    reveal();
  }

  function keydown(e: KeyboardEvent) {
    const key = e.key;
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " ", "Home", "End"].includes(key)) {
        e.preventDefault();
        const from = chosen >= 0 ? chosen : -1;
        show(key === "Home" || key === "End" ? step(options, from, key) : from >= 0 ? from : step(options, -1, "ArrowDown"));
        return;
      }
    } else {
      if (key === "Escape") {
        e.preventDefault();
        hide();
        return;
      }
      if (key === "Enter" || key === " " || (key === "ArrowUp" && e.altKey)) {
        e.preventDefault();
        pick(active);
        return;
      }
      // Tab chooses the active option and moves on, as a native select does.
      if (key === "Tab") {
        if (active >= 0) pick(active);
        else hide();
        return;
      }
      if (["ArrowDown", "ArrowUp", "Home", "End", "PageDown", "PageUp"].includes(key)) {
        e.preventDefault();
        move(step(options, active, key));
        return;
      }
    }
    if (key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey && key !== " ") {
      const now = performance.now();
      typed = now - typedAt < 1000 ? typed + key : key;
      typedAt = now;
      const from = open ? active : chosen;
      const found = typeahead(options, from, typed);
      if (found < 0) return;
      e.preventDefault();
      if (open) move(found);
      else if (options[found].id !== value) choose(options[found].id);
    }
  }
</script>

<span class="pn-select-wrap">
  <button
    bind:this={button}
    {id}
    type="button"
    class="pn-select pn-dropdown"
    class:is-placeholder={chosen < 0}
    role="combobox"
    aria-haspopup="listbox"
    aria-expanded={open}
    aria-controls="{id}-list"
    aria-activedescendant={open && active >= 0 ? optionId(active) : undefined}
    aria-labelledby={labelledby ? `${labelledby} ${id}` : undefined}
    aria-describedby={describedby}
    {disabled}
    onclick={() => (open ? hide() : show(chosen >= 0 ? chosen : step(options, -1, "ArrowDown")))}
    onkeydown={keydown}
    onblur={(e) => {
      // Focus leaving for anything but the list closes it.
      const to = e.relatedTarget as Node | null;
      if (open && !list?.contains(to)) hide();
    }}
  >
    <!-- Every label in one grid cell, only the chosen one visible: the
         button is as wide as the longest, as a native select is. -->
    <span class="pn-dropdown-value">
      {#each [placeholder, ...options.map((o) => o.label)] as label, i (i)}
        <span class:is-shown={i === chosen + 1}>{label}</span>
      {/each}
    </span>
    <Icon name="chevron-down" class="pn-icon pn-select-chev" />
  </button>
  {#if open}
    <ul bind:this={list} id="{id}-list" class="pn-pop pn-listbox" role="listbox" style={place} tabindex="-1">
      {#each options as o, i (o.id)}
        <!-- The button handles the keys (aria-activedescendant); a pointer picks here. -->
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <li
          id={optionId(i)}
          class="pn-option"
          class:is-active={i === active}
          role="option"
          aria-selected={o.id === value}
          aria-disabled={o.disabled || undefined}
          onpointermove={() => !o.disabled && (active = i)}
          onpointerdown={(e) => e.preventDefault()}
          onclick={() => !o.disabled && pick(i)}
        >
          <span class="pn-option-label">{o.label}</span>
          {#if o.id === value}<Icon name="check" class="pn-icon pn-option-check" />{/if}
        </li>
      {/each}
    </ul>
  {/if}
</span>
