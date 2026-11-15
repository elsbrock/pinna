// Short confirmations ("Saved") and rejected commands, shown for a few seconds
// at the bottom of the page. Toasts.svelte renders them.
export type Tone = "ok" | "info" | "danger";
export type Toast = { id: number; text: string; tone: Tone };

const SHOW_MS = 4_000;

class Toasts {
  list = $state<Toast[]>([]);
  #next = 0;

  show(text: string, tone: Tone = "ok") {
    // The same words again restart the clock instead of stacking.
    this.list = this.list.filter((t) => t.text !== text);
    const t = { id: this.#next++, text, tone };
    this.list = [...this.list.slice(-2), t];
    setTimeout(() => (this.list = this.list.filter((x) => x.id !== t.id)), SHOW_MS);
  }
}

export const toasts = new Toasts();
