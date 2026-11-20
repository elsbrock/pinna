// The dashboard's access state (ADR 0029): whether a password is set, whether
// this browser has a session, and the calls that change them. GetSecurity needs
// no session, so it still answers when every other call is refused.
import { type Security, TokenKind } from "../gen/pinna/v1/device_pb.ts";
import { client, errorSentence } from "./device.svelte.ts";
import { type LoginOutcome, loginOutcome } from "./view.ts";

const JSON_HEADERS = { "content-type": "application/json" };

class SecurityState {
  /** Null until GetSecurity first answers. */
  state = $state<Security | null>(null);

  #refreshing = false;

  async refresh() {
    if (this.#refreshing) return;
    this.#refreshing = true;
    try {
      this.state = (await client.getSecurity({})).security ?? this.state;
    } catch {
      // Unreachable: the page shows that already, and asks again at the next change.
    } finally {
      this.#refreshing = false;
    }
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
   *  right behind it may be asked to wait a second: wait it out once. */
  async signIn(password: string, afterChange = false): Promise<LoginOutcome> {
    let outcome = await this.login(password);
    if (afterChange && !outcome.ok && outcome.wait > 0 && outcome.wait <= 2) {
      const wait = outcome.wait;
      await new Promise((resolve) => setTimeout(resolve, wait * 1000 + 100));
      outcome = await this.login(password);
    }
    if (outcome.ok) location.reload();
    return outcome;
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
