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
satellite's **Voice mode** select. It stays in both modes, and **Standalone** works only once the three services have passed
their check. If the satellite is answering a request when the select changes,
it switches once the request has ended, after the reply and any follow-up.

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
something that sets the **Voice mode** select, because Assist has no command
that changes a select:

- **With an AI conversation agent that may control Home Assistant**: in Home
  Assistant, create a script under **Settings > Automations & scenes >
  Scripts**, and expose it under **Settings > Voice assistants > Expose**. In
  its **Description**, say when to use it. Replace the entity ID with your
  satellite's: copy it from the **Voice mode** select on the device page.

  ```yaml
  alias: Switch the Echo Dot to standalone mode
  description: >-
    Switches the Echo Dot to Pinna's standalone mode. Use it when the person
    asks to switch to standalone mode.
  sequence:
    - action: select.select_option
      target:
        entity_id: select.echo_dot_voice_mode
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
  local time, the place, the language and its name, and in a satellite group
  the other satellites' names, with those in Home Assistant mode named as out
  of reach.
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
  microphones, switching back to Home Assistant mode (`switch_assistant`),
  saying which version runs, checking for updates, and
  [passing messages](#pass-messages-between-satellites) to other satellites
  (`send_message`). In a satellite group, the brain can also
  [control another satellite](#control-another-satellite). Only a person can
  turn the microphones back on, with the **Microphone off** button. Turn off a
  tool you don't want used. **Install update** is the one tool that starts off:
  turn it on to let the brain install an update, as
  [Ask the satellite to update](/docs/update/#ask-the-satellite-to-update)
  describes.
- **Radio stations**: add a station with its name and stream address. Then ask
  for it by name, such as "play Radio Eins". The brain can also search the
  radio directory for stations.
- **Your MCP servers**: add an MCP server on your network with a name and its
  address. The brain then calls its tools next to Pinna's. Saving a server asks
  it for its tools, and the list shows how many it has, or why it can't be
  reached. **Its tools** under a server lists them, each with a switch: turn
  off the ones the brain shouldn't call. A server that needs a sign-in or a
  token in a header can't be added yet.

## Pass messages between satellites

With more than one satellite in standalone mode, you can ask one to pass a
message to another, such as "tell the kitchen that dinner is ready". The
satellite in the kitchen says "Message from Hall: dinner is ready." in its
own voice. The opening is in the receiving satellite's **Language**, under
**Conversation**. With no **Language** set, or one that Pinna has no wording
for, it is in English.

If two satellites hear the wake word, both answer. Choosing one is planned.

The satellites must be in one satellite group.

### Set up a satellite group

Before you begin:

- The satellites are on the same subnet. The search doesn't find a satellite
  on another subnet or VLAN, even if your router passes mDNS between them.
- The satellite you talk to is in standalone mode, with a language model as
  its brain. An agent such as Hermes or OpenClaw gets no device tools, so it
  can't send messages or act on another satellite.
- Each satellite that should take messages is in standalone mode, with
  text-to-speech that has passed its check.

1. Name each satellite something easy to say, such as `Kitchen`, on
   **System** on its dashboard; see
   [Name the device](/docs/dashboard/#name-the-device). The brain calls each
   member by its name, so two members can't share a name, and none can be
   called "all".
2. On the dashboard of each satellite, select **Assistant**. Under
   **Satellite group**, turn on **Satellite group**. A satellite takes part
   only once its own switch is on.
3. Wait about a minute. The other satellites see the switch after their next
   search, which runs once a minute. Until then, they show this one as
   **Satellite group is off there, or its Pinna is older**.
4. On one of them, under **Other satellites**, select **Add** next to another
   satellite. This starts a group of the two. Adding takes up to 15 seconds,
   and **Members** then lists both.
5. To add another satellite, select **Add** next to it on the dashboard of any
   member.

**Other satellites** lists the satellites outside the group that the search
found, and what each one offers:

- **Add**: its **Satellite group** switch is on and it is in no group.
- **In another group**: it belongs to another group. To move it to this one,
  turn its switch off and on again on its dashboard, which takes it out of
  that group.
- **Satellite group is off there, or its Pinna is older**: on its dashboard,
  turn on **Satellite group**, or [update it](/docs/update/). If you just
  turned it on, wait about a minute.

The list says when a name is refused: rename that satellite first, on
**System** on its dashboard. A satellite that still has the name Pinna gave
it, such as `Echo Dot 2a914e`, can be added, but the name is hard to say:
[name it](/docs/dashboard/#name-the-device) first. **Open its dashboard**
opens that satellite's dashboard in a new tab.

If you rename a member, the other members use the new name after their next
search, within about a minute. Names are checked only when a satellite is
added: if a rename gives two members the same name, the brain can't use that
name until you rename one of them.

**Members** lists the satellites of the group by name, with this satellite
marked, and for each other one its mode and when it was last seen. To take a
member out, select **Remove** and confirm. Every member is told, the one
removed too, and a group left with one member ends. The satellite removed can
be added again after the next search, within about a minute.

To take a satellite out from its own dashboard, turn off its **Satellite
group** switch: it tells the other members, and drops the messages it holds.
To join again, turn the switch back on, wait about a minute, then select
**Add** next to this satellite on a member's dashboard. If the group had two
members, it ended when this one left: **Add** on the other satellite's
dashboard starts a new group of the two.

A member that was reset, or that left without the other members hearing it,
stays under **Members** as **Left the group**, or as **In another group** once
it has joined another one. You can remove it. **Add again** shows next to it
while its **Satellite group** switch is on and it is in no group. A member in
another group can't be added again until its switch has been turned off and
on again on its dashboard.

### How messages play

- The brain gets the `send_message` tool once another satellite of the group
  is in standalone mode. It sends a message to one satellite by its name, or
  to all of them, and tells you what each one did: it played the message, it
  holds it, it refused it and why, or it couldn't be reached.
- A satellite says a message at once when it is idle or plays only music,
  which gets quieter under it. While it answers a request or listens for a
  follow-up, plays something else, rings an alarm or runs a calibration, or
  while its microphones are off, it holds the message. Held messages play by
  themselves, oldest first, once it is idle and its microphones are on.
- A satellite holds at most 5 messages, each for 12 hours at most. It forgets
  them when it restarts, when it switches to Home Assistant mode, and when its
  **Satellite group** switch is turned off. A satellite removed from its
  group, or whose group ended, keeps them and plays them. The dashboard's
  **Overview** shows how many it holds, such as "2 messages held". While the
  microphones are off, it adds that they play once the microphones are on.
- The wake word stops a message, which plays again later. The Action button
  stops it for good.
- A satellite in Home Assistant mode stays in its group, but takes no
  messages: the brain says it is out of reach.
- To stop a satellite from taking messages, select **Assistant** on its
  dashboard, and under **From the group**, turn off **Group messages**.
  **From the group** shows while **Satellite group** is on. Home Assistant
  has a switch of the same name. Messages the satellite already holds still
  play.

  Pinna has no schedule for this. To turn messages off at night, create an
  automation in Home Assistant under **Settings > Automations & scenes >
  Automations**, and enter it with **Edit in YAML**. Replace the entity ID
  with your satellite's **Group messages** switch, which keeps the name the
  satellite had when you added it to Home Assistant.

  ```yaml
  alias: No group messages in the bedroom at night
  triggers:
    - trigger: time
      at: "22:00:00"
  actions:
    - action: switch.turn_off
      target:
        entity_id: switch.bedroom_group_messages
  ```

  A second automation with `switch.turn_on` turns them back on in the
  morning.

## Control another satellite

In a satellite group, you can ask one satellite to act on another by its name,
such as "turn the kitchen down" or "set a 10 minute timer in the office". The
group is set up as for
[passing messages](#pass-messages-between-satellites).

- Once another satellite of the group is in standalone mode, the brain can
  name it for these tools: the volume and the reply volume, the ring light,
  playing a stream, pausing, resuming and stopping music and radio, playing
  and listing radio stations, and setting, listing and cancelling timers. It
  can also stop another satellite's request, its ringing alarm, or a message
  or `speak` that it is saying (`cancel_request`). A message stopped this way
  doesn't play again. Without a name, a tool acts on the satellite you are
  talking to.
- A timer set on another satellite rings there, and that satellite lists and
  cancels it.
- Radio stations are the other satellite's own: it plays a station from its
  own list.
- The other satellite runs the tool as if an MCP client had called it: its
  own tool switches apply, and it is busy while it runs two tool calls
  already. A tool switched off on the satellite you are talking to isn't
  offered for any satellite.
- The brain says when another satellite can't do it: it is in Home Assistant
  mode, the search doesn't find it, it doesn't answer within 9.5 seconds (what
  it was asked may still happen), or it refused.
- To stop the others from acting on a satellite, select **Assistant** on its
  dashboard, and under **From the group**, turn off **Control from the
  group**. **From the group** shows while **Satellite group** is on.
- Your MCP clients can't act on another satellite: Pinna's MCP server offers
  this satellite's tools only.

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
Assistant, read the satellite's status and which version it runs, and check
for updates. It can't install an update or make the satellite listen, and it
doesn't get the weather. Like the dashboard, the MCP server has no password:
anyone on your network can use it. It answers in standalone mode only. The list under it shows the agents that called.

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
