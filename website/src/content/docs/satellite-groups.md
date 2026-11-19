---
title: Satellite groups
description: Put satellites in standalone mode in a group, so that they pass messages to each other and act on each other.
---

Satellites in standalone mode that are in one group pass messages to each
other, such as "tell the kitchen that dinner is ready", and act on each other,
such as "turn the kitchen down".

## Set up a satellite group

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
   [Name the device](/docs/settings/#name-the-device). The brain calls each
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
[name it](/docs/settings/#name-the-device) first. **Open its dashboard**
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

## Pass messages between satellites

With more than one satellite in standalone mode, you can ask one to pass a
message to another, such as "tell the kitchen that dinner is ready". The
satellite in the kitchen says "Message from Hall: dinner is ready." in its
own voice. The opening is in the receiving satellite's **Language**, under
**Conversation**. With no **Language** set, or one that Pinna has no wording
for, it is in English.

If two satellites hear the wake word, both answer. Choosing one is planned.

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
satellites must be in one group; see
[Set up a satellite group](#set-up-a-satellite-group).

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

## What's next

- [Choose what the brain may do](/docs/standalone/#choose-what-the-brain-may-do):
  turn off `send_message` and the other tools that you don't want used.
