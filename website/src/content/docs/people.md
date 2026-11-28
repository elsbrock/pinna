---
title: Recognize who speaks
description: Teach the satellite the voices in your household, so that Home Assistant shows who spoke and, in standalone mode, each person gets their own instructions.
---

The satellite can tell the people in your household apart by their voices. You
add each person on the dashboard, and each one speaks to the satellite once,
for 30 seconds. After that, the satellite compares each request with those
voices.

What recognition does depends on the mode:

- **In standalone mode,** a recognized person's requests get the extra
  instructions that you wrote for them, and the brain is told their name. For
  example, you can tell the brain how to address a child.
- **In both modes,** the **Last speaker** entity in Home Assistant shows who
  spoke. See [Home Assistant entities](/docs/entities/#sensors).

Recognition is off until someone enrolls. With nobody enrolled, the satellite
doesn't load the speaker model. In standalone mode, every spoken request gets
the extra instructions of **Everyone else**.

## What to know first

- **Recognition is a convenience, not a lock.** A recording or a similar voice
  can pass as someone else, and the satellite can mix up voices in one family.
  Don't use it to protect anything.
- **Put restrictions on Everyone else.** **Everyone else** applies whenever
  the satellite isn't sure who spoke: someone it doesn't know, speech that is
  too short, far away or over music, a request just after the satellite
  started, before the speaker model has loaded, or, in standalone mode, a
  request whose text arrives before recognition finishes. A person's extra
  instructions replace those of **Everyone else**; they don't add to them. So
  write what the brain may not do for **Everyone else**, and leave it out for
  the people who may. A restriction written only for one person doesn't apply
  when the satellite doesn't recognize them. Instructions ask the brain; they
  enforce nothing, and every person can use the same tools.
- **Voiceprints stay on the satellite.** Voiceprints never leave it: not to
  Home Assistant, not to your speech services, not to your browser. The
  recording of an enrollment stays in memory and is dropped once the
  voiceprint is made. Each person enrolls on each satellite. In standalone
  mode, your brain's service gets the name and the extra instructions as part
  of the prompt.
- **The log and the timeline name who spoke.** Check both before you share
  them.
- **Home Assistant keeps a history of who spoke.** It records every change of
  the **Last speaker** entity, with the time and the satellite.

## Before you begin

- The satellite is running and you can open its dashboard.
- To use extra instructions, set up [standalone mode](/docs/standalone/).
  Recognition and **Last speaker** work in Home Assistant mode too.
- Whoever enrolls can stand at the satellite, within 1 m, in a quiet room. A
  TV or radio is off, and nobody else talks.

## Add a person and enroll their voice

1. On the dashboard, open **Voice & sound > People**.
2. Next to **Add a person**, select **Add**. You can add up to 10 people.
3. Enter the **Name**, at most 40 characters. It must differ from the other
   names, ignoring case, and from Everyone else. Home Assistant shows this
   name as **Last speaker**.
4. Optional: in **Extra instructions**, write what the brain should know
   about this person. See [Write extra instructions](#write-extra-instructions).
5. Select **Add**. The page says that the person was added.
6. Select **Enroll voice**, then **Start**.
7. The ring circles in blue for up to 60 seconds. The person presses **Action**
   on the satellite while it does. This press confirms that someone is at the
   satellite, so enrolling needs no dashboard password. If you set a password,
   you sign in to the dashboard as for any other setting. If nobody presses
   **Action** in time, the page says "Nobody pressed Action in time." Select
   **Try again**. After a confirmation, pressed or not, the satellite waits 60
   seconds before it asks for the next, so the page may count down first.
8. The person speaks for 30 seconds. The page shows the level and how much
   speech the satellite has heard. The person either reads the text on the
   page or talks about anything. Children who can't read can just talk.
9. After the 30 seconds, the satellite takes a few more seconds to make the
   voiceprint. The page then says that it saved the voice. Select **Close**.

Stay on the page until it says that the voice is saved. Leaving it, **Cancel**,
muting the microphones, pressing **Action** or an announcement or a ringing
timer alarm ends the enrollment, and nothing is saved. The satellite refuses to
start one while the microphones are muted, wake word calibration, a recording
or another enrollment runs, or the satellite isn't quiet: a request, a reply,
music, an announcement or a timer alarm. Stop them before you select
**Start**.

### If the enrollment fails

The satellite checks the recording before it saves anything. A failed check
says why and what to do. Select **Try again** when you have fixed it.

| Message starts with | What it means | What to do |
| --- | --- | --- |
| `Too short` | Under 10 seconds of the 30 were speech | Speak for the whole time, without long pauses |
| `Too quiet` | The speech was too soft | Stand closer, within 1 m |
| `Clipped` | The speech was loud enough to distort | Speak more softly, or stand farther from the satellite |
| `Inconsistent` | Part of the recording doesn't sound like the rest | Record again with only one person speaking, and a TV or radio off |
| `The speaker model is busy` or `still loading` | The satellite is working on another voice, or has just started | Wait a moment, then try again |

A failed attempt keeps the person's earlier voice, if there is one.

### Enroll again

Select **Change** next to the person, then **Enroll again**. The old voiceprint
is replaced only when the new recording passes all the checks.

The page suggests enrolling again in these cases:

- **Enrolled with older settings.** An update changed how the satellite
  processes the microphones. The old voiceprint still works.
- **Enroll again: the speaker model changed.** The old voiceprint is ignored,
  and the person counts as not enrolled until they enroll again. While nobody
  has a usable voiceprint, the page says "Nobody has a voiceprint the
  satellite can use, so it doesn't recognize voices yet. Enroll each person."
  Select **Change** next to each person, then **Enroll again**.
- **May be mixed up with** another person. The two voices are close. Enroll
  both again, closer to the satellite. If the warning stays, expect the
  satellite to confuse them, and put the limits on **Everyone else**.

## Write extra instructions

Each person has **Extra instructions**, at most 1,000 characters. So does
**Everyone else**, which you find on the same page. They are for what the brain
needs to know about the speaker. Say how to address them and which pronouns to
use there: Pinna records no gender, age or other detail about a person.

Extra instructions apply in standalone mode only. In Home Assistant mode, the
page only records who spoke.

To change them, open the page, select **Change** next to the person, edit the
text and select **Save**. For **Everyone else**, edit the text under
**Everyone else** and select **Save**. **Everyone else** can't be enrolled,
removed or renamed.

In standalone mode, the prompt is, in order:

1. your prompt, or the voice rules;
2. the extra instructions of the person, or of **Everyone else**;
3. the context, such as the time and the place. For a recognized person, it
   includes "The speaker is" and their name. For **Everyone else**, it names
   nobody.

Typed questions, from **Ask a question** on the **Conversation** page, use your
settings and no person's extra instructions. They don't change **Last speaker**.

## How a conversation follows the speaker

In standalone mode, a follow-up, the reply you give within 4 seconds of an
answer, stays with the person who started the conversation, even when it is
too short to recognize. "Yes" or "The kitchen" keeps that person's
instructions.

When the satellite recognizes someone else within the 5 minutes of a
conversation, it starts a new one, and the new speaker doesn't get the earlier
turns. To let them carry on the same conversation, turn on **Continue the
conversation when someone else speaks**, under **Assistant > Conversation**.
Either way, the instructions follow the current speaker.

## Use Last speaker in Home Assistant

**Last speaker** is a text sensor. It is empty when a spoken request starts,
and then shows the person's name, or `Everyone else` when the satellite isn't
sure. In standalone mode, it shows the profile that the request used. Typed
questions don't change it.

The satellite usually sets it before Home Assistant has the text of the
request, so an automation can read it. An automation that runs while it is
still empty should treat the speaker as unknown. On a follow-up, **Last
speaker** can show `Everyone else` first, and the name of the person the
conversation belongs to once the request has its text.

For example, this automation answers only when the person named Anna asks. The
entity ID is `sensor.` and the device's name, as for the other
[entities](/docs/entities/), followed by `_last_speaker`:

```yaml
alias: Greet Anna
triggers:
  - trigger: conversation
    command: "good morning"
conditions:
  - condition: state
    entity_id: sensor.kitchen_dot_last_speaker
    state: "Anna"
actions:
  - set_conversation_response: "Good morning, Anna."
```

If you rename a person, **Last speaker** shows the new name from then on.
Change the automations that compare the old name.

## Rename or remove a person

1. Open **Voice & sound > People**, and select **Change** next to the person.
2. To rename, edit the **Name** and select **Save**.
3. To remove, select **Remove…**, then **Remove**. This deletes the person's
   name, extra instructions and voiceprint from the satellite. A request that
   is running finishes with the profile it started with.

When the last person is removed, the satellite unloads the speaker model.

## If the satellite doesn't recognize you

- Speak a little longer. Recognition needs at least half a second of speech,
  and works better with a full sentence than with a single word.
- Stand closer, and turn off music or a TV.
- Select **Enroll again** for your name, within 1 m of the satellite, in the
  room where you use it.
- Check that the page says **Voice recognition is on**. After a start, the
  satellite needs several seconds to load the speaker model. Until then,
  requests run as **Everyone else**.
- With debugging on, the [timeline](/docs/troubleshooting/#follow-a-request-on-the-timeline)
  shows each recognition run as a stage, with the person it heard, the score,
  the margin to the next best person and the seconds of speech it used, and
  which profile the request used. A run's outcome is a name, `uncertain`,
  `too short`, `no model` or `failed`. The profile line says why: `recognized`,
  `carried over on a follow-up`, `person removed` (the person was removed
  meanwhile), or, for **Everyone else**, `uncertain`, `too short`, `no model`,
  `recognition failed` or `too late` (recognition hadn't finished when the
  text arrived).

See also [The satellite doesn't recognize me](/docs/troubleshooting/#the-satellite-doesnt-recognize-me).

## What the satellite stores

On the satellite, in its own storage:

- the people's names and extra instructions, with those of **Everyone else**;
- one voiceprint for each person: a list of numbers that describes the voice.
  No audio is stored.

A factory reset deletes all of it. Removing a person deletes their voiceprint.
The satellite can't promise that flash storage overwrites the bytes.

## What's next

- Write the prompt every request starts with: [Set up standalone mode](/docs/standalone/#set-up-the-conversation).
- Use **Last speaker** in automations: [Home Assistant entities](/docs/entities/).
- Watch how often recognition is sure: [Prometheus metrics](/docs/metrics/#recognizing-who-speaks).
- For developers: manage people from scripts with the control API's
  [PeopleService](/docs/control-api/#peopleservice).
