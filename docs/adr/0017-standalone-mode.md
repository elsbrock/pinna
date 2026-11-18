# ADR 0017: Standalone mode runs the voice request on services the owner picks

Status: proposed (2026-11-16), built on its branch (2026-11-18), not yet on `main`. Amends ADR 0005,
ADR 0007, ADR 0013 and ADR 0020; builds on ADR 0008, ADR 0011, ADR 0018, ADR 0021 and ADR 0022.

## Context

- Today Home Assistant runs every voice request. pinnad streams the microphone over the ESPHome API.
  Home Assistant detects the end of speech, transcribes, answers, synthesises the reply and sends its URL.
  It also counts the timers; pinnad only shows them and rings.
- The wake word interrupts a request (while it thinks or speaks) or an announcement and starts a new
  request. While a timer alarm rings, it stops the alarm and starts no request.
- pinnad sets the microphone gain itself (AGC, ADR 0004). It keeps room noise under the floor of the wake
  word detector's energy VAD.
- goals.md listed a standalone mode under "Later" from the start. Pinna's parts were kept separate so that
  it stays possible.
- The device has about 480 MB of RAM and four Cortex-A53 cores. Speech recognition, a language model and
  natural speech synthesis don't fit on it. They run elsewhere.
- What owners already use:
  - Language models:
    - hosted ones through OpenRouter, OpenAI, Anthropic or Google;
    - local ones through Ollama or LM Studio.
    - Most speak OpenAI's chat completions format, directly or through OpenRouter. Anthropic and Gemini
      have their own APIs, with features their compatible endpoints lack.
  - Speech:
    - transcription from OpenAI and Groq;
    - speech from OpenAI;
    - both from ElevenLabs.
    - Self-hosted speech servers copy OpenAI's audio API.
  - Agents that keep their own long-term memory and tools:
    - Hermes Agent: an API server on port 8642 that speaks OpenAI chat completions, with a bearer key. Its
      endpoint keeps no conversation between requests: the caller sends the earlier turns.
    - OpenClaw: a gateway whose `/v1/chat/completions` is off by default. Its `model` field names an
      agent, such as `openclaw/default`. Each request is a new session unless it carries a `user` string,
      from which the gateway derives a stable session.
    - Both also run as MCP servers, but over stdio only, and they offer their messaging channels, not a
      question and its answer.
  - MCP servers for tools. Many run as local Node or Python programs over stdio, which the read-only root
    can't run.
- pinnad uses reqwest 0.12 with rustls on ring; the static musl build avoids aws-lc. Rust's crates for
  language models and MCP (genai, rig, rmcp, async-openai) need reqwest 0.13, which can also run on ring.
  None of them does speech for more than one provider.
- Open-source apps can now use a ChatGPT plan through "Sign in with ChatGPT". This covers model requests
  only, not speech, and needs a client ID from OpenAI.
- The dashboard has no login (ADR 0005). Anyone on the LAN can use it.
  - It already sets up a release image without a file or SSH: the name and the time zone (ADR 0007),
    Wi-Fi over USB (ADR 0008), the hostname and a factory reset (ADR 0010).
  - It plays a URL on the media lane. A URL replaces what plays only once it has audio; one that fails
    leaves it playing and reports why (ADR 0011).
  - The listener also serves Prometheus metrics, which count Assist runs and errors (ADR 0020).
  - The settings it writes live in `settings.toml`, written durably. Keys that an older image doesn't know
    survive a rollback. Every setting is also a Home Assistant entity, unless Home Assistant has its own way
    (ADR 0007).
  - It never shows or takes the ESPHome encryption key; Home Assistant sets that (ADR 0018). Until it
    does, the ESPHome API accepts plaintext connections.

## Decision

### Two modes

- **The device runs in Home Assistant mode or in standalone mode.**
  - `mode` is a setting in `settings.toml` (ADR 0007).
  - A missing key means Home Assistant, so an older image reads the file as before.
- **Home Assistant can stay connected in standalone mode, without the voice.** The ESPHome API keeps
  running, so an owner who has Home Assistant keeps the media player (announcements included), volume,
  mute, the ring and the buttons there, while requests go to the owner's services. Without Home Assistant,
  the API is simply unused.
  - pinnad reports no voice assistant features (`voice_assistant_feature_flags` 0), so Home Assistant
    shows no Assist satellite.
  - pinnad sends no microphone audio over the ESPHome API in standalone mode, even to a client that
    subscribes to the voice assistant.
- **Standalone mode is switched on only when it is ready.** The dashboard switches only once all three
  slots are set and have passed their check, so a first run never leaves the device without a voice.
- **Changing the mode:**
  - ends a running request first, stops a ringing alarm, clears all timers and forgets the
    conversation. Home Assistant can't finish its timers in standalone mode, and it never learns about
    the device's;
  - updates the device info and closes the API connection once the device is idle, as a rename does
    (ADR 0007). Home Assistant reconnects and reads the new features.
- **In standalone mode the dashboard always chooses the wake words** (amends ADR 0013). Without the Assist
  satellite, Home Assistant shows no wake word dropdowns.
- **The standalone settings are on the dashboard only** (amends ADR 0007). The connections, slots,
  language, place, the prompt, the tool switches, stations and MCP servers have no Home Assistant entity:
  - Home Assistant takes no part in a standalone request;
  - keys never leave the device.
- **The mode is also a Home Assistant entity**, the select *Assistant* with *Home Assistant* and
  *Standalone*, so an owner who has both can switch from Home Assistant and back. In standalone mode the
  brain and MCP clients can also switch back with a tool (below). It stays available in
  both modes, since Home Assistant stays connected without the voice. It follows the same rule as the
  dashboard: *Standalone* is refused until the three slots are ready, and the select then reports the
  mode it stayed in.
- **The configuration has its own files:** `standalone.toml` beside `settings.toml`, `secrets.toml` for
  keys (below) and `timers.toml` for device timers. Each is loaded entry by entry: a top-level key this
  image doesn't know is kept and written back, an array element or table entry it can't read is kept
  and written back, and a known value it can't read falls back to its default without resetting the
  rest. Unknown fields inside an entry it can read are not kept.

### The pipeline

- **Three slots: speech-to-text, brain and text-to-speech.**
  - Each slot names a connection and a model or voice.
  - A connection is a base URL and an optional key.
  - One connection can fill several slots.
  - Presets fill in the URL for known providers, and OpenClaw's fixed model. The owner types the other
    models and voices as the service lists them.
- **pinnad supports protocols, not providers:**
  - speech-to-text: OpenAI-compatible `/v1/audio/transcriptions` and ElevenLabs;
  - brain: OpenAI-compatible `/v1/chat/completions`, Anthropic's Messages API and Gemini's API, streamed,
    with tool calls;
  - text-to-speech: OpenAI-compatible `/v1/audio/speech` and ElevenLabs.

  A provider that speaks one of these is a preset. One with its own API is a new adapter.
  The presets cover OpenAI, OpenRouter, Anthropic, Gemini, Groq, Mistral, ElevenLabs, Ollama, LM Studio,
  Hermes and OpenClaw.
- **Presets, not kinds of brain.** A preset sets how pinnad treats a brain connection:
  - whether pinnad runs its tool loop with its own tools and the owner's MCP servers (a model), or hands
    the request off (an agent such as Hermes or OpenClaw, which owns its tools and memory);
  - who keeps the conversation;
  - the time limit and the acknowledgment.

  Handing off is one way. An agent that wants the device's tools reaches them over `/mcp` (below). The
  brain's Advanced settings override the preset for any brain, meant for a server of the owner's own:
  who runs the tools, how much of the conversation it gets, and the first-text limit (1 to 300 s).
- **Thinking is the owner's choice.** A brain on the OpenAI-compatible protocol gets `reasoning_effort`
  only when the owner picks *None*, *Low*, *Medium* or *High* under Advanced; unset sends nothing, so a
  model or server that doesn't know the parameter keeps working. Some reasoning models need it: OpenAI's
  `gpt-6-luna` calls tools over Chat Completions only with `none`. A brain request that fails with 400 on
  that protocol names the setting. Anthropic's and Gemini's thinking controls differ per model and are
  not offered yet.
- **A local server's address is the computer it runs on.** For Ollama, LM Studio and Hermes the owner
  enters a name or address, with or without a scheme, port or path; the preset fills in what is missing
  (`192.168.1.5` becomes `http://192.168.1.5:11434/v1` for Ollama). OpenClaw's gateway has no fixed
  port, so the owner enters the computer and the port, and the preset adds `/v1`. Any other server is
  entered in full.
- **pinnad runs the tool loop itself**, not through an agent framework:
  - it sends the prompt, the conversation and the new turn, and streams the answer;
  - text goes to text-to-speech sentence by sentence while tool calls are collected;
  - it runs the tool calls, sends the results back and asks again: at most 5 rounds, at most 8 calls a
    round, 10 s per tool. The last round still offers the tools, but forbids calling them, so the model
    answers in words;
  - it offers at most 64 tools: the device's first, then the owner's MCP servers' (at most 32 each).
    A tool's output is cut at 8 KiB;
  - a tool that fails goes back to the model as an error result.
- **The prompt is the owner's.**
  - The dashboard edits it in full, at most 4,000 characters. It starts as pinnad's voice rules: the
    reply is spoken, so short, plain text without markdown, lists or links; numbers as they are said; a
    question only when an answer is wanted; and things are done only by calling a tool, never claimed
    without one (a model may say "timer set" without calling the tool, which the owner can't hear). A
    reset brings them back.
  - An edit names the version it started from, and is refused when the prompt changed since, so a save
    in one window doesn't silently replace another's. An edit without one, from a script, isn't checked.
  - pinnad adds the context after it with each request: the local time and time zone, the place, the
    language and the satellite's name.
  - Models and agents get the same prompt. An agent keeps its own persona; an empty prompt sends only the
    context.
- **The conversation lives in RAM only.**
  - After every spoken reply the device listens 4 s more without a wake word, so the owner can answer.
    Silence closes that window; there is no count. The owner can switch follow-ups off.
  - Nothing is written to disk. The conversation is forgotten after 5 idle minutes, when the brain
    changes, when the mode changes, or when pinnad restarts. pinnad keeps no memory of its own; anything longer-lived is the
    agent's, on its side.
  - A model gets the earlier turns: at most 10.
  - An agent's conversation has no turn limit. Hermes gets the earlier turns, up to 64 KB of text,
    dropping the oldest first. OpenClaw gets only the new turn, with one stable `user` per device, so
    the gateway keeps the session.
- **The owner can type a question** on the dashboard (at most 1,000 characters). It runs as a spoken
  request would, without a follow-up, and the reply plays on the device. Like a spoken one, it is
  refused while the microphones are off: muted is the owner saying "not now".
- **pinnad's own tools are the paths behind its Home Assistant entities** (ADR 0007), so Home Assistant,
  the dashboard and the brain change the device the same way:
  - the controls, not the configuration switches: the ring light, volume (set or change) and reply
    volume, the media player (play a URL, pause, resume, stop), and clearing the notification. Cancel
    request is an MCP tool only: the brain is the request;
  - the microphone can be muted but not turned back on. On a device with the hardware switch, the mute
    closes the switch (`privacy_trigger`), which software can't open again, so one press of the
    **Microphone off** button turns the microphones back on;
  - Bluetooth pairing is left out;
  - the mode is the one configuration switch among them (`switch_assistant`), so the owner can ask a
    standalone satellite to switch back to Home Assistant: the brain or an MCP client calls it. `/mcp`
    answers in standalone mode only, so the way from Home Assistant to standalone stays the select,
    which Home Assistant can set from a script or automation. The tool follows the select's rule:
    *Standalone* is refused until the three slots are ready. The switch waits until the satellite is
    idle, so the reply that confirms it still plays, and that reply gets no follow-up. Anyone on the
    network can call it over MCP, as every MCP tool, until MCP has authentication.

  Added to them, because Home Assistant does them itself in its mode:
  - set, list and cancel timers;
  - the date and time;
  - the weather, from Open-Meteo (no key), for a place the person names (found by Open-Meteo's place
    search, first match; a country after a comma tells places apart), else for the place set on the
    dashboard;
  - radio: the owner's own station list (play one by name, list them) and a search of the
    radio-browser.info directory. A playlist address (`.m3u`, `.pls`) plays its first entry.

  Playing a URL works as on the dashboard (ADR 0011): the tool returns once the stream has audio or has
  failed, with the reason, which the brain can say. Each tool has an owner's switch; a tool that is off
  isn't offered and is refused if asked for.
- **Local time comes from the time zone setting** (ADR 0007), for the time tool and the brain. Unset is
  UTC. The place search returns the place's time zone, and the dashboard offers it when the device's
  differs.
- **A language setting** (a BCP 47 tag such as `de` or `en-US`) goes to the brain as the language to
  answer in, and its primary subtag (`de`) to speech-to-text as a hint and to ElevenLabs' text-to-speech.
- **Each stage has a time limit:**
  - speech-to-text: 15 s;
  - text-to-speech: 10 s to the first audio;
  - the brain's first text: 30 s, or 120 s for an agent, which may run tools first. The same limit
    applies between later pieces of the answer, so a stream that stalls fails.

  The state machine arms a later backstop for each stage, so a run that stops reporting still ends.
- **A slow agent gets an acknowledgment.**
  - If an agent's first text hasn't arrived after 3 s, the device says a short phrase, "One moment." by
    default, in the owner's language.
  - The phrase comes from a small table with one phrase per language, falling back to English. The owner
    can change it.
  - It is synthesised once, whenever the voice, its text-to-speech model, the language or the phrase
    changes, and kept on disk. It
    plays without waiting for a service.
- **The owner's MCP servers are remote only**, for the tool loop. pinnad uses Streamable HTTP (protocol
  2025-06-18, or 2025-03-26 for an older server). stdio servers don't run on the device.
  - They have no authentication yet: a server that takes a token in its address works; one that needs a
    header token or OAuth can't be added until a later ADR adds them.
  - A server's tools are named `<server>_<tool>`, with the server's hyphens as underscores; a name
    that doesn't fit providers' 64-byte `[A-Za-z0-9_-]` rule is left out. Its tool list is cached for 5 minutes, a failure for
    30 s. A server that doesn't list within 2 s is left out of that request.
  - Saving a server asks it for its tools at once, and the dashboard shows how it answered.
  - At most 8 servers.
- **pinnad detects the end of speech** with the energy VAD that the wake word detector already runs, on
  the frames after the AGC (ADR 0004).
  - Speech starts after 300 ms of voice and ends after 800 ms of silence.
  - Nothing heard within 5 s ends the request (4 s for a follow-up), and nothing is uploaded.
  - A request records at most 15 s.
- **The reply streams.**
  - The model's text is cut into sentences.
  - Each sentence is synthesised and plays through the voice lane's streaming decoder as it arrives.
  - At most two sentences are synthesised ahead of playback.
- **A standalone request is its own task.**
  - It reports typed events to the same state machine: speech ended, no speech, first audio, done, failed.
  - The Home Assistant path doesn't change.
- **The wake word works as it does in Home Assistant mode.**
  - It interrupts a request while it thinks or speaks, and starts a new one. The interrupted run is
    cancelled, and its late events are ignored.
  - It stops a ringing timer alarm and starts no request.
- **Timers run on the device in standalone mode**, with the same ring arc and alarm as Home Assistant's.
  - At most 10 at once, each at most 24 hours, with a name of at most 40 characters.
  - They survive a restart. Each is saved with its deadline; at boot it resumes from its saved remaining
    time at once, and if the clock syncs within 60 s, the deadline corrects it: a timer that came due
    rings, one overdue by more than 5 minutes is dropped. A timer saved before the clock had synced
    keeps its remaining time, as the time it was off can't be known.
- **Speak** says a text with the text-to-speech slot alone (at most 500 characters): the dashboard's
  Play sample and the `speak` tool. It works in either mode, once a voice is set up, and is refused
  while a request runs, an alarm rings or the device already says something. Music ducks under it.
- **Libraries and crates:**
  - Pinna moved to reqwest 0.13 with rustls on ring (`rustls-no-provider`), one HTTP stack for the
    updater, the media lane and the providers (`pinna-http`). It trusts the image's CA store
    (`ca-certificates`) instead of built-in roots.
  - The provider adapters, the tool loop and the sentence splitter are Pinna's own (`pinna-assist`), as
    are the MCP protocol, server and client (`pinna-mcp`): not genai or rmcp, so the device keeps one
    reqwest on ring. Every adapter is built from one endpoint (client, address, key header, preset), the
    same way for a request and for a save-time check.
  - The energy VAD and the end-of-speech detector are `pinna-vad`.

### Pinna as an MCP server

- **`/mcp` on the dashboard's listener** (amends ADR 0005) offers the device's tools to agents. It answers
  in standalone mode only; in Home Assistant mode it refuses with a sentence that says so. Its tools:
  - pinnad's own tools, except the weather, so an agent can set a timer or start a stream on the device;
  - speak a text;
  - cancel the request the device is handling, or silence an alarm;
  - status.

  It speaks MCP 2025-06-18 and 2025-03-26 over JSON, refuses batches and bodies over 64 KiB, runs at
  most two tool calls at once (another is told the device is busy), and keeps room in the state
  machine's queue for the wake word. The dashboard lists the last 8 clients that introduced themselves
  (`initialize` with a name), by that name and their address.
- **Access works like the dashboard: anyone on the LAN, no token.**
  - The Host allowlist applies. An agent reaches `/mcp` by the `.local` name, an IP address or the
    dashboard address (ADR 0021), and the Assistant page shows that address to copy.
  - Requests must be `application/json` and pass an Origin check.
- **No tool opens the microphone.** An agent can't make the device listen without a wake word.

### Secrets

- **Keys come in through the dashboard.** Unlike the ESPHome key (ADR 0018), no Home Assistant is there to
  set them.
- **Keys go in and never come out.** The API reports only that a key is set, and its last four characters.
- **A key belongs to its URL.**
  - A key is stored with the URL it was entered for.
  - Changing the URL clears the key, so nobody on the LAN can point a connection at their own server and
    receive the key.
  - Provider and MCP clients follow no redirects, so a key never goes to another host.
- **Secrets live in `/var/lib/pinna/secrets.toml`**, mode 0600, written durably like the ESPHome key
  (ADR 0018).
  - A factory reset deletes them with the rest of `/var/lib/pinna` (ADR 0010).
  - An edit writes `secrets.toml` before `standalone.toml`. If the second write fails, the keys are
    put back, so memory and disk agree.
- **MCP server URLs count as secrets.** Some services put the token in the path. The dashboard shows a
  server's name and host only. Changing a URL means entering it again in full.
- **Nothing secret is logged at info or above:** no key, no URL with credentials, no response body and no
  transcript. The one exception is what a service said when it answered with an error status: the
  message field of its error body, or a short plain-text body, on one line, at most 200 characters, with
  the connection's key and anything that looks like a key masked. It names what the status alone can't,
  such as a model that takes tools only with `reasoning_effort` `none`.

### Errors

- **A slot is checked when it is saved, and again when its connection changes.**
  - pinnad makes a cheap call, given 10 s: the model list, half a second of silence for a speech server
    without one (and always for ElevenLabs, whose list has no Scribe models), or a one-word phrase in
    the slot's voice and text-to-speech model. Only the brain's model is compared with the list; an
    agent's and a speech model aren't.
  - A refused check (400 or 422) adds what the service said, masked and capped like the log: it names
    the model the service didn't take, and often the ones it has.
  - A slot is saved only when its check passes, in either mode.
  - A connection change re-checks the slots on it. In standalone mode a change that fails is refused
    with a sentence naming each slot that failed. In Home Assistant mode it is saved, and those slots
    aren't ready until they pass again.
  - No brain is sent a prompt for the check.
- **The model and voice fields suggest what the service offers**, as it lists it when the field opens:
  the brain's models, an OpenAI-compatible server's speech models, and ElevenLabs' voices (by name, for
  their IDs) and text-to-speech models. Pinna keeps no list of its own, and any name can still be typed;
  the check stays the proof.
- **Text-to-speech has an optional model** besides the voice. Empty is the preset's: `tts-1` for
  OpenAI-compatible servers, `eleven_multilingual_v2` for ElevenLabs.
- **Each request leaves its trace in the log**, without what was said:
  - one line per tool call: the request or MCP client, the tool, ok or error, and how long it took. The
    arguments and the output stay out; they can hold what was said;
  - one line when the request is over: the speech-to-text time and seconds of audio, the time to the
    brain's first words, how long text-to-speech took for the first sentence, tokens and characters,
    and how it ended (answered, nothing heard, an invented transcript, failed, or stopped before it
    ended).
- **A request that fails gives one error cue.**
  - Its log line names the slot, the provider and the HTTP status, and what the service said. The
    dashboard's log shows it. A failed check is logged the same way.
  - The dashboard shows the last failure as a sentence with its slot, service and time, until a request
    is answered again.
  - pinnad doesn't retry.
- **Guards against spending:**
  - nothing is uploaded without speech;
  - the phrases Whisper invents on silence are dropped;
  - replies are capped with `max_tokens` (500, or 1,000 for an agent) and 1,500 spoken characters. An
    agent's answer is no longer read once the cap is reached;
  - a follow-up window that hears nothing closes, and uploads nothing;
  - the Action button or the wake word stops a request.

### Metrics (amends ADR 0020)

- **Standalone runs count where Home Assistant's do:** `pinna_assist_runs` and
  `pinna_reply_first_audio_seconds`. Their failures count in `pinna_standalone_errors`, by slot and a
  bounded reason: `http_401`, `http_402`, `http_403`, `http_404`, `http_429`, `http_3xx`, `http_4xx`,
  `http_5xx`, `http_other`, `timeout`, `unreachable`, `tls`, `clock`, `protocol`, `too_large` or
  `internal`.
  `pinna_assist_errors` stays Home Assistant's, by its error code.
- **Each stage has a histogram,** by slot: speech-to-text, the brain's first text, and each sentence's
  text-to-speech to its first audio.
- **What costs money is counted,** by preset, one family for each slot: seconds of audio uploaded,
  the tokens the provider reports (input and output), characters synthesised.
- **The guards are counted:** requests and follow-up windows that uploaded nothing, transcripts dropped
  as invented, follow-ups,
  acknowledgments played, requests stopped by the wake word or the Action button.
- **Tool calls are counted,** by tool, outcome and whether the brain or an MCP client called. pinnad's
  own tools are named; a tool from the owner's MCP servers counts under the server that answered it.
- **The mode and each slot's readiness are gauges.** Microphone frames a request didn't take in time
  are counted.
- **Labels never carry a URL, a key, a transcript or a name an MCP client sends.** Presets, slots, tool
  names and the owner's MCP server names are bounded; anything else would let the LAN grow the series.

## Consequences

- An Echo works without Home Assistant, as a voice assistant on its own: timers, the weather, radio and
  questions to a model. An agent such as Hermes or OpenClaw can use it as its voice and speaker.
- Owners without Home Assistant can set up a release image over USB, with no file and no SSH (ADR 0007,
  ADR 0008). The guided setup in ADR 0009, not built yet, ends by saying how Home Assistant finds the
  satellite. It needs a step that offers standalone mode instead.
- A device backup (ADR 0019) copies `userdata`, keys included. It is as sensitive as the keys. The
  settings backup ADR 0019 leaves to the settings needs to decide whether keys go into it.
- A satellite that never meets Home Assistant keeps a plaintext ESPHome API, and anyone on the LAN can set
  its first key (ADR 0018). It carries no microphone audio in standalone mode, and it controls nothing the
  dashboard doesn't already control.
- After the wake word, audio leaves the device for services the owner picked and pays for. goals.md now
  says "Your voice goes only where you send it" instead of "Keep speech local".
- A false wake can cost money. A TV that says the wake word counts as speech.
- Keys are safe from the LAN, but voice is not. Anyone who can use the dashboard can point a slot without a
  key, such as a local speech-to-text server, at their own server and receive the audio. This is the same
  LAN trust as ADR 0005, and goals.md says so.
- Home Assistant's timers don't carry over into standalone mode, and timers set on the device don't show
  in Home Assistant.
- Home Assistant follows the features (checked 2026-11-18 with Home Assistant 2026.11.1 against a local
  pinnad): switching to standalone removes the Assist satellite entity and both wake word selectors after
  the reconnect, and the other entities stay; switching back adds them again.
- Typing a question or playing a sample needs no microphone, so a setup can be tried from a laptop.
- After a rollback, an older image reports the voice assistant features again, although the setting still
  says standalone.
- The energy VAD may cut off a slow talker, or wait too long in a noisy room.
  - During Home Assistant requests, pinnad logs its own end of speech next to Home Assistant's.
  - If the two disagree, a model VAD (Silero) replaces the energy VAD.
- pinnad trusts the CAs in the image. A self-hosted server with a private CA needs plain http on the LAN
  until owners can add a CA.
- Moving to reqwest 0.13 touches the update and audio code too.
- A provider with its own API needs an adapter in pinnad. Deepgram and Cartesia are left out until someone
  needs them.
- Left out for now:
  - an MCP tool that asks a question out loud and returns the answer. It would open the microphone
    without a wake word and hand anyone on the LAN what was said;
  - ChatGPT sign-in, which needs a client ID;
  - speech-to-speech models;
  - MCP servers that need a header token or OAuth;
  - several satellites sharing one conversation.
- Spotify Connect is not planned (2026-11-18). librespot logs in, but Spotify refuses the audio keys
  of accounts created after a cutoff in 2024/2025, in every librespot version (librespot issue #1649,
  locked without a fix on 2026-11-07); tested with zeroconf and librespot 0.8.0. Spotify's own headless
  client, Soloist, may not be redistributed, its builds expire after 90 days, and each owner would need
  a developer API key. Spotify plays to the satellite over AirPlay or Bluetooth (ADR 0011).
