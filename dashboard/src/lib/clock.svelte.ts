// A clock for countdowns on the screens that run before the dashboard does,
// where `device.now` isn't ticking yet. It ticks while something uses it.
class Clock {
  now = $state(Date.now());

  #users = 0;
  #timer: ReturnType<typeof setInterval> | undefined;

  /** Tick for as long as the caller needs it; returns the stop for an $effect. */
  use(): () => void {
    if (this.#users++ === 0) {
      this.now = Date.now();
      this.#timer = setInterval(() => (this.now = Date.now()), 500);
    }
    return () => {
      if (--this.#users === 0) clearInterval(this.#timer);
    };
  }
}

export const clock = new Clock();
