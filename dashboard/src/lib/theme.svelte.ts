// The theme this browser shows the dashboard in: the system's, light or dark.
// It belongs to the browser, not to the satellite, so it is kept in
// localStorage under the key the website uses, as its switch does (each
// origin keeps its own).
// index.html applies it before the first paint.
import type { IconName } from "../../../design/icons.ts";

export const THEMES = ["system", "light", "dark"] as const;
export type Theme = (typeof THEMES)[number];

const KEY = "pinna-theme";

function read(): Theme {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    // A browser that blocks storage keeps the system theme.
  }
  return "system";
}

function show(t: Theme) {
  if (t === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
}

class ThemeState {
  current = $state<Theme>(read());

  constructor() {
    // Another tab of the dashboard changed it.
    window.addEventListener("storage", (e) => {
      if (e.key !== KEY) return;
      this.current = read();
      show(this.current);
    });
  }

  set(t: Theme) {
    this.current = t;
    show(t);
    try {
      if (t === "system") localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, t);
    } catch {
      // The switch still works until the page reloads.
    }
  }

  /**
   * What the button switches to: from System, first the opposite of what the
   * system shows, so the first press always changes the page; then the other
   * one, then System again.
   */
  upcoming(): Theme {
    const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    const order: Theme[] = dark ? ["system", "light", "dark"] : ["system", "dark", "light"];
    return order[(order.indexOf(this.current) + 1) % order.length];
  }

  next() {
    this.set(this.upcoming());
  }
}

export const theme = new ThemeState();

export const THEME_LABEL = { system: "System", light: "Light", dark: "Dark" } as const satisfies Record<Theme, string>;
export const THEME_ICON = { system: "contrast", light: "sun", dark: "moon" } as const satisfies Record<Theme, IconName>;
