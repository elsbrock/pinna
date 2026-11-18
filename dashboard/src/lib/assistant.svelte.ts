// Standalone mode's setup as the page sees it: AssistantService's view and
// its edits (ADR 0017). Keys and MCP server addresses go to the satellite
// and never come back; nothing here keeps or logs them.
import type { MessageInitShape } from "@bufbuild/protobuf";
import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import {
  type Assistant,
  AssistantService,
  AssistantSlot,
  type SetConnectionRequestSchema,
  type SetMcpServerRequestSchema,
  type UpdateAssistantRequestSchema,
} from "../gen/pinna/v1/assistant_pb.ts";
import type { SlotKey } from "./assistant.ts";
import { errorSentence } from "./device.svelte.ts";
import { toasts } from "./toast.svelte.ts";

const client = createClient(AssistantService, createConnectTransport({ baseUrl: location.origin }));

const SLOT: Record<SlotKey, AssistantSlot> = {
  stt: AssistantSlot.STT,
  brain: AssistantSlot.BRAIN,
  tts: AssistantSlot.TTS,
};

type Refused = { sentence: string; code: Code };

class AssistantSetup {
  /** The setup as the satellite last answered it; null until it has. */
  view = $state.raw<Assistant | null>(null);
  /** Why the setup couldn't be read, while it can't. */
  error = $state<string | null>(null);
  busy = $state(false);
  /** The sentence of a slot's check that failed on this page, until it passes. */
  failed = $state.raw<Partial<Record<SlotKey, string>>>({});

  async load() {
    try {
      this.view = (await client.getAssistant({})).assistant ?? null;
      this.error = null;
    } catch (e) {
      this.error = errorSentence(e);
    }
  }

  /** Run an edit; the reply's view wins. Resolves to the refusal, for the
   *  form to show, or null. */
  async #edit(run: () => Promise<{ assistant?: Assistant }>, done?: string): Promise<Refused | null> {
    this.busy = true;
    try {
      const reply = await run();
      if (reply.assistant) this.view = reply.assistant;
      if (done) toasts.show(done);
      return null;
    } catch (e) {
      const code = ConnectError.from(e).code;
      // Made on a revision that changed meanwhile: show the current one.
      if (code === Code.Aborted) void this.load();
      return { sentence: errorSentence(e), code };
    } finally {
      this.busy = false;
    }
  }

  async setConnection(edit: MessageInitShape<typeof SetConnectionRequestSchema>, done: string) {
    const refused = await this.#edit(() => client.setConnection(edit), done);
    // The slots on it were checked again with the change: an old failure
    // no longer says anything.
    if (!refused && edit.id) {
      const view = this.view;
      this.failed = Object.fromEntries(
        Object.entries(this.failed).filter(([key]) => view?.[key as SlotKey]?.connection !== edit.id),
      );
    }
    return refused?.sentence ?? null;
  }

  async deleteConnection(id: string, revision: bigint) {
    return (await this.#edit(() => client.deleteConnection({ id, revision }), "Connection removed"))?.sentence ?? null;
  }

  /** Point a slot at a connection; it is saved only when its check passes. */
  async setSlot(key: SlotKey, connection: string, model: string) {
    const refused = await this.#edit(
      () => client.setSlot({ slot: SLOT[key], connection, model }),
      "Checked and saved",
    );
    if (!refused) {
      const { [key]: _passed, ...others } = this.failed;
      this.failed = others;
    } else if (refused.code === Code.FailedPrecondition) {
      this.failed = { ...this.failed, [key]: refused.sentence };
    }
    return refused?.sentence ?? null;
  }

  async update(patch: MessageInitShape<typeof UpdateAssistantRequestSchema>, done = "Saved") {
    return (await this.#edit(() => client.updateAssistant(patch), done))?.sentence ?? null;
  }

  /** Add or change one of the owner's MCP servers. Its address is sent
   *  only when typed; the satellite answers with its host alone. */
  async setMcpServer(edit: MessageInitShape<typeof SetMcpServerRequestSchema>, done: string) {
    return (await this.#edit(() => client.setMcpServer(edit), done))?.sentence ?? null;
  }

  async deleteMcpServer(id: string, revision: bigint) {
    return (await this.#edit(() => client.deleteMcpServer({ id, revision }), "MCP server removed"))?.sentence ?? null;
  }

  /** Ask the brain a typed question, as if it was spoken; the satellite
   *  answers aloud. Standalone mode only. */
  async askText(text: string) {
    return (await this.#edit(() => client.askText({ text }).then(() => ({}))))?.sentence ?? null;
  }

  /** Say `text` with the text-to-speech slot; resolves once it sounds. */
  async playSample(text: string) {
    return (await this.#edit(() => client.playSample({ text }).then(() => ({}))))?.sentence ?? null;
  }
}

export const assistant = new AssistantSetup();
