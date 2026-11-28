import assert from "node:assert/strict";
import { test } from "node:test";

import type { Status } from "../gen/pinna/v1/device_pb.ts";
import {
  addReason,
  closeWarning,
  countOf,
  ENROLL_PASSAGE,
  enrollBlocked,
  enrolledLine,
  enrollHeading,
  isNameRefusal,
  removeNote,
  levelPercent,
  nameList,
  nameProblem,
  namesOf,
  RECOGNITION,
  recognitionLine,
  VOICE,
  voiceLine,
} from "./people.ts";

const person = (id: string, name: string) => ({ id, name });
const people = [person("p1", "Anna"), person("p2", "Ben"), person("p3", "Carl")];

test("each voice state reads in words", () => {
  assert.equal(voiceLine(VOICE.none), "Not enrolled");
  assert.equal(voiceLine(VOICE.enrolled), "Enrolled");
  assert.equal(voiceLine(VOICE.olderCapture), "Enrolled with older settings. Enroll again for better recognition.");
  assert.equal(voiceLine(VOICE.otherModel), "Enroll again: the speaker model changed.");
  assert.equal(voiceLine(0), "Not enrolled");
});

test("each recognition state has its line, and a failure is an error with the satellite's reason", () => {
  assert.deepEqual(recognitionLine({ state: RECOGNITION.notNeeded, reason: "" }), {
    text: "Nobody is enrolled, so the satellite doesn't recognize voices yet.",
    error: false,
  });
  // People are listed, but none has a usable voiceprint: after a model change, say what to do.
  assert.deepEqual(recognitionLine({ state: RECOGNITION.notNeeded, reason: "" }, true), {
    text: "Nobody has a voiceprint the satellite can use, so it doesn't recognize voices yet. Enroll each person.",
    error: false,
  });
  assert.deepEqual(recognitionLine({ state: RECOGNITION.loading, reason: "" }), {
    text: "Loading the speaker model. Until it's ready, requests use Everyone else.",
    error: false,
  });
  assert.deepEqual(recognitionLine({ state: RECOGNITION.ready, reason: "" }), { text: "Voice recognition is on.", error: false });
  assert.deepEqual(recognitionLine({ state: RECOGNITION.unavailable, reason: "The model file is missing." }), {
    text: "The model file is missing.",
    error: true,
  });
  assert.equal(recognitionLine({ state: RECOGNITION.unavailable, reason: "" })?.error, true);
  assert.equal(recognitionLine({ state: 0, reason: "" }), null);
  assert.equal(recognitionLine(undefined), null);
});

test("a close pair names the people, skips one removed meanwhile, and says nothing for none", () => {
  assert.equal(
    closeWarning(["p2"], people),
    "May be mixed up with Ben. Enroll both again, closer to the satellite.",
  );
  assert.equal(
    closeWarning(["p2", "p3"], people),
    "May be mixed up with Ben and Carl. Enroll them all again, closer to the satellite.",
  );
  assert.deepEqual(namesOf(["p9", "p2"], people), ["Ben"]);
  assert.equal(closeWarning(["p9"], people), null, "an id that is gone");
  assert.equal(closeWarning([], people), null);
  assert.equal(nameList(["A", "B", "C"]), "A, B and C");
});

test("adding stops at the limit, with its reason", () => {
  assert.equal(addReason(9, 10), null);
  assert.equal(addReason(10, 10), "At most 10 people.");
  assert.equal(addReason(0, 0), null, "limits not read yet");
});

test("a name is checked before it is sent, the rest is the satellite's", () => {
  assert.equal(nameProblem("  ", 24), "Enter a name.");
  assert.equal(nameProblem("Anna", 24), null);
  assert.equal(nameProblem("a".repeat(25), 24), "A name has at most 24 characters.");
  assert.equal(nameProblem("🙂".repeat(24), 24), null, "code points, as the satellite counts");
});

test("the counter counts code points against the limit", () => {
  assert.equal(countOf("héllo", 1000), "5 of 1,000");
  assert.equal(countOf("🙂", 1000), "1 of 1,000");
});

test("the level meter runs from -60 dBFS to 0", () => {
  assert.equal(levelPercent(-90), 0);
  assert.equal(levelPercent(-60), 0);
  assert.equal(levelPercent(-30), 50);
  assert.equal(levelPercent(0), 100);
  assert.equal(levelPercent(6), 100);
  assert.equal(levelPercent(-Infinity), 0);
  assert.equal(levelPercent(Number.NaN), 0);
});

test("a saved enrollment says whose voice and how much speech", () => {
  assert.equal(enrolledLine("Anna", 24.4), "Saved Anna's voice, from 24 seconds of speech.");
});

test("the passage is about 30 seconds of reading and names nothing", () => {
  const words = ENROLL_PASSAGE.split(/\s+/).length;
  assert.ok(words >= 70 && words <= 100, `${words} words`);
});

test("an enrollment can't start while the microphones are in use elsewhere", () => {
  const recording = (kind: number) =>
    ({ recording: { $typeName: "pinna.v1.Recording", secondsLeft: 9, from: "192.168.1.30", kind } }) as unknown as Pick<
      Status,
      "recording"
    >;
  assert.equal(enrollBlocked({ recording: undefined }, false), null);
  assert.equal(enrollBlocked(null, false), null);
  assert.equal(enrollBlocked(recording(1), false), "A browser at 192.168.1.30 is enrolling a voice. Try again when it's done.");
  assert.equal(enrollBlocked(recording(0), false), "A recording is running. Try again when it's done.");
  assert.equal(enrollBlocked(recording(1), true), null, "this dashboard runs it");
});

test("the heading, the removal note and the name refusals", () => {
  assert.equal(enrollHeading("Anna", false), "Enroll Anna's voice");
  assert.equal(enrollHeading("Anna", true), "Enroll Anna's voice again");
  assert.equal(removeNote(VOICE.none), "Their name and extra instructions are deleted from this satellite.");
  assert.equal(
    removeNote(VOICE.enrolled),
    "Their name, extra instructions and voiceprint are deleted from this satellite.",
  );
  assert.equal(isNameRefusal("Ben is on the list already. Choose another name."), true);
  assert.equal(isNameRefusal("Everyone else is the profile for anyone who isn't on the list. Choose another name."), true);
  assert.equal(isNameRefusal("Instructions can have at most 1,000 characters. Shorten them."), false);
});
