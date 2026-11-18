---
title: Use Pinna without Home Assistant
description: Set up standalone mode, where the satellite answers requests itself with the speech services, language model or AI agent you choose.
---

In standalone mode the satellite answers voice requests itself. After the wake
word it sends what you say to services that you set up on the dashboard:
speech-to-text, a brain (a language model or an AI agent such as Hermes or
OpenClaw) and text-to-speech. It keeps timers itself, tells the weather, plays
radio, and lets AI agents use it over MCP.

Home Assistant isn't needed. If you have it, it can stay connected: it keeps
the media player, announcements, volume, mute, the ring and the buttons, but it
no longer hears requests.

## Before you begin

- Pinna runs on the device, and you can open its
  [dashboard](/docs/dashboard/).
- You have an account with a key for each hosted service you want to use, or
  your own servers on your network. A brain can be any of these:

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

  **Other server** is any server that speaks OpenAI's API, such as a
  self-hosted speech server. One OpenAI key can serve all three.
- Hosted services charge for what you use. The satellite uploads audio only
  after the wake word, and only when it heard speech. A TV or a conversation
  that sounds like the wake word still costs a request.

After the wake word, the audio goes to the services you chose, and nowhere
else. Anyone who can use the dashboard can change those services; see
[Who can use the dashboard](/docs/dashboard/#who-can-use-the-dashboard).

## Add the services

1. Open the dashboard, select **Assistant**, and then select **Services**.
2. Under **Connections**, select **Add…** and choose the service.
   - For a hosted service, enter its **API key**.
   - For Ollama, LM Studio or Hermes, enter the computer it runs on, such as
     `192.168.1.20`. Pinna adds the port and the path.
   - For OpenClaw, enter the computer and the port its gateway listens on,
     such as `192.168.1.20:8080`.
   - For **Other server**, enter its whole address, up to and including `/v1`.
3. Select **Add**.
4. Under **Speech-to-text**, **Brain** and **Text-to-speech**, choose a
   connection, enter the model or the voice as the service names it, and
   select **Save and check**.

   The check makes the cheapest call that proves the service works: it lists
   the models, or synthesises one word. It never sends a prompt. If the check
   fails, the page says why, for example that the key is wrong or that the
   model isn't offered.
5. Under **Text-to-speech**, select **Play sample** to hear the voice on the
   satellite.

A key goes to the satellite and never comes back: the page shows only that it
is set, with its last four characters. A key belongs to the address it was
entered for. If you change a connection's address, enter the key again.

## Switch to standalone mode

1. Select **Assistant**.
2. Under **Mode**, select **Standalone**. The button works once all three
   services have passed their check.
3. Select **Switch**.

The satellite ends any request it is working on and cancels the timers that
run. Home Assistant reconnects and no longer shows the satellite's Assist
satellite. In standalone mode the dashboard chooses the wake words, under
**Voice & sound**.

To go back, select **Home Assistant** under **Mode**. Home Assistant answers
requests again as soon as it is connected.

## Set up the conversation

Select **Assistant**, then **Conversation**.

- **Language**: a tag such as `en`, `de` or `en-US`. Speech-to-text listens for
  it, and the brain answers in it.
- **Place**: a town or city, for the weather and so the brain knows where it
  is. Select **Find**. If the place is in another time zone than the
  satellite, the page offers to use the place's.
- **What the brain is told**: the prompt, at most 4,000 characters. It starts
  as the satellite's voice rules: short, plain spoken answers. **Reset to the
  voice rules** brings them back. With each request the satellite adds the
  local time, the place, the language and its name.
- **Follow-ups**: after each reply, the satellite listens for 4 seconds more,
  without the wake word, so you can answer. Silence ends the conversation.
- **Acknowledgment**: a short phrase, "One moment." in your language, that the
  satellite says when an agent hasn't answered after 3 seconds.
- **Ask a question**: type a question and select **Ask**. The brain gets it as
  if you had said it, and the satellite answers aloud. Use it to try the setup
  from your computer.

The conversation stays in the satellite's memory only. It is forgotten after 5
minutes without a request, when you change the brain, or when the satellite
restarts.

## Choose what the brain may do

Select **Assistant**, then **Tools**.

- **Pinna's tools** lists what the brain can do on the satellite: timers, the
  time, the weather, the volume and the reply volume, the ring light, playing,
  pausing and stopping music and radio, clearing a notification, and muting the
  microphones. Only a person can turn the microphones back on. Turn off a tool
  you don't want the brain to use.
- **Radio stations**: add a station with its name and stream address. Then ask
  for it by name, such as "play Radio Eins". The brain can also search the
  radio-browser.info directory for stations.
- **Your MCP servers**: add an MCP server on your network with a name and its
  address. The brain then calls its tools next to Pinna's. Saving a server asks
  it for its tools, and the list shows how many it has, or why it can't be
  reached. A server that needs a sign-in or a token in a header can't be added
  yet.

The **Brain** has **Advanced** settings under **Services**, for a server of
your own: who runs the tools, how much of the conversation the brain gets, and
how long it may take to start answering.

## Use an AI agent as the brain

Hermes and OpenClaw keep their own memory and tools. When one of them is the
brain, the satellite hands each request to it and speaks its answer.

- Hermes listens on port 8642. Pinna sends it the conversation so far.
- OpenClaw's chat endpoint is off until you turn it on in OpenClaw. Pinna sends
  it only the new request; OpenClaw keeps the conversation.
- An agent may take a while; the acknowledgment tells you it heard you.

An agent that wants to use the satellite reaches it as an MCP server:

1. Select **Assistant**, then **Tools**.
2. Under **Pinna as an MCP server**, select **Copy** next to the address.
3. Enter the address in the agent's MCP settings.

The agent can then set timers, play a stream, speak a text, change the volume
and the light, and read the satellite's status. It can't make the satellite
listen. Like the dashboard, the MCP server has no password: anyone on your
network can use it. It answers in standalone mode only. The list under it shows
the agents that called.

## Use timers

Ask for a timer as you would ask anyone, such as "set a tea timer for four
minutes". The ring shows an amber arc while it runs, and the alarm rings when it
ends. Say the wake word or press the Action button to stop it.

Up to 10 timers run at once, each for up to 24 hours. They survive a restart of
the satellite.

## When a request fails

The satellite plays the error sound, and **Assistant** shows the last failure
with what failed, the service and the time, until a request is answered again.
The log under **System** has a line for each failure. See
[Troubleshooting](/docs/troubleshooting/) for what the messages mean.

## What's next

- Watch the costs and the delays with the [Prometheus metrics](/docs/metrics/).
- Set up the satellite from scripts with the
  [control API](/docs/control-api/).
