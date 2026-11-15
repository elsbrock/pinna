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
| Blue, with a brighter part pointing at you | The device is listening to you. When it can't tell where you are, for example before you speak or in a noisy room, the whole ring slowly gets brighter and dimmer instead. You can turn the pointing off with the **Listening direction** switch in Home Assistant |
| A violet light that runs around the ring | Home Assistant is working on your request |
| Cyan, brighter as the voice gets louder | The device is speaking a reply or an announcement |
| A green arc | Wake word calibration runs. The arc shows the sensitivity, and the volume buttons tune it. It shows in place of a running timer's arc, a notification, the light and the music. See [Calibrate at the device](/docs/dashboard/#calibrate-at-the-device) |
| An amber arc | A timer is running. The arc shows how much time is left |
| Yellow, the whole ring slowly getting brighter and dimmer | A notification is waiting. See [Notifications](#notifications) |
| Coloured lights that move with the music, the same on the left and the right: bass at the front, treble at the back | The media player is playing |
| Off | The device is idle |

Two short signals appear on top of these states:

| What you see | Meaning |
| --- | --- |
| Red, flashing three times | Something went wrong: Home Assistant refused or didn't answer the request, the microphones are off when you press the Action button, or a calibration +/− press changed nothing |
| Green, one flash | The wake word was heard while calibration runs. No request starts |
| A light blue arc, for 1.5 seconds | The volume changed. The arc shows the new level |

### Use the ring as a light

The ring is also a light in Home Assistant, named **LED ring**. You can set
its colour, its brightness and one of three effects:

- **Solid**: steady light.
- **Pulse**: slowly getting brighter and dimmer.
- **Flash**: on and off, every half second.

The light shows only while the device is idle. When the device listens, works
on a request or speaks, the ring shows that state, and the light comes back
afterwards. While a timer runs or a notification is waiting, the ring shows
that instead of the light. The light takes the place of the music lights.

**LED brightness** in Home Assistant sets the brightness of the whole ring,
including the states above.

### Notifications

The ring can tell you that something happened, like the yellow ring on an
Echo. While a notification is waiting and the device is idle, the whole ring
slowly gets brighter and dimmer in yellow.

The **Notification** switch in Home Assistant controls it. Turn it on from an
automation, for example when the washing machine finishes. The notification
stays until one of these clears it:

- You press the Action button while the ring shows the notification. This
  press doesn't start a request: the ring flashes once and goes out. If you
  set up the read-out in step 4 below, the device reads the message. Press
  again after it finishes to talk. The Action button's `press` event still
  fires.
- You select **Clear** on the [dashboard](/docs/dashboard/).
- Home Assistant turns the switch off.

When you clear it with the Action button or on the dashboard, the
**Notification** event fires with the type `cleared`, so an automation can
tell you what the notification was. When Home Assistant turns the switch off,
the event doesn't fire.

The notification doesn't show while the microphones are off, while a timer
alarm rings, while the device listens, works on a request or speaks, or while
a timer runs. It comes back afterwards. While the ring shows something else,
the Action button does what it normally does. A waiting notification survives
a restart of the device.

#### Hear what the notification was

The switch only says that something happened. To keep the message, store it
in a Home Assistant helper.

The examples use the entity IDs of a device named **Echo Dot**, such as
`switch.echo_dot_notification`. To find your device's entity IDs, go to
**Settings > Devices & services > ESPHome** and select the device. To add an
automation, go to **Settings > Automations & scenes > Create automation >
Create new automation**, open the menu, select **Edit in YAML**, and paste it.

1. Go to **Settings > Devices & services > Helpers > Create helper > Text**.
   Name it `Pinna notification`. Under **More options**, set **Maximum
   length** to 255.

2. Add these two actions to the `actions:` of the automation that tells you
   the washing machine is done. They add the message to the helper, or start
   a new message if no notification is waiting, and turn the switch on:

   ```yaml
   - action: input_text.set_value
     target:
       entity_id: input_text.pinna_notification
     data:
       value: >-
         {% set old = states('input_text.pinna_notification')
            if is_state('switch.echo_dot_notification', 'on')
               and has_value('input_text.pinna_notification') else '' %}
         {% set max = state_attr('input_text.pinna_notification', 'max') | int(100) %}
         {{ ((old ~ ' The washing machine is done.') | trim)[-max:] }}
   - action: switch.turn_on
     target:
       entity_id: switch.echo_dot_notification
   ```

3. Add an automation that answers "Alexa, what are my notifications?" and
   clears them:

   ```yaml
   alias: Read Pinna notifications
   triggers:
     - trigger: conversation
       command:
         - "what are my notifications"
         - "read my notifications"
   actions:
     - variables:
         message: >-
           {{ states('input_text.pinna_notification')
              if is_state('switch.echo_dot_notification', 'on')
                 and has_value('input_text.pinna_notification') else '' }}
     - set_conversation_response: "{{ message or 'You have no notifications.' }}"
     - action: switch.turn_off
       target:
         entity_id: switch.echo_dot_notification
     - action: input_text.set_value
       target:
         entity_id: input_text.pinna_notification
       data:
         value: ""
   ```

   Home Assistant matches these sentences itself. If your voice assistant
   uses an AI conversation agent, turn on **Prefer handling commands
   locally** for it.

4. Add an automation that reads the message when you clear the notification
   with the Action button or on the dashboard:

   ```yaml
   alias: Read a Pinna notification when it's cleared
   triggers:
     - trigger: state
       entity_id: event.echo_dot_notification
       not_from: unavailable
       not_to: unavailable
   conditions:
     - condition: template
       value_template: >-
         {{ trigger.to_state.attributes.event_type == 'cleared'
            and has_value('input_text.pinna_notification')
            and states('input_text.pinna_notification') != '' }}
   actions:
     - action: assist_satellite.announce
       target:
         entity_id: assist_satellite.echo_dot_assist_satellite
       data:
         message: "{{ states('input_text.pinna_notification') }}"
     - action: input_text.set_value
       target:
         entity_id: input_text.pinna_notification
       data:
         value: ""
   ```

If Home Assistant isn't connected when you clear a notification on the device,
Home Assistant sees the switch turn off but gets no `cleared` event, and
nothing is read out. The next notification starts a new message.

With more than one device, change the automations so that clearing the
notification on one device clears it on all of them:

- In steps 2 and 3, list every device's switch under `entity_id`.
- Replace step 4 with this version. It listens to every device's event, reads
  the message on the device that was cleared, and turns every switch off. The
  example has two devices, named **Echo Dot** and **Kitchen Dot**:

  ```yaml
  alias: Read a Pinna notification when it's cleared
  triggers:
    - trigger: state
      entity_id:
        - event.echo_dot_notification
        - event.kitchen_dot_notification
      not_from: unavailable
      not_to: unavailable
  conditions:
    - condition: template
      value_template: >-
        {{ trigger.to_state.attributes.event_type == 'cleared'
           and has_value('input_text.pinna_notification')
           and states('input_text.pinna_notification') != '' }}
  actions:
    - action: assist_satellite.announce
      target:
        entity_id: >-
          {{ device_entities(device_id(trigger.entity_id))
             | select('match', 'assist_satellite') | first }}
      data:
        message: "{{ states('input_text.pinna_notification') }}"
    - action: switch.turn_off
      target:
        entity_id:
          - switch.echo_dot_notification
          - switch.kitchen_dot_notification
    - action: input_text.set_value
      target:
        entity_id: input_text.pinna_notification
      data:
        value: ""
  ```

## Buttons

Seen from above, with the front of the device towards you, the buttons are:

- **Action** (a dot), at the back
- **Volume down** (−), on the right
- **Microphone off** (a crossed-out microphone), at the front
- **Volume up** (+), on the left

| Button | Press | Hold |
| --- | --- | --- |
| Action | Starts a request without the wake word. While the device listens or answers, cancels the request. While a timer rings, stops it. While calibration runs, ends it. While the ring shows a notification, clears it | Sends a `long_press` event to Home Assistant after 2 seconds |
| Volume up | Raises the volume by 5. While calibration runs, raises the wake word sensitivity to catch the strongest near miss heard in the last 15 seconds instead | Keeps raising the volume |
| Volume down | Lowers the volume by 5. While calibration runs, lowers the wake word sensitivity to ignore the last wake instead | Keeps lowering the volume |
| Microphone off | Turns the microphones off or on | Nothing |

Every press also sends an event to Home Assistant, so you can use the buttons
in automations. While calibration runs, the volume buttons tune the
sensitivity instead and don't send theirs. See
[Home Assistant entities](/docs/entities/#events).

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
