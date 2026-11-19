import assert from "node:assert/strict";
import { test } from "node:test";

import type { SatelliteGroupCandidate, SatelliteGroupMember } from "../gen/pinna/v1/assistant_pb.ts";
import type { Satellite } from "../gen/pinna/v1/device_pb.ts";
import { MODE } from "./assistant.ts";
import {
  CANDIDATE,
  type Change,
  candidateOffer,
  groupLists,
  happened,
  keyed,
  memberLine,
  modeWord,
  noCandidates,
  ownName,
  refusalElsewhere,
  refusalToast,
  rejoinOffer,
  removedNote,
  removedText,
  removeEndsGroup,
  type Refusal,
  Replies,
  rowButton,
  sameMac,
  seenWord,
} from "./group.ts";

// node's type stripping can't run the generated module (it holds a TS enum),
// so the tests build message shapes by hand.
const member = (over: Partial<SatelliteGroupMember> = {}): SatelliteGroupMember => ({
  $typeName: "pinna.v1.SatelliteGroupMember",
  mac: "02:00:00:00:00:02",
  name: "Kitchen",
  thisSatellite: false,
  lastSeenUnixMs: 0n,
  ...over,
});
const satellite = (mac: string): Satellite => ({
  $typeName: "pinna.v1.Satellite",
  name: "kitchen",
  host: "kitchen",
  friendlyName: "Kitchen",
  board: "Echo Dot (2nd Gen)",
  mac,
  version: "0.9.0",
  webPort: 80,
  url: "",
  ipv4: ["192.168.1.20"],
  ipv6: [],
});
const candidate = (over: Partial<SatelliteGroupCandidate> = {}): SatelliteGroupCandidate => ({
  $typeName: "pinna.v1.SatelliteGroupCandidate",
  satellite: satellite("02:00:00:00:00:03"),
  state: CANDIDATE.addable,
  addable: true,
  defaultName: false,
  nameRefusal: "",
  ...over,
});

const NOW = Date.UTC(2026, 10, 19, 12, 0, 0);

test("MACs match whatever their case and separators; an empty one never does", () => {
  assert.ok(sameMac("02:00:00:AA:BB:CC", "02:00:00:aa:bb:cc"));
  assert.ok(sameMac("0200-00aa-bbcc", "02:00:00:aa:bb:cc"));
  assert.ok(!sameMac("02:00:00:aa:bb:cc", "02:00:00:aa:bb:cd"));
  assert.ok(!sameMac("", ""));
});

test("a member's mode in words, and unknown until its first answer", () => {
  assert.equal(modeWord(MODE.standalone), "Standalone mode");
  assert.equal(modeWord(MODE.homeAssistant), "Home Assistant mode");
  assert.equal(modeWord(undefined), "Mode not known yet");
});

test("when a member was last seen, relative to now", () => {
  // Named: in another member's row, "this satellite" reads as that member.
  assert.equal(seenWord(0n, NOW, "Hall"), "not seen since Hall started");
  assert.equal(seenWord(BigInt(NOW - 20_000), NOW, "Hall"), "seen just now");
  // The satellite's clock may run ahead of this computer's.
  assert.equal(seenWord(BigInt(NOW + 90_000), NOW, "Hall"), "seen just now");
  assert.equal(seenWord(BigInt(NOW - 60_000), NOW, "Hall"), "seen 1 min ago");
  assert.equal(seenWord(BigInt(NOW - 59 * 60_000), NOW, "Hall"), "seen 59 min ago");
  assert.equal(seenWord(BigInt(NOW - 2 * 3_600_000 - 1), NOW, "Hall"), "seen 2 h ago");
  assert.equal(seenWord(BigInt(NOW - 26 * 3_600_000), NOW, "Hall"), "seen 1 day ago");
  assert.equal(seenWord(NOW - 3 * 86_400_000, NOW, "Hall"), "seen 3 days ago");
});

test("this satellite's name, from its own entry in the member list", () => {
  assert.equal(ownName({ members: [member(), member({ mac: "02:00:00:00:00:01", name: "Hall", thisSatellite: true })] }), "Hall");
  assert.equal(ownName({ members: [] }), "this satellite");
});

test("a member's line: this satellite's own mode, the others' mode and when they were seen", () => {
  const seen = BigInt(NOW - 4 * 60_000);
  const line = (m: SatelliteGroupMember, outside?: number) => memberLine(m, MODE.standalone, "Hall", NOW, outside);
  // This satellite's entry carries no mode; its Status does.
  assert.equal(memberLine(member({ thisSatellite: true }), MODE.homeAssistant, "Hall", NOW), "Home Assistant mode");
  assert.equal(line(member({ mode: MODE.standalone, lastSeenUnixMs: seen })), "Standalone mode · seen 4 min ago");
  assert.equal(line(member({ mode: MODE.homeAssistant, lastSeenUnixMs: seen })), "Home Assistant mode · seen 4 min ago");
  // Seen by the search, but no signed answer yet.
  assert.equal(line(member({ lastSeenUnixMs: seen })), "Mode not known yet · seen 4 min ago");
  assert.equal(line(member()), "Not heard from since Hall started");
  assert.equal(line(member({ mode: MODE.standalone, lastSeenUnixMs: seen }), CANDIDATE.leftTheGroup), "Left the group · seen 4 min ago");
  assert.equal(
    line(member({ mode: MODE.standalone, lastSeenUnixMs: seen }), CANDIDATE.inAnotherGroup),
    "In another group · seen 4 min ago",
  );
  assert.equal(line(member(), CANDIDATE.leftTheGroup), "Left the group · not seen since Hall started");
});

test("a member the candidates list too shows once, under Members, and doesn't count", () => {
  const own = member({ mac: "02:00:00:00:00:01", name: "Hall", thisSatellite: true });
  const kitchen = member({ mac: "02:00:00:00:00:02" });
  const garage = member({ mac: "02:00:00:00:00:06", name: "Garage", mode: MODE.standalone, lastSeenUnixMs: BigInt(NOW - 60_000) });
  const study = member({ mac: "02:00:00:00:00:08", name: "Study" });
  const left = candidate({ satellite: satellite("02:00:00:00:00:08"), state: CANDIDATE.leftTheGroup });
  // A member that joined another group; the search writes MACs in capitals.
  const elsewhere = candidate({ satellite: satellite("02:00:00:00:00:06".toUpperCase()), state: CANDIDATE.inAnotherGroup, addable: false });
  const office = candidate({ satellite: satellite("02:00:00:00:00:03") });
  const lists = groupLists({
    members: [garage, own, kitchen, study],
    candidates: [left, elsewhere, office, candidate({ satellite: undefined })],
  });
  assert.deepEqual(lists.members, [
    { member: garage, outside: elsewhere },
    { member: own, outside: undefined },
    { member: kitchen, outside: undefined },
    { member: study, outside: left },
  ]);
  assert.equal(lists.count, 2);
  assert.deepEqual(lists.candidates, [office]);
  // The line its row shows: where it is, not its mode, as if it were still a member.
  assert.equal(memberLine(garage, MODE.standalone, "Hall", NOW, lists.members[0].outside?.state), "In another group · seen 1 min ago");
});

test("a satellite with its switch on and in no group offers Add", () => {
  assert.deepEqual(candidateOffer(candidate()), { add: true, text: "In no group.", link: false });
});

test("a default name is noted, and Add stays", () => {
  const offer = candidateOffer(candidate({ defaultName: true }));
  assert.equal(offer.add, true);
  assert.match(offer.text, /^In no group\. Its name is still the one Pinna gave it, which is hard to say\./);
  assert.equal(offer.link, true);
});

test("a name that Add would refuse takes Add's place, with the satellite's sentence", () => {
  const why = "A member is called Kitchen already. Rename it first on its dashboard.";
  assert.deepEqual(candidateOffer(candidate({ nameRefusal: why, defaultName: true })), { add: false, text: why, link: true });
});

test("a member outside the group offers Add again only while its switch is on and it is in no group", () => {
  // Its line says where it is; the offer says only what to do.
  assert.deepEqual(rejoinOffer(candidate({ state: CANDIDATE.leftTheGroup })), { add: true, text: "", link: false });
  assert.equal(rejoinOffer(candidate({ state: CANDIDATE.leftTheGroup, defaultName: true })).add, true);
  assert.deepEqual(rejoinOffer(candidate({ state: CANDIDATE.leftTheGroup, addable: false })), {
    add: false,
    text: "Satellite group is off there, or its Pinna is older.",
    link: true,
  });
  assert.deepEqual(rejoinOffer(candidate({ state: CANDIDATE.inAnotherGroup, addable: false })), {
    add: false,
    text: "To move it here, turn its Satellite group switch off and on again on its dashboard.",
    link: true,
  });
  const why = "A member is called Kitchen already. Rename it first on its dashboard.";
  assert.deepEqual(rejoinOffer(candidate({ state: CANDIDATE.leftTheGroup, nameRefusal: why })), { add: false, text: why, link: true });
});

test("the satellites that can't be added say why, and link to their dashboard", () => {
  assert.deepEqual(candidateOffer(candidate({ state: CANDIDATE.inAnotherGroup, addable: false })), {
    add: false,
    text: "In another group. To move it here, turn its Satellite group switch off and on again on its dashboard.",
    link: true,
  });
  assert.deepEqual(candidateOffer(candidate({ state: CANDIDATE.offOrOlder, addable: false })), {
    add: false,
    text: "Satellite group is off there, or its Pinna is older.",
    link: true,
  });
  // A state this page doesn't know.
  assert.deepEqual(candidateOffer(candidate({ state: 0, addable: false })), {
    add: false,
    text: "It can't be added now.",
    link: true,
  });
  // Add follows `addable`, whatever the state.
  assert.equal(candidateOffer(candidate({ state: 0 })).add, true);
});

test("with no satellite listed, the search says why before it says it found none", () => {
  // Its first search hasn't finished since pinnad started.
  assert.deepEqual(noCandidates({ state: "looking" }, false), { text: "Looking for other satellites…", tone: "status" });
  const why = "Couldn't look for other satellites: no route to host. The satellite tries again every minute.";
  assert.deepEqual(noCandidates({ state: "failed", why }, true), { text: why, tone: "alert" });
  assert.deepEqual(noCandidates({ state: "done", onNetwork: false }, false), {
    text: "This satellite isn't on a network yet.",
    tone: "note",
  });
  assert.deepEqual(noCandidates({ state: "done", onNetwork: true }, false), {
    text: "The search found no other satellite. Satellites that run Pinna on the same subnet show up here.",
    tone: "note",
  });
  assert.deepEqual(noCandidates({ state: "done", onNetwork: true }, true), {
    text: "The search found no other satellite to add. Satellites that run Pinna on the same subnet show up here.",
    tone: "note",
  });
});

test("right after a Remove, the list says the member removed shows again after the next search", () => {
  // The search still has its record from inside the group.
  for (const inGroup of [true, false]) {
    assert.deepEqual(noCandidates({ state: "done", onNetwork: true }, inGroup, "Kitchen"), {
      text: "Kitchen shows here again after the next search, within about a minute.",
      tone: "note",
    });
  }
  // The search's own state still comes first.
  assert.equal(noCandidates({ state: "looking" }, true, "Kitchen").text, "Looking for other satellites…");
});

test("a member just removed that the search still lists from inside the group offers nothing until the next search", () => {
  // The group of two ended: its stale record names that group, which reads as another group.
  const stale = candidate({ state: CANDIDATE.inAnotherGroup, addable: false });
  assert.deepEqual(candidateOffer(stale, true), {
    add: false,
    text: "Removed. It can be added again after the next search, within about a minute.",
    link: false,
  });
  assert.equal(candidateOffer(stale).text.startsWith("In another group."), true);
  assert.equal(removedNote("Kitchen"), "Kitchen shows here again after the next search, within about a minute.");
});

test("the toast after a Remove says when the group ended", () => {
  assert.equal(removedText("Kitchen", { inGroup: true }), "Kitchen removed");
  assert.equal(removedText("Kitchen", { inGroup: false }), "Kitchen removed. The group ended.");
  assert.equal(removedText("Kitchen", undefined), "Kitchen removed");
});

test("while a change runs, its own button says so and the other buttons are disabled", () => {
  const kitchen = "02:00:00:00:00:02";
  const office = "02:00:00:00:00:03";
  assert.deepEqual(rowButton(true, null, "add", kitchen), { working: false, disabled: false });
  const adding = { kind: "add", mac: kitchen } as const;
  assert.deepEqual(rowButton(true, adding, "add", "02:00:00:00:00:02".toUpperCase()), { working: true, disabled: false });
  assert.deepEqual(rowButton(true, adding, "add", office), { working: false, disabled: true });
  assert.deepEqual(rowButton(true, adding, "remove", kitchen), { working: false, disabled: true });
  // A switch that changes blocks Add and Remove too.
  assert.deepEqual(rowButton(true, { kind: "on" }, "remove", kitchen), { working: false, disabled: true });
  // Nothing works while the satellite can't be reached.
  assert.deepEqual(rowButton(false, null, "remove", kitchen), { working: false, disabled: true });
});

test("a refusal shows by its row or card, or at the top once the view read after it took that away", () => {
  const own = member({ mac: "02:00:00:00:00:01", name: "Hall", thisSatellite: true });
  const kitchen = member();
  const office = candidate({ satellite: satellite("02:00:00:00:00:03") });
  const view = { on: true, inGroup: true, members: [own, kitchen], candidates: [office] };
  const alone = { on: true, inGroup: false, members: [], candidates: [] };
  const off = { on: false, inGroup: false, members: [], candidates: [] };
  const refusal = (change: Change, name: string, sentence: string): Refusal => ({ change, name, sentence });

  assert.equal(refusalElsewhere(null, view), null);

  // Remove: in the member's row while it is listed.
  const notMember = refusal(
    { kind: "remove", mac: kitchen.mac.toUpperCase() },
    "Kitchen",
    "The satellite isn't a member of this group. Reload the page.",
  );
  assert.equal(refusalElsewhere(notMember, view), null);
  assert.equal(
    refusalElsewhere(notMember, { ...view, members: [own] }),
    "Kitchen: The satellite isn't a member of this group. Reload the page.",
  );
  const noGroup = refusal({ kind: "remove", mac: kitchen.mac }, "Kitchen", "This satellite is in no group.");
  assert.equal(refusalElsewhere(noGroup, alone), "Kitchen: This satellite is in no group.");

  // Add: in the candidate's row while the search lists it.
  const add = refusal({ kind: "add", mac: "02:00:00:00:00:03" }, "Office", "Office is in another group.");
  assert.equal(refusalElsewhere(add, view), null);
  assert.equal(refusalElsewhere(add, { ...view, candidates: [] }), "Office: Office is in another group.");
  // Other satellites shows only with the switch on.
  assert.equal(refusalElsewhere(add, { ...view, on: false }), "Office: Office is in another group.");

  // A member outside the group: in its row under Members.
  const study = candidate({ satellite: satellite("02:00:00:00:00:08"), state: CANDIDATE.leftTheGroup });
  const again = refusal({ kind: "add", mac: "02:00:00:00:00:08" }, "Study", "Study is in another group.");
  assert.equal(refusalElsewhere(again, { ...view, members: [own, kitchen, member({ mac: "02:00:00:00:00:08" })], candidates: [study] }), null);

  // This satellite's own switches: the sentence alone. From the group shows
  // while the switch is on, in a group or not.
  const control = refusal({ kind: "control" }, "Control from the group", "The settings couldn't be saved.");
  assert.equal(refusalElsewhere(control, view), null);
  assert.equal(refusalElsewhere(control, alone), null);
  assert.equal(refusalElsewhere(control, off), "The settings couldn't be saved.");
  // The Satellite group switch is always shown.
  assert.equal(refusalElsewhere(refusal({ kind: "on" }, "Satellite group", "No."), off), null);
});

test("a refusal the view read since contradicts is dropped: the change happened all the same", () => {
  const own = member({ mac: "02:00:00:00:00:01", name: "Hall", thisSatellite: true });
  const kitchen = member({ mac: "02:00:00:00:00:02" });
  const view = { inGroup: true, members: [own, kitchen], candidates: [] };
  const alone = { inGroup: false, members: [], candidates: [] };

  // An Add whose answer timed out, but the satellite is a member now.
  const add = { kind: "add", mac: kitchen.mac.toUpperCase() } as const;
  assert.ok(happened(add, view));
  assert.ok(!happened(add, alone));
  assert.ok(!happened(add, { ...view, members: [own] }));
  // A member the search lists outside the group isn't back yet.
  const left = candidate({ satellite: satellite(kitchen.mac), state: CANDIDATE.leftTheGroup });
  assert.ok(!happened(add, { ...view, candidates: [left] }));

  // A Remove: the member is gone, or the group with it.
  const remove = { kind: "remove", mac: kitchen.mac } as const;
  assert.ok(!happened(remove, view));
  assert.ok(happened(remove, { ...view, members: [own] }));
  assert.ok(happened(remove, alone));

  // A switch's refusal stays.
  assert.ok(!happened({ kind: "control" }, view));
});

test("a refusal shows as a toast while no page shows the group", () => {
  const refused: Refusal = { change: { kind: "add", mac: "02:00:00:00:00:03" }, name: "Office", sentence: "Office is in another group." };
  assert.equal(refusalToast(refused, true), null);
  assert.equal(refusalToast(refused, false), "Office: Office is in another group.");
  const control: Refusal = { change: { kind: "control" }, name: "Control from the group", sentence: "No." };
  assert.equal(refusalToast(control, false), "No.");
});

test("a refresh shows only when nothing newer is shown", () => {
  const r = new Replies();
  // Two refreshes that answer out of order: the older one is dropped.
  const first = r.read();
  const second = r.read();
  assert.ok(r.fresh(second));
  assert.ok(!r.fresh(first));

  // A refresh asked before a change, or while it runs, answers too late:
  // the change's own answer is newer.
  const before = r.read();
  r.start();
  const during = r.read();
  assert.ok(!r.fresh(during), "nothing but the change's answer while it runs");
  r.end();
  assert.ok(!r.fresh(before));
  assert.ok(!r.fresh(during));
  const after = r.read();
  assert.ok(r.fresh(after));

  // A refused change ends the same way.
  r.start();
  r.end();
  assert.ok(r.fresh(r.read()));
});

test("removing the last other member ends the group", () => {
  const own = member({ mac: "02:00:00:00:00:01", name: "Hall", thisSatellite: true });
  const kitchen = member({ mac: "02:00:00:00:00:02" });
  const office = member({ mac: "02:00:00:00:00:03", name: "Office" });
  assert.ok(removeEndsGroup({ members: [own, kitchen] }, kitchen.mac));
  assert.ok(!removeEndsGroup({ members: [own, kitchen, office] }, kitchen.mac));
});

test("rows keep their keys, and a repeated one gets a count", () => {
  const keys = keyed(["a", "b", "a", "", ""], (x) => x).map((x) => x.key);
  assert.deepEqual(keys, ["a", "b", "a#1", "", "#1"]);
});
