// The dashboard's access state (ADR 0029): whether a password is set, whether
// this browser has a session, and the calls that change them. GetSecurity needs
// no session, so it still answers when every other call is refused.
import { type Security, TokenKind } from "../gen/pinna/v1/device_pb.ts";
import { client, errorSentence } from "./device.svelte.ts";
import { type LoginOutcome, loginOutcome, lostSession, PASSWORD_NOTICES, type PasswordNotice } from "./view.ts";

const JSON_HEADERS = { "content-type": "application/json" };
/** sessionStorage key: what to tell after the reload that follows a new password. */
const NOTICE_KEY = "pinna.passwordNotice";

class SecurityState {
  /** Null until GetSecurity first answers. */
  state = $state<Security | null>(null);
  /** Why the last read failed, until one succeeds. */
  error = $state<string | null>(null);
  /** This tab had a session and the satellite says it has none now. */
  signedOut = $state(false);
  /** A confirmation this browser asked for waits: the Overview doesn't warn of its own. */
  confirmingHere = $state(false);
  /** When `state` arrived (Date.now()), for counting a confirmation's seconds down. */
  receivedAt = $state(0);

  #refreshing: Promise<void> | null = null;

  /** Ask the satellite; a call while one runs waits for that one. */
  refresh(): Promise<void> {
    this.#refreshing ??= this.#fetch().finally(() => (this.#refreshing = null));
    return this.#refreshing;
  }

  async #fetch() {
    try {
      const next = (await client.getSecurity({})).security;
      if (next) {
        if (lostSession(this.state, next)) this.signedOut = true;
        this.state = next;
        this.receivedAt = Date.now();
      }
      this.error = null;
    } catch (e) {
      // Pages that need the state say why; the others ask again at the next change.
      this.error = errorSentence(e);
    }
  }

  /** End the confirmation that waits, whoever started it; or the satellite's sentence. */
  async stopConfirmation(): Promise<string | null> {
    try {
      await client.cancelConfirmation({});
    } catch (e) {
      return errorSentence(e);
    }
    await this.refresh();
    return null;
  }

  /** A call came back `unauthenticated`: ask what this browser may do now. */
  expire() {
    if (this.state?.passwordSet && !this.state.signedIn) return;
    void this.refresh();
  }

  /** POST /auth/login. On success the cookie is set; the caller reloads. */
  async login(password: string): Promise<LoginOutcome> {
    let res: Response;
    try {
      res = await fetch("/auth/login", { method: "POST", headers: JSON_HEADERS, body: JSON.stringify({ password }) });
    } catch {
      return { ok: false, message: "Can't reach the satellite. Check that it has power and Wi-Fi.", wait: 0 };
    }
    return loginOutcome(res.status, await res.json().catch(() => null));
  }

  /** Sign in, then load the page afresh, as on first load. `afterChange`: the
   *  password was just set or checked, which counts as an attempt, so a login
   *  right behind it may be asked to wait a second: wait it out once. `notice`
   *  is told once the page has loaded. */
  async signIn(password: string, afterChange = false, notice?: PasswordNotice): Promise<LoginOutcome> {
    let outcome = await this.login(password);
    if (afterChange && !outcome.ok && outcome.wait > 0 && outcome.wait <= 2) {
      const wait = outcome.wait;
      await new Promise((resolve) => setTimeout(resolve, wait * 1000 + 100));
      outcome = await this.login(password);
    }
    if (outcome.ok) {
      try {
        if (notice) sessionStorage.setItem(NOTICE_KEY, notice);
      } catch {
        // Private mode: the page just doesn't say it.
      }
      location.reload();
    }
    return outcome;
  }

  /** What to tell after a reload that followed a new password; once. */
  takeNotice(): string | null {
    try {
      const kind = sessionStorage.getItem(NOTICE_KEY) as PasswordNotice | null;
      sessionStorage.removeItem(NOTICE_KEY);
      return kind ? (PASSWORD_NOTICES[kind] ?? null) : null;
    } catch {
      return null;
    }
  }

  /** Seconds until a new confirmation may start; 0 when one may or the satellite doesn't answer. */
  async cooldown(): Promise<number> {
    try {
      return (await client.getSecurity({})).security?.confirmationCooldownS ?? 0;
    } catch {
      return 0;
    }
  }

  /** POST /auth/logout: this browser only. */
  async signOut(): Promise<string | null> {
    try {
      await fetch("/auth/logout", { method: "POST", headers: JSON_HEADERS, body: "{}" });
    } catch (e) {
      return errorSentence(e);
    }
    location.reload();
    return null;
  }

  /** Each of these resolves to the satellite's sentence when it refuses, else null. */
  async #call(run: () => Promise<{ security?: Security }>, adopt = true): Promise<string | null> {
    try {
      const r = await run();
      if (adopt && r.security) this.state = r.security;
      return null;
    } catch (e) {
      return errorSentence(e);
    }
  }

  leaveOpen() {
    return this.#call(() => client.leaveDashboardOpen({}));
  }
  /** The reply says no session is left; the caller signs in again with the new password. */
  setPassword(req: { newPassword: string; currentPassword?: string; grant?: string }) {
    return this.#call(() => client.setDashboardPassword(req), false);
  }
  removePassword(req: { currentPassword?: string; grant?: string }) {
    return this.#call(() => client.removeDashboardPassword(req));
  }
  signOutEverywhere() {
    return this.#call(() => client.signOutEverywhere({}), false);
  }

  /** A new token, shown once; or the satellite's sentence. */
  async createToken(kind: "metrics" | "agent"): Promise<{ token: string } | { error: string }> {
    try {
      const r = await client.createToken({ kind: kind === "metrics" ? TokenKind.METRICS : TokenKind.AGENT });
      if (r.security) this.state = r.security;
      return { token: r.token };
    } catch (e) {
      return { error: errorSentence(e) };
    }
  }
}

export const security = new SecurityState();
