// The satellite group (ADR 0026) in words: each member's line, what each
// satellite the search found offers, and which buttons work while a change
// runs. From AssistantService's SatelliteGroupView; no Svelte and no runtime
// imports, so `node --test` runs it.
import type { SatelliteGroupCandidate, SatelliteGroupMember, SatelliteGroupView } from "../gen/pinna/v1/assistant_pb.ts";
import { MODE } from "./assistant.ts";

// pinna.v1.SatelliteGroupCandidate.State values (a generated TS enum, which
// `node --test` cannot load).
export const CANDIDATE = { addable: 1, inAnotherGroup: 2, offOrOlder: 3, leftTheGroup: 4 } as const;

/** Two MACs name the same satellite: the search and the member list may
 *  write them in other cases. An empty MAC never matches. */
export function sameMac(a: string, b: string): boolean {
  const digits = (mac: string) => mac.toLowerCase().replace(/[^0-9a-f]/g, "");
  return digits(a) !== "" && digits(a) === digits(b);
}

/** A member's mode as its row says it; unset until its first signed answer. */
export function modeWord(mode: number | undefined): string {
  if (mode === MODE.standalone) return "Standalone mode";
  if (mode === MODE.homeAssistant) return "Home Assistant mode";
  return "Mode not known yet";
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * When a member was last heard from, for the end of its line: `seen just
 * now`, `seen 4 min ago`, `seen 2 h ago`, `seen 3 days ago`. 0 means not
 * since pinnad started on this satellite, called `here`: in another
 * member's row, "this satellite" would read as that member. A time ahead of
 * this computer's clock is just now.
 */
export function seenWord(unixMs: bigint | number, now: number, here: string): string {
  const at = Number(unixMs);
  if (!at) return `not seen since ${here} started`;
  const ago = now - at;
  if (ago < MINUTE) return "seen just now";
  if (ago < HOUR) return `seen ${Math.floor(ago / MINUTE)} min ago`;
  if (ago < DAY) return `seen ${Math.floor(ago / HOUR)} h ago`;
  const days = Math.floor(ago / DAY);
  return `seen ${days} ${days === 1 ? "day" : "days"} ago`;
}

export type MemberLike = Pick<SatelliteGroupMember, "mac" | "thisSatellite" | "mode" | "lastSeenUnixMs" | "hasPassword">;

/**
 * The Assistant page's Satellite group row: a value and a line under it. An open
 * adding window comes first, because it is a time-limited exposure; the value
 * stays the group's own. `joinLeftS` counts the window's seconds down.
 */
export function groupSummary(
  view: Pick<SatelliteGroupView, "on" | "inGroup" | "members"> | null,
  standalone: boolean,
  joinLeftS: number,
): { value?: string; hint: string; tone?: "caution" } {
  if (!view) return { hint: "Satellites that pass messages to each other and act on each other by name" };
  const value = !view.on ? "Off" : view.inGroup ? `${view.members.length} satellites` : "On";
  if (joinLeftS > 0) {
    const m = Math.floor(joinLeftS / 60);
    const s = String(joinLeftS % 60).padStart(2, "0");
    return { value, hint: `Another satellite may add this one for ${m}:${s}`, tone: "caution" };
  }
  if (!view.on) return { value, hint: "Pass messages between satellites, and act on one by its name" };
  if (!view.inGroup) return { value, hint: "Not in a group yet" };
  if (!standalone) return { value, hint: "Takes messages and control; Home Assistant answers requests" };
  return { value, hint: "Messages and control between them" };
}

/** This satellite's name in the view's member list, for lines in the other
 *  members' rows. */
export function ownName(view: Pick<SatelliteGroupView, "members">): string {
  return view.members.find((m) => m.thisSatellite)?.name || "this satellite";
}

/**
 * The line under a member's name. This satellite's mode comes from its
 * Status (`ownMode`); the others' from their last signed answer. `here` is
 * this satellite's name (`ownName`). `outside`: the member's candidate
 * state, set when the search lists it outside the group and it refused a
 * call as not in it (`groupLists`).
 */
export function memberLine(
  m: MemberLike,
  ownMode: number | undefined,
  here: string,
  now: number,
  outside?: number,
): string {
  if (m.thisSatellite) return modeWord(ownMode);
  if (outside !== undefined) {
    const where = outside === CANDIDATE.inAnotherGroup ? "In another group" : "Left the group";
    return `${where} · ${seenWord(m.lastSeenUnixMs, now, here)}`;
  }
  if (m.mode === undefined && !Number(m.lastSeenUnixMs)) return `Not heard from since ${here} started`;
  // A satellite with a password refuses messages and calls from a member without one (ADR 0029).
  const line = `${modeWord(m.mode)} · ${seenWord(m.lastSeenUnixMs, now, here)}`;
  return m.hasPassword === false ? `${line} · No password` : line;
}

/** A row under Members: the member, and its entry among the candidates while
 *  the search lists it outside the group, left or in another group. */
export type MemberRow = { member: SatelliteGroupMember; outside?: SatelliteGroupCandidate };

/**
 * The page's two lists. The view lists a member outside the group twice, as
 * a member and as a candidate; the page shows it once, under Members with
 * its candidate entry, and leaves it out of the count of members and out of
 * Other satellites. Candidates without a satellite are left out.
 */
export function groupLists(view: Pick<SatelliteGroupView, "members" | "candidates">): {
  members: MemberRow[];
  count: number;
  candidates: SatelliteGroupCandidate[];
} {
  const entry = (mac: string) => view.candidates.find((c) => sameMac(c.satellite?.mac ?? "", mac));
  const members = view.members.map((member) => ({
    member,
    outside: member.thisSatellite ? undefined : entry(member.mac),
  }));
  return {
    members,
    count: members.filter((row) => !row.outside).length,
    candidates: view.candidates.filter(
      (c) => c.satellite && !view.members.some((m) => sameMac(m.mac, c.satellite?.mac ?? "")),
    ),
  };
}

export type CandidateLike = Pick<SatelliteGroupCandidate, "state" | "addable" | "defaultName" | "nameRefusal">;

/** What a satellite the search found offers on its row. */
export type Offer = {
  /** Add is offered. */
  add: boolean;
  /** Where it stands, or why it can't be added. */
  text: string;
  /** Its own dashboard is where to act: to turn Satellite group on, rename
   *  it, or see its group. */
  link: boolean;
};

const DEFAULT_NAME =
  "Its name is still the one Pinna gave it, which is hard to say. You can add it, or rename it on its dashboard first.";
const OFF_THERE = "Satellite group is off there, or its Pinna is older.";
const MOVE_HERE = "To move it here, turn its Satellite group switch off and on again on its dashboard.";
const JUST_REMOVED = "Removed. It can be added again after the next search, within about a minute.";

/**
 * A candidate's offer under Other satellites. Add is offered while its
 * switch is on and it is in no group (`addable`), unless its name would be
 * refused: then the refusal takes Add's place. A member that left shows
 * under Members instead (`rejoinOffer`). `justRemoved`: this page removed
 * it a moment ago. The search still has its record from inside the group
 * until its next run, which can read as another group once the group ended.
 */
export function candidateOffer(c: CandidateLike, justRemoved = false): Offer {
  if (justRemoved) return { add: false, text: JUST_REMOVED, link: false };
  if (c.addable && c.nameRefusal) return { add: false, text: c.nameRefusal, link: true };
  if (c.addable) return { add: true, text: c.defaultName ? `In no group. ${DEFAULT_NAME}` : "In no group.", link: c.defaultName };
  if (c.state === CANDIDATE.inAnotherGroup) return { add: false, text: `In another group. ${MOVE_HERE}`, link: true };
  if (c.state === CANDIDATE.offOrOlder) return { add: false, text: OFF_THERE, link: true };
  return { add: false, text: "It can't be added now.", link: true };
}

/**
 * What a member outside the group offers in its row under Members, below
 * its line (`memberLine`), which already says where it is: Add again on the
 * same terms as Add, or why it can't be added again.
 */
export function rejoinOffer(c: CandidateLike): Offer {
  if (c.addable && c.nameRefusal) return { add: false, text: c.nameRefusal, link: true };
  if (c.addable) return { add: true, text: c.defaultName ? DEFAULT_NAME : "", link: c.defaultName };
  if (c.state === CANDIDATE.inAnotherGroup) return { add: false, text: MOVE_HERE, link: true };
  return { add: false, text: OFF_THERE, link: true };
}

/** The search for the other satellites, as DeviceService.ListSatellites
 *  answers it: not finished since pinnad started, failed, or done. */
export type Search = { state: "looking" } | { state: "failed"; why: string } | { state: "done"; onNetwork: boolean };

/**
 * The line Other satellites shows while it lists no satellite. Until the
 * search has run on a network and found none, it says why, in the satellite
 * switcher's words. `alert` for a failure, `status` while it looks.
 * `removed`: the name of a member just removed. The search still has its
 * record from inside the group until its next run, so it shows here only
 * after that.
 */
export function noCandidates(
  search: Search,
  inGroup: boolean,
  removed: string | null = null,
): { text: string; tone: "status" | "alert" | "note" } {
  if (search.state === "looking") return { text: "Looking for other satellites…", tone: "status" };
  if (search.state === "failed") return { text: search.why, tone: "alert" };
  if (!search.onNetwork) return { text: "This satellite isn't on a network yet.", tone: "note" };
  if (removed) return { text: removedNote(removed), tone: "note" };
  const none = inGroup ? "The search found no other satellite to add." : "The search found no other satellite.";
  return { text: `${none} Satellites that run Pinna on the same subnet show up here.`, tone: "note" };
}

/** What Other satellites says about the member `name` just removed while
 *  the search doesn't list it yet. */
export function removedNote(name: string): string {
  return `${name} shows here again after the next search, within about a minute.`;
}

/** A change to the group that runs: Add or Remove of one satellite, or a switch. */
export type Change = { kind: "add" | "remove"; mac: string } | { kind: "on" | "control" };

/** A change the satellite refused, with its sentence for the owner. */
export type Refusal = { change: Change; name: string; sentence: string };

/** A refusal away from its row: `<name>: <sentence>` for an Add or Remove,
 *  the sentence alone for this satellite's own switches. */
function refusalLine({ change, name, sentence }: Refusal): string {
  return "mac" in change ? `${name}: ${sentence}` : sentence;
}

/**
 * The line for the top of the Satellite group card when the row or card a
 * refusal belongs to isn't shown: the view read again after a refusal can
 * take it away. Null while its own row or card shows it.
 */
export function refusalElsewhere(
  refused: Refusal | null,
  view: Pick<SatelliteGroupView, "on" | "inGroup" | "members" | "candidates">,
): string | null {
  if (!refused) return null;
  const { change } = refused;
  const inGroup = view.on && view.inGroup;
  switch (change.kind) {
    // The Satellite group card always shows.
    case "on":
      return null;
    // From the group shows while the Satellite group switch is on.
    case "control":
      return view.on ? null : refusalLine(refused);
    case "remove":
      return inGroup && view.members.some((m) => sameMac(m.mac, change.mac)) ? null : refusalLine(refused);
    // Under Other satellites, or under Members for a member outside the group.
    case "add":
      return view.on && view.candidates.some((c) => sameMac(c.satellite?.mac ?? "", change.mac))
        ? null
        : refusalLine(refused);
  }
}

/**
 * The toast for a refusal while no page shows the group: a change can end
 * after its page closed, and a success shows a toast on any page. Null
 * while a page shows the refusal by its row or card.
 */
export function refusalToast(refused: Refusal, watched: boolean): string | null {
  return watched ? null : refusalLine(refused);
}

/**
 * A view read after a refusal shows that the change happened all the same
 * (an answer that timed out, say): its refusal would then contradict the
 * lists. An Add happened once the satellite is a member that the search
 * doesn't list outside the group; a Remove once the member is gone.
 */
export function happened(change: Change, view: Pick<SatelliteGroupView, "inGroup" | "members" | "candidates">): boolean {
  if (!("mac" in change)) return false;
  const member = view.inGroup && view.members.some((m) => sameMac(m.mac, change.mac));
  if (change.kind === "remove") return !member;
  return member && !view.candidates.some((c) => sameMac(c.satellite?.mac ?? "", change.mac));
}

/** The toast after a Remove, from the view it answered: removing the last
 *  other member ends the group. */
export function removedText(name: string, after: Pick<SatelliteGroupView, "inGroup"> | undefined): string {
  return after && !after.inGroup ? `${name} removed. The group ended.` : `${name} removed`;
}

/**
 * An Add or Remove button. The one whose change runs says so and ignores
 * presses (`working`); while any change runs, the others are disabled, as
 * they are while the satellite can't be reached.
 */
export function rowButton(
  live: boolean,
  running: Change | null,
  kind: "add" | "remove",
  mac: string,
): { working: boolean; disabled: boolean } {
  const working = !!running && "mac" in running && running.kind === kind && sameMac(running.mac, mac);
  return { working, disabled: !live || (!!running && !working) };
}

/**
 * Which answer the page shows: a refresh never puts back a view older than
 * the one shown. A change's answer always wins (one runs at a time). A
 * refresh's answer counts only if it was asked after the latest change
 * ended and after the refresh already shown, and while no change runs: a
 * refresh asked during an Add may have been read before the Add finished.
 */
export class Replies {
  #next = 0;
  #shown = 0;
  #changing = false;

  /** A refresh is asked: its ticket. */
  read(): number {
    return ++this.#next;
  }

  /** A change starts. */
  start() {
    this.#changing = true;
  }

  /** The change ended, answered or refused: refreshes asked before now are older. */
  end() {
    this.#changing = false;
    this.#shown = ++this.#next;
  }

  /** Whether the refresh with `ticket` is newer than what the page shows; it
   *  is shown from now on if so. */
  fresh(ticket: number): boolean {
    if (this.#changing || ticket <= this.#shown) return false;
    this.#shown = ticket;
    return true;
  }
}

/** Removing `mac` leaves this satellite alone, which ends the group. */
export function removeEndsGroup(view: Pick<SatelliteGroupView, "members">, mac: string): boolean {
  return view.members.filter((m) => !m.thisSatellite && !sameMac(m.mac, mac)).length === 0;
}

/**
 * Each item with a key for a keyed `#each`, which fails on a repeated key:
 * the search's list comes from the network. A repeated key gets a count.
 */
export function keyed<T>(items: readonly T[], key: (item: T) => string): { key: string; item: T }[] {
  const seen = new Map<string, number>();
  return items.map((item) => {
    const k = key(item);
    const n = seen.get(k) ?? 0;
    seen.set(k, n + 1);
    return { key: n ? `${k}#${n}` : k, item };
  });
}
