// Standalone mode's setup as the page sees it: AssistantService's view and
// its edits (ADR 0017). Keys and MCP server addresses go to the satellite
// and never come back; nothing here keeps or logs them.
import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { type Assistant, AssistantService } from "../gen/pinna/v1/assistant_pb.ts";
import { errorSentence } from "./device.svelte.ts";

const client = createClient(AssistantService, createConnectTransport({ baseUrl: location.origin }));

class AssistantSetup {
  /** The setup as the satellite last answered it; null until it has. */
  view = $state.raw<Assistant | null>(null);
  /** Why the setup couldn't be read, while it can't. */
  error = $state<string | null>(null);

  async load() {
    try {
      this.view = (await client.getAssistant({})).assistant ?? null;
      this.error = null;
    } catch (e) {
      this.error = errorSentence(e);
    }
  }
}

export const assistant = new AssistantSetup();
