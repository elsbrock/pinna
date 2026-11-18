<!-- Assistant › Conversation: the language, the place, what the brain is told,
     follow-ups and the acknowledgment, and a box to type a question in. Each
     setting saves on its own; none needs a provider check. -->
<script lang="ts">
  import { onMount } from "svelte";

  import { assistant } from "../../lib/assistant.svelte.ts";
  import { brainPreset, chars, isStandalone, PROMPT_MAX, presetName, promptCount } from "../../lib/assistant.ts";
  import { device } from "../../lib/device.svelte.ts";
  import Group from "../../lib/Group.svelte";
  import Icon from "../../lib/Icon.svelte";
  import SwitchRow from "../../lib/SwitchRow.svelte";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const view = $derived(assistant.view);
  const standalone = $derived(isStandalone(s));

  onMount(() => void assistant.load());

  // Each field follows the satellite until someone types in it.
  let language = $state("");
  let languageTouched = $state(false);
  let languageError = $state<string | null>(null);
  $effect(() => {
    if (!languageTouched) language = view?.language ?? "";
  });
  async function saveLanguage() {
    languageError = await assistant.update({ language: language.trim() });
    if (!languageError) languageTouched = false;
  }

  let place = $state("");
  let placeError = $state<string | null>(null);
  async function savePlace(name: string) {
    placeError = await assistant.update({ place: name.trim() }, name.trim() ? "Place found and saved" : "Place cleared");
    if (!placeError) place = "";
  }
  const coords = (lat: number, lon: number) => `${lat.toFixed(2)}, ${lon.toFixed(2)}`;

  // The prompt starts from the voice rules while none of the owner's is set.
  let prompt = $state("");
  let promptTouched = $state(false);
  let promptError = $state<string | null>(null);
  const stored = $derived(view ? (view.prompt ?? view.voiceRules) : "");
  $effect(() => {
    if (!promptTouched) prompt = stored;
  });
  const tooLong = $derived(chars(prompt) > PROMPT_MAX);
  async function savePrompt() {
    // The voice rules typed back in mean the voice rules, which then follow updates.
    const patch = prompt === view?.voiceRules ? { defaultPrompt: true } : { prompt };
    promptError = await assistant.update(patch);
    if (!promptError) promptTouched = false;
  }
  async function resetPrompt() {
    promptError = await assistant.update({ defaultPrompt: true }, "Back to the voice rules");
    if (!promptError) promptTouched = false;
  }

  // The acknowledgment: what an agent brain says while it thinks.
  let phrase = $state("");
  let phraseTouched = $state(false);
  let ackError = $state<string | null>(null);
  $effect(() => {
    if (!phraseTouched) phrase = view?.ack?.phrase ?? "";
  });
  const agent = $derived(view ? brainPreset(view) : undefined);
  async function saveAck(enabled: boolean, text: string) {
    ackError = await assistant.update({ ack: { enabled, phrase: text.trim() } });
    if (!ackError) phraseTouched = false;
  }

  let question = $state("");
  let askError = $state<string | null>(null);
  async function ask() {
    askError = await assistant.askText(question);
    if (!askError) question = "";
  }
</script>

{#if view && s}
  <Group title="Language and place">
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void saveLanguage();
      }}
    >
      <div class="pn-field" data-invalid={languageError ? "" : undefined}>
        <label class="pn-field-label" for="language">Language</label>
        <div class="inline">
          <input
            id="language"
            class="pn-input is-mono short"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            placeholder="en"
            bind:value={language}
            disabled={!live}
            aria-describedby="language-hint"
            aria-invalid={languageError ? "true" : undefined}
            oninput={() => {
              languageTouched = true;
              languageError = null;
            }}
          />
          <button
            class="pn-btn pn-btn-primary"
            type="submit"
            disabled={!live || assistant.busy || language.trim() === view.language}
          >
            Save
          </button>
        </div>
        {#if languageError}<span class="pn-field-error" role="alert"><Icon name="alert" />{languageError}</span>{/if}
        <span id="language-hint" class="pn-field-hint">
          A tag such as en, de or en-US. Speech-to-text listens for it and the brain answers in it; the weather is in
          Fahrenheit for en-US. Empty leaves it to the services.
        </span>
      </div>
    </form>

    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void savePlace(place);
      }}
    >
      <div class="pn-field" data-invalid={placeError ? "" : undefined}>
        <label class="pn-field-label" for="place">Place</label>
        {#if view.place}
          <div class="found" id="place-found">
            <span>{view.place.name}</span>
            <span class="coords">{coords(view.place.latitude, view.place.longitude)}</span>
            <button
              class="pn-btn pn-btn-quiet pn-btn-sm"
              type="button"
              disabled={!live || assistant.busy}
              onclick={() => savePlace("")}>Clear</button
            >
          </div>
        {/if}
        <div class="inline">
          <input
            id="place"
            class="pn-input"
            autocomplete="off"
            placeholder={view.place ? "Another town or city" : "A town or city"}
            bind:value={place}
            disabled={!live}
            aria-describedby="place-hint"
            aria-invalid={placeError ? "true" : undefined}
            oninput={() => (placeError = null)}
          />
          <button class="pn-btn pn-btn-primary" type="submit" disabled={!live || assistant.busy || !place.trim()}>
            Find
          </button>
        </div>
        {#if placeError}<span class="pn-field-error" role="alert"><Icon name="alert" />{placeError}</span>{/if}
        <span id="place-hint" class="pn-field-hint">
          For the weather, and so the brain knows where it is. The satellite looks the name up with Open-Meteo and keeps
          the place it found.
        </span>
      </div>
    </form>

    <a class="pn-row" href="#/system">
      <span class="pn-row-text">
        <span class="pn-row-label">Time zone</span>
        <span class="pn-row-hint">The brain gets the local time with each request. Set it under System.</span>
      </span>
      <span class="pn-row-value is-mono">{s.settings?.timeZone || "UTC"}</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <Group title="Prompt">
    {#snippet foot()}
      The satellite adds the local time, the place, the language and its name after the prompt, with every request.
    {/snippet}
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void savePrompt();
      }}
    >
      <div class="pn-field" data-invalid={promptError || tooLong ? "" : undefined}>
        <div class="label-line">
          <label class="pn-field-label" for="prompt">What the brain is told</label>
          <span id="prompt-count" class="count" class:is-over={tooLong}>{promptCount(prompt)}</span>
        </div>
        <textarea
          id="prompt"
          class="pn-input prompt"
          rows="7"
          spellcheck="true"
          bind:value={prompt}
          disabled={!live}
          aria-describedby="prompt-count prompt-hint"
          aria-invalid={promptError || tooLong ? "true" : undefined}
          oninput={() => {
            promptTouched = true;
            promptError = null;
          }}
        ></textarea>
        {#if promptError}<span class="pn-field-error" role="alert"><Icon name="alert" />{promptError}</span>{/if}
        <span id="prompt-hint" class="pn-field-hint">
          {#if view.prompt === undefined}
            These are the satellite's voice rules: short, plain spoken answers. Change them as you like.
          {:else if view.prompt === ""}
            Empty: the brain gets only what the satellite adds.
          {:else}
            Your own prompt. Reset brings back the satellite's voice rules.
          {/if}
        </span>
      </div>
      <div class="inline">
        <button
          class="pn-btn pn-btn-primary"
          type="submit"
          disabled={!live || assistant.busy || tooLong || prompt === stored}
        >
          Save
        </button>
        <button
          class="pn-btn"
          type="button"
          disabled={!live || assistant.busy || (view.prompt === undefined && prompt === stored)}
          onclick={resetPrompt}>Reset to the voice rules</button
        >
      </div>
    </form>
  </Group>

  <Group title="Replies">
    <SwitchRow
      id="follow-ups"
      label="Follow-ups"
      checked={view.followUps}
      disabled={!live}
      busy={assistant.busy}
      hint="After each reply the satellite listens for 4 seconds more, without the wake word, so you can answer. Silence ends the conversation."
      toggle={(on) => assistant.update({ followUps: on })}
    />
    <SwitchRow
      id="ack"
      label="Acknowledgment"
      checked={!!view.ack?.enabled}
      disabled={!live}
      busy={assistant.busy}
      hint={agent?.handOff
        ? `${presetName(agent.id)} can take a while: the satellite says this when no answer has come after 3 seconds.`
        : "Only a brain that is an agent, such as Hermes or OpenClaw, can take long enough for it."}
      toggle={(on) => saveAck(on, phrase)}
    />
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void saveAck(!!view.ack?.enabled, phrase);
      }}
    >
      <div class="pn-field" data-invalid={ackError ? "" : undefined}>
        <label class="pn-field-label" for="ack-phrase">Phrase</label>
        <div class="inline">
          <input
            id="ack-phrase"
            class="pn-input"
            autocomplete="off"
            maxlength="100"
            bind:value={phrase}
            disabled={!live}
            aria-describedby="ack-said"
            aria-invalid={ackError ? "true" : undefined}
            oninput={() => {
              phraseTouched = true;
              ackError = null;
            }}
          />
          <button
            class="pn-btn pn-btn-primary"
            type="submit"
            disabled={!live || assistant.busy || phrase.trim() === (view.ack?.phrase ?? "")}
          >
            Save
          </button>
        </div>
        {#if ackError}<span class="pn-field-error" role="alert"><Icon name="alert" />{ackError}</span>{/if}
        <span id="ack-said" class="pn-field-hint">
          {#if view.ackSpoken && view.ackSpoken !== view.ack?.phrase}
            Said as “{view.ackSpoken}” in the conversation's language.
          {:else if view.ackSpoken}
            It says “{view.ackSpoken}”.
          {/if}
        </span>
      </div>
    </form>
  </Group>

  <Group title="Ask a question">
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void ask();
      }}
    >
      <div class="pn-field" data-invalid={askError ? "" : undefined}>
        <label class="pn-field-label" for="question">Question</label>
        <div class="inline">
          <input
            id="question"
            class="pn-input"
            autocomplete="off"
            maxlength="1000"
            placeholder="What's the weather tomorrow?"
            bind:value={question}
            disabled={!live || !standalone}
            aria-describedby="question-hint"
            aria-invalid={askError ? "true" : undefined}
            oninput={() => (askError = null)}
          />
          <button
            class="pn-btn pn-btn-primary"
            type="submit"
            disabled={!live || !standalone || assistant.busy || !question.trim()}
          >
            Ask
          </button>
        </div>
        {#if askError}<span class="pn-field-error" role="alert"><Icon name="alert" />{askError}</span>{/if}
        <span id="question-hint" class="pn-field-hint">
          {#if standalone}
            The brain gets it as if you had said it, and the satellite answers aloud.
          {:else}
            Works in standalone mode. Until then, the checks under Services and Play sample test the services.
          {/if}
        </span>
      </div>
    </form>
  </Group>
{:else if assistant.error}
  <div class="pn-notice" data-tone="danger" role="alert">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">The conversation settings couldn't be read</p>
      <p>{assistant.error}</p>
      <div><button class="pn-btn" type="button" onclick={() => assistant.load()}>Try again</button></div>
    </div>
  </div>
{:else}
  <span class="pn-skeleton" style:height="200px"></span>
  <span class="pn-skeleton" style:height="240px"></span>
{/if}

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .inline {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .inline input {
    flex: 1 1 220px;
    width: auto;
  }
  .inline input.short {
    flex: 0 1 10em;
  }
  .found {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1) var(--space-3);
  }
  .coords {
    font-family: var(--font-mono);
    font-size: 13px;
    color: var(--ink-muted);
  }
  .label-line {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-1) var(--space-3);
  }
  .count {
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 18px;
    color: var(--ink-muted);
    font-variant-numeric: tabular-nums;
  }
  .count.is-over {
    color: var(--danger);
    font-weight: 650;
  }
  .prompt {
    height: auto;
    min-height: 10em;
    padding: var(--space-2) var(--space-3);
    line-height: 22px;
    resize: vertical;
  }
</style>
