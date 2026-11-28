// What the People page says (ADR 0038): the voice and recognition states in
// words, close pairs by name, the limits' reasons, and the enrollment's text.
// Pure, so `node --test` covers it; the generated enums can't be loaded
// there, so their values are copied here.
import type { Status } from "../gen/pinna/v1/device_pb.ts";
import type { Person, Recognition } from "../gen/pinna/v1/people_pb.ts";
import { chars } from "./assistant.ts";
import { recordingKind } from "./view.ts";

/** pinna.v1.VoiceState values. */
export const VOICE = { none: 1, enrolled: 2, otherModel: 3, olderCapture: 4 } as const;
/** pinna.v1.RecognitionState values. */
export const RECOGNITION = { notNeeded: 1, loading: 2, ready: 3, unavailable: 4 } as const;

/** How long the satellite listens during an enrollment, until `started` says. */
export const ENROLL_SECONDS = 30;
/** The seconds of speech a voiceprint needs. */
export const ENROLL_SPEECH_NEEDED = 10;

/** A plain passage for about 30 seconds of reading aloud, with no names or brands. */
export const ENROLL_PASSAGE =
  "On a quiet morning the town wakes up slowly. The baker opens her shop, and the smell of warm bread drifts down " +
  "the street. A few people walk to work, and a bus stops at the corner to let them on. By the river, an old man " +
  "feeds the ducks and watches the water move. Later the sun climbs above the roofs, the market fills with voices, " +
  "and children run between the stalls. Nobody is in a hurry.";

/** The state of a person's voiceprint, in words. */
export function voiceLine(voice: number): string {
  switch (voice) {
    case VOICE.enrolled:
      return "Enrolled";
    case VOICE.olderCapture:
      return "Enrolled with older settings. Enroll again for better recognition.";
    case VOICE.otherModel:
      return "Enroll again: the speaker model changed.";
    default:
      return "Not enrolled";
  }
}

/** The one line about whether recognition runs; null when there is nothing to add. `listed`: people
 *  are on the list, so NOT_NEEDED means none of them has a voiceprint it can use, for example after
 *  the speaker model changed. */
export function recognitionLine(
  r: Pick<Recognition, "state" | "reason"> | undefined,
  listed = false,
): { text: string; error: boolean } | null {
  switch (r?.state) {
    case RECOGNITION.notNeeded:
      return {
        text: listed
          ? "Nobody has a voiceprint the satellite can use, so it doesn't recognize voices yet. Enroll each person."
          : "Nobody is enrolled, so the satellite doesn't recognize voices yet.",
        error: false,
      };
    case RECOGNITION.loading:
      return { text: "Loading the speaker model. Until it's ready, requests use Everyone else.", error: false };
    case RECOGNITION.ready:
      return { text: "Voice recognition is on.", error: false };
    case RECOGNITION.unavailable:
      return {
        text: r.reason || "The speaker model couldn't be loaded. Requests use Everyone else.",
        error: true,
      };
    default:
      return null;
  }
}

/** "Ben", "Ben and Carl", "Ben, Carl and Dan". */
export function nameList(names: string[]): string {
  if (names.length < 2) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** The names of the people `ids` point at; an id that is gone meanwhile is skipped. */
export function namesOf(ids: readonly string[], people: readonly Pick<Person, "id" | "name">[]): string[] {
  return ids.flatMap((id) => people.find((p) => p.id === id)?.name ?? []);
}

/** A person's close-pair warning, or null when no one they could be mixed up with is on the list. */
export function closeWarning(ids: readonly string[], people: readonly Pick<Person, "id" | "name">[]): string | null {
  const names = namesOf(ids, people);
  if (!names.length) return null;
  return `May be mixed up with ${nameList(names)}. Enroll ${names.length === 1 ? "both" : "them all"} again, closer to the satellite.`;
}

/** Why Add a person is disabled; null while there is room. */
export function addReason(count: number, max: number): string | null {
  return max > 0 && count >= max ? `At most ${max} people.` : null;
}

/** "12 of 1,000" for a field with a limit. */
export function countOf(text: string, max: number): string {
  const n = (x: number) => x.toLocaleString("en");
  return `${n(chars(text))} of ${n(max)}`;
}

/** What a name needs before it is sent: the satellite checks the rest, duplicates included. */
export function nameProblem(name: string, max: number): string | null {
  const n = name.trim();
  if (!n) return "Enter a name.";
  if (max > 0 && chars(n) > max) return `A name has at most ${max} characters.`;
  return null;
}

/** The meter's fill, 0 to 100, for a level in dBFS: silence at -60 and below, full scale at 0. */
export function levelPercent(dbfs: number): number {
  if (!Number.isFinite(dbfs)) return 0;
  return Math.round(Math.min(1, Math.max(0, (dbfs + 60) / 60)) * 100);
}

/** What an enrollment that saved says. */
export function enrolledLine(name: string, seconds: number): string {
  return `Saved ${name}'s voice, from ${Math.round(seconds)} seconds of speech.`;
}

/** Why an enrollment can't start while something else uses the microphones; null when nothing does
 *  or this dashboard runs it. */
export function enrollBlocked(s: Pick<Status, "recording"> | null | undefined, here: boolean): string | null {
  const kind = recordingKind(s);
  if (!kind || here) return null;
  return kind === "enrollment"
    ? `A browser at ${s?.recording?.from} is enrolling a voice. Try again when it's done.`
    : "A recording is running. Try again when it's done.";
}

/** The enrollment panel's heading. */
export function enrollHeading(name: string, again: boolean): string {
  return again ? `Enroll ${name}'s voice again` : `Enroll ${name}'s voice`;
}

/** What removing a person deletes, for the dialog. */
export function removeNote(voice: number): string {
  return voice === VOICE.none || voice === VOICE.otherModel
    ? "Their name and extra instructions are deleted from this satellite."
    : "Their name, extra instructions and voiceprint are deleted from this satellite.";
}

/** The satellite's refusal is about the name: it names the name, or Everyone else. */
export function isNameRefusal(sentence: string): boolean {
  return /\bname\b/i.test(sentence);
}
