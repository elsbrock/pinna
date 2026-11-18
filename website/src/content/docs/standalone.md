---
title: Use Pinna without Home Assistant
description: Set up standalone mode, where the satellite answers requests itself with the speech services, language model or AI agent you choose.
---

In standalone mode the satellite answers voice requests itself. After the wake
word it sends what you say to services that you set up on the dashboard:
speech-to-text, a brain (a language model or an AI agent such as Hermes or
OpenClaw) and text-to-speech. It keeps timers itself, tells the weather and
plays radio. AI agents can also use it over MCP, the protocol they use to call
tools.

> **Standalone mode is new.** Its tests run against simulated services, and it
> has been tried on an Echo Dot (2nd gen) with OpenRouter and ElevenLabs, not
> yet with every service; see [Status and roadmap](/docs/status/).

## Before you begin

- Pinna runs on the device, and you can open its
  [dashboard](/docs/dashboard/). If **Assistant** has no **Services**, the
  device runs a Pinna from before standalone mode:
  [update it](/docs/update/) first.
- You have an account with a key for each hosted service you want to use, or
  your own servers on your network. Each service offers some of the three
  parts:

  | Service | Speech-to-text | Brain | Text-to-speech |
  | --- | --- | --- | --- |
  | OpenAI | Yes | Yes | Yes |
  | OpenRouter | | Yes | |
  | Anthropic | | Yes | |
  | Google Gemini | | Yes | |
  | Groq | Yes | Yes | |
  | Mistral | | Yes | |
  | ElevenLabs | Yes | | Yes |
  | Ollama, LM Studio | | Yes | |
  | Hermes, OpenClaw (agents) | | Yes | |
  | Other server | Yes | Yes | Yes |

  One OpenAI key can serve all three. **Other server** is any server that
  speaks OpenAI's API, such as a self-hosted speech server.
- Hosted services charge for what you use. The satellite uploads audio only
  after the wake word, and only when it heard speech. A TV or a conversation
  that sounds like the wake word still costs a request.

After the wake word, the audio goes to the services you chose, and nowhere
else. Two more services are fixed: Pinna looks up places and the weather at
[Open-Meteo](https://open-meteo.com/), which gets the name and coordinates of
your place and of any place you ask about, and searches for radio stations at
[radio-browser.info](https://www.radio-browser.info/), which gets what you
searched for. Leave **Place** empty and turn off the **Weather** and **Search
radio** tools to use neither. Neither needs an account.

Anyone who can use the dashboard can change your services and use them; see
[Who can use the dashboard](/docs/dashboard/#who-can-use-the-dashboard).

## Add the services

1. Open the dashboard, select **Assistant**, and then select **Services**.
2. Under **Connections**, select **Add…** and choose the service.
   - For a hosted service, enter its **API key**. You make the key in your
     account with the service, for ElevenLabs under its API keys. One
     ElevenLabs connection serves both speech-to-text and text-to-speech. A
     key with limited permissions needs speech-to-text and text-to-speech, and
     reading voices and models for the lists below.
   - For Ollama, LM Studio or Hermes, enter the computer it runs on, such as
     `192.168.1.20`. Pinna adds the port and the path.
   - For OpenClaw, enter the computer and the port its gateway listens on,
     such as `192.168.1.20:8080`.
   - For **Other server**, enter its whole address, up to and including `/v1`.
3. Select **Add**.
4. Under **Speech-to-text**, **Brain** and **Text-to-speech**, choose a
   connection, enter the model, and select **Save and check**. Text-to-speech
   asks for a voice, and optionally the model that speaks it; empty uses the
   service's usual one. Each field offers what the service lists when you open
   it with the arrow at its end or the arrow keys, and typing narrows the list.
   You can also type a name it doesn't list. For example:

   | Part | OpenAI | ElevenLabs | OpenRouter |
   | --- | --- | --- | --- |
   | Speech-to-text | A transcription model, such as `whisper-1` or `gpt-4o-mini-transcribe` | A Scribe model, such as `scribe_v2`. ElevenLabs doesn't list them | (none) |
   | Brain | A chat model, such as `gpt-4.1-mini` | (none) | A model with its provider, such as `deepseek/deepseek-v4.1-flash` |
   | Text-to-speech | A voice, such as `alloy` or `nova`; the model is `tts-1` unless you choose another | A voice from your own voice library: the list shows the names and enters the ID. The model is `eleven_multilingual_v2` unless you choose another, such as `eleven_flash_v2_5` | (none) |

   The check makes the cheapest call that proves the service works: it lists
   the models, transcribes half a second of silence, or synthesises one word.
   It never sends a prompt. If it fails, the page says why, for example that
   the key is wrong, or what the service said about a model it doesn't have.
   The brain's model must be in the service's list.
5. Under **Text-to-speech**, select **Play sample** to hear the voice on the
   satellite.

Some reasoning models refuse requests with tools until you choose how long
they think. OpenAI's `gpt-6-luna`, for example, uses tools only with thinking
off. Under **Brain**, select **Advanced**, under **Thinking** choose
**None**, and select **Save**. **None** also answers fastest. **Thinking** is there for OpenAI and
the other services that speak OpenAI's API; **Service default** sends nothing.

A key goes to the satellite and never comes back: the page shows only that it
is set, with its last four characters. A key belongs to the address it was
entered for. If you change a connection's address, enter the key again.

### Your own servers

- **Ollama** listens only on its own computer until you set
  `OLLAMA_HOST=0.0.0.0` for it. Enter the model as `ollama list` shows it;
  `llama3.2` works for `llama3.2:latest`. Choose a model that supports tool
  calls: the satellite offers the brain its tools with every request, and a
  model without tool support refuses every request.
- **LM Studio** has to serve on the local network, not only on its own
  computer. The same rule about tool calls applies.
- **Hermes** answers once its API server is on and reachable from the
  satellite. Enter its API key as the connection's **API key**, and as the
  model, a name its API server lists.
- **OpenClaw**'s chat endpoint is off until you turn it on in OpenClaw. It
  chooses its own model.

## Switch to standalone mode

Before you switch, set the language and the place under **Conversation**
([below](#set-up-the-conversation)): the weather needs a place unless you name
one when you ask.

1. Select **Assistant**.
2. Under **Mode**, select **Standalone**. The button works once all three
   services have passed their check.
3. Select **Switch**.

The satellite ends any request it is working on, stops a ringing alarm,
cancels the timers that run and forgets the conversation. In standalone mode
the dashboard chooses the wake words, under **Voice & sound**.

To go back, select **Home Assistant** under **Mode**, then **Switch**. If Home
Assistant isn't connected, nothing answers requests until it connects.

If Home Assistant is connected, you can also switch there, with the
satellite's **Assistant** select that has the choices **Home Assistant** and
**Standalone**. The device page in Home Assistant has a second select called
**Assistant**: that one chooses the Assist pipeline. The mode select stays in
both modes, and **Standalone** works only once the three services have passed
their check. If the satellite is answering a request when the select changes,
it switches once the reply has played.

### Switch by voice

To switch back to Home Assistant, say the wake word and "switch to Home
Assistant". The satellite switches once its reply has played, and doesn't
listen for a follow-up. As with **Switch** on the dashboard, the timers are
deleted, and if Home Assistant isn't connected, nothing answers requests until
it connects. This uses the `switch_assistant` tool under
[**Tools**](#choose-what-the-brain-may-do), so it works while that tool is on.
With Hermes or OpenClaw as the brain, it works only if the agent uses
[Pinna's MCP address](#use-an-ai-agent-as-the-brain).

To switch to standalone mode while Home Assistant answers, Home Assistant needs
something that sets the mode select, because Assist has no command that
changes a select:

- **With an AI conversation agent that may control Home Assistant**: in Home
  Assistant, create a script under **Settings > Automations & scenes >
  Scripts**, and expose it under **Settings > Voice assistants > Expose**. In
  its **Description**, say when to use it. Replace the entity ID with your
  satellite's mode select: copy it from the device page, because Home
  Assistant's pipeline select may also end in `_assistant`.

  ```yaml
  alias: Switch the Echo Dot to standalone mode
  description: >-
    Switches the Echo Dot to Pinna's standalone mode. Use it when the person
    asks to switch to standalone mode.
  sequence:
    - action: select.select_option
      target:
        entity_id: select.echo_dot_assistant
      data:
        option: Standalone
  ```

- **With Home Assistant's built-in agent**, or an AI conversation agent set to
  prefer handling commands locally: create an automation with a sentence
  trigger such as "switch to standalone mode", and the same
  `select.select_option` action.

If the three services haven't passed their check, the satellite refuses and
the select goes back to **Home Assistant**, even if the agent says it
switched. Switching deletes the timers that Home Assistant set on the satellite
too.

### If you also use Home Assistant

Home Assistant can stay connected in standalone mode. It keeps the media
player, announcements, volume, mute, the ring and the buttons, but it no longer
hears requests: it removes the satellite's voice assistant and wake word
choices until you switch back. When you switch back, Home Assistant answers
requests again as soon as it is connected.

## Set up the conversation

Select **Assistant**, then **Conversation**.

- **Language**: a tag such as `en`, `de` or `en-US`. Speech-to-text listens for
  the language, and the brain answers in it.
- **Place**: a town or city, for the weather and so the brain knows where it
  is. Select **Find**. You can still ask for the weather somewhere else, such
  as "the weather in Paris"; add the country when a name is ambiguous. If the place is in another time zone than the
  satellite, the page offers to use the place's.
- **What the brain is told**: the prompt, at most 4,000 characters. It starts
  as the satellite's voice rules: short, plain spoken answers. **Reset to the
  voice rules** brings them back. With each request the satellite adds the
  local time, the place, the language and its name.
- **Follow-ups**: on at first. After each reply, the satellite listens for 4
  seconds more, without the wake word, so you can answer. If you say nothing,
  it stops listening.
- **Acknowledgment**: on at first. A short phrase, "One moment." in your
  language, that the satellite says when an agent hasn't answered after 3
  seconds, so you know it heard you.
- **Ask a question**: type a question and select **Ask**. The brain gets it as
  if you had said it, and the satellite answers aloud. Use it to try the setup
  from your computer.

The conversation stays in the satellite's memory only. A request within 5
minutes of the last one continues it. It is forgotten after 5 minutes without
a request, when you change the brain or the mode, or when the satellite
restarts.

## Choose what the brain may do

Select **Assistant**, then **Tools**.

- **Pinna's tools** lists what a brain can do on the satellite: timers, the
  time, the weather, the volume and the reply volume, the ring light, playing,
  pausing and stopping music and radio, clearing a notification, muting the
  microphones, and switching back to Home Assistant mode (`switch_assistant`).
  Only a person can turn the microphones back on, with the **Microphone off**
  button. Turn off a tool you don't want used.
- **Radio stations**: add a station with its name and stream address. Then ask
  for it by name, such as "play Radio Eins". The brain can also search the
  radio directory for stations.
- **Your MCP servers**: add an MCP server on your network with a name and its
  address. The brain then calls its tools next to Pinna's. Saving a server asks
  it for its tools, and the list shows how many it has, or why it can't be
  reached. **Its tools** under a server lists them, each with a switch: turn
  off the ones the brain shouldn't call. A server that needs a sign-in or a
  token in a header can't be added yet.

## Use an AI agent as the brain

Hermes and OpenClaw keep their own memory and tools. When one of them is the
brain, the satellite hands each request to it and speaks its answer.

- The satellite offers an agent none of Pinna's tools and none of your MCP
  servers. To let it set timers or play radio on the satellite, give it
  Pinna's MCP address (below).
- Hermes gets the conversation so far. OpenClaw gets only the new request and
  keeps the conversation itself.
- An agent may take a while; the acknowledgment tells you it heard you. If it
  takes longer than 2 minutes to start answering, the request fails. To change
  that, select **Assistant**, then **Services**, and under **Brain** select
  **Advanced**: **Time to answer** sets the limit, and **Who runs the tools**
  and **Conversation it gets** change how the satellite treats the brain.

An agent reaches the satellite as an MCP server:

1. Select **Assistant**, then **Tools**.
2. Under **Pinna as an MCP server**, select **Copy** next to the address.
3. Enter the address in the agent's MCP settings.

The agent can then set timers, play a stream, speak a text, change the volume
and the light, mute the microphones, switch the satellite back to Home
Assistant, and read the satellite's status. It can't make the satellite
listen, and it doesn't get the weather. Like the dashboard, the MCP server has
no password: anyone on your network can use it. It answers in standalone mode
only. The list under it shows the agents that called.

## Use timers

Ask for a timer as you would ask anyone, such as "set a tea timer for four
minutes". The ring shows an amber arc while it runs, and the alarm rings when it
ends. Say the wake word or press the Action button to stop it. With an agent as
the brain, the agent sets the timer through Pinna's MCP server.

Up to 10 timers run at once, each for up to 24 hours. They survive a restart of
the satellite.

## When a request fails

The satellite plays the error sound and the ring flashes red. **Assistant**
shows the last failure, with what failed, the service and the time, until a
request is answered again. **System** › **Logs** has a line for each failure,
with what the service said, a line for each tool the brain called, and one
line for each request with how long each part took and how it ended; select
**All** there to see the last two. The log never contains what was said, and
it masks keys.
See [Standalone mode doesn't answer](/docs/troubleshooting/#standalone-mode-doesnt-answer)
for what the messages mean.

## What's next

- Learn what the [light ring and the buttons](/docs/ring-and-buttons/) show
  and do.
- Watch usage and delays with the [Prometheus metrics](/docs/metrics/).
- For developers: set up the satellite from scripts with the control API's
  [AssistantService](/docs/control-api/#assistantservice).
