// The people the satellite recognizes, as the People page sees them:
// PeopleService's view and its edits (ADR 0038). After every change the view
// is read again, since close pairs and the recognition state can change with it.
import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { type GetPeopleResponse, PeopleService } from "../gen/pinna/v1/people_pb.ts";
import { errorSentence } from "./device.svelte.ts";

const client = createClient(PeopleService, createConnectTransport({ baseUrl: location.origin }));

/** Said when an edit named a revision that changed meanwhile. */
export const CHANGED_ELSEWHERE = "Changed in another window. This shows the current version.";

class People {
  /** The list as the satellite last answered it; null until it has. */
  view = $state.raw<GetPeopleResponse | null>(null);
  /** Why the list couldn't be read, while it can't. */
  error = $state<string | null>(null);
  busy = $state(false);
  /** An enrollment waits for a press or listens: the forms and rows stay as they are until it ends. */
  enrolling = $state(false);

  async load() {
    try {
      this.view = await client.getPeople({});
      this.error = null;
    } catch (e) {
      this.error = errorSentence(e);
    }
  }

  /** Run an edit, then read the list again. Resolves to the sentence that
   *  refused it, for the form to show, or null. */
  async #edit(run: () => Promise<unknown>): Promise<string | null> {
    this.busy = true;
    try {
      await run();
      await this.load();
      return null;
    } catch (e) {
      // Made on a revision that changed meanwhile: show the current one.
      if (ConnectError.from(e).code === Code.Aborted) {
        await this.load();
        return CHANGED_ELSEWHERE;
      }
      return errorSentence(e);
    } finally {
      this.busy = false;
    }
  }

  /** Add a person (`id` empty) or change one, on the revision it started from. */
  async setPerson(id: string, revision: bigint, name: string, instructions: string) {
    let saved = id;
    const error = await this.#edit(async () => {
      saved = (await client.setPerson({ id, revision, name, instructions })).person?.id ?? id;
    });
    return { error, id: saved };
  }

  deletePerson(id: string) {
    return this.#edit(() => client.deletePerson({ id }));
  }

  setEveryoneElse(instructions: string, revision: bigint) {
    return this.#edit(() => client.setEveryoneElse({ instructions, revision }));
  }

  /** Enroll a voice with the grant of a confirmation. The stream ends with
   *  `done` or `failed`; aborting `signal` stores nothing. */
  enroll(id: string, grant: string, signal: AbortSignal) {
    return client.enroll({ id, grant }, { signal });
  }
}

export const people = new People();
