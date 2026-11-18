// Keyboard rules for the design system's dropdown (Dropdown.svelte), after
// the WAI-ARIA "select-only combobox" pattern: the arrows, Home, End and the
// page keys move the active option over the enabled ones, and typing jumps to
// the next option that starts with what was typed.

export type ListOption = { id: string; label: string; disabled?: boolean };

/** How far PageUp and PageDown move. */
const PAGE = 10;

/** The enabled option `key` moves to from `from` (-1: none active yet), or `from`. */
export function step(options: ListOption[], from: number, key: string): number {
  const enabled = options.flatMap((o, i) => (o.disabled ? [] : [i]));
  if (!enabled.length) return from;
  const first = enabled[0];
  const last = enabled[enabled.length - 1];
  const after = (n: number) => enabled.find((i) => i > from + n - 1 && i !== from) ?? last;
  const before = (n: number) => [...enabled].reverse().find((i) => i < from - n + 1 && i !== from) ?? first;
  switch (key) {
    case "Home":
      return first;
    case "End":
      return last;
    case "ArrowDown":
      return from < 0 ? first : after(1);
    case "ArrowUp":
      return from < 0 ? last : before(1);
    case "PageDown":
      return from < 0 ? first : after(PAGE);
    case "PageUp":
      return from < 0 ? last : before(PAGE);
    default:
      return from;
  }
}

/**
 * The enabled option whose label starts with `typed` (case-insensitive),
 * searching after `from` and wrapping; -1 when none does. One letter typed
 * again cycles through the options that start with it.
 */
export function typeahead(options: ListOption[], from: number, typed: string): number {
  const text = typed.toLocaleLowerCase();
  if (!text) return -1;
  const same = [...text].every((c) => c === text[0]);
  const prefix = same ? text[0] : text;
  // A longer prefix may still match the current option; one letter moves on.
  const start = same ? from + 1 : Math.max(from, 0);
  for (let n = 0; n < options.length; n++) {
    const i = (start + n) % options.length;
    const o = options[i];
    if (!o.disabled && o.label.toLocaleLowerCase().startsWith(prefix)) return i;
  }
  return -1;
}
