---
title: Light ring and buttons
description: What each colour and animation of the light ring means, and what the four buttons do.
---

The Echo Dot has no screen. The light ring shows what the device is doing, and
the four buttons on top control it. This page describes both.

## Light ring

The ring shows one state at a time. When two states apply, the higher one in
this table wins:

| What you see | Meaning |
| --- | --- |
| A blue light that runs around the ring, briefly | Pinna is starting |
| Solid red | The microphones are off |
| Amber, pulsing twice a second | A timer has finished. Press the Action button to stop it |
| Blue, with a brighter part pointing at you | The device is listening to you. When it can't tell where you are, for example before you speak or in a noisy room, the whole ring slowly gets brighter and dimmer instead |
| A violet light that runs around the ring | Home Assistant is working on your request |
| Cyan, slowly getting brighter and dimmer | The device is playing the reply |
| An amber arc | A timer is running. The arc shows how much time is left |
| Coloured lights that move with the music, the same on the left and the right: bass at the front, treble at the back | The media player is playing |
| Off | The device is idle |

Two short signals appear on top of these states:

| What you see | Meaning |
| --- | --- |
| Red, flashing three times | Something went wrong: Home Assistant refused or didn't answer the request, or the microphones are off when you press the Action button |
| A light blue arc, for 1.5 seconds | The volume changed. The arc shows the new level |

### Use the ring as a light

The ring is also a light in Home Assistant, named **LED ring**. You can set
its colour, its brightness and one of three effects:

- **Solid**: steady light.
- **Pulse**: slowly getting brighter and dimmer.
- **Flash**: on and off, every half second.

The light shows only while the device is idle. When the device listens, works
on a request or speaks, the ring shows that state, and the light comes back
afterwards. The light also takes the place of the timer arc and the music
lights.

**LED brightness** in Home Assistant sets the brightness of the whole ring,
including the states above.

## Buttons

Seen from above, with the front of the device towards you, the buttons are:

- **Action** (a dot), at the back
- **Volume down** (−), on the right
- **Microphone off** (a crossed-out microphone), at the front
- **Volume up** (+), on the left

| Button | Press | Hold |
| --- | --- | --- |
| Action | Starts a request without the wake word. While the device listens or answers, cancels the request. While a timer rings, stops it | Sends a `long_press` event to Home Assistant after 2 seconds |
| Volume up | Raises the volume by 5 | Keeps raising the volume |
| Volume down | Lowers the volume by 5 | Keeps lowering the volume |
| Microphone off | Turns the microphones off or on | Nothing |

Every press also sends an event to Home Assistant, so you can use the buttons
in automations. See [Home Assistant entities](/docs/entities/#events).

### Microphone off

When the microphones are off, the ring is solid red and Pinna ignores all
audio from the microphones: it doesn't listen for the wake word, and a request
that is running stops.

The Echo Dot has a hardware switch behind the **Microphone off** button. When
the hardware switch is present, it turns the microphones off, and Pinna
follows it. You can also turn the microphones off from Home Assistant (the
**Microphone mute** switch) or with `pinnactl mute on`. The microphones are off
when either the button or the software setting says so.

## What's next

- See all entities in [Home Assistant entities](/docs/entities/).
- Change the ring brightness, the volume and the wake word in
  [Configure Pinna](/docs/configure/).
