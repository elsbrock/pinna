---
title: Light ring and buttons
description: What each colour and animation of the light ring means, and what the four buttons do.
---

The device has no screen. The light ring shows what it is doing, and the four
buttons on top control it. This page describes both.

## Light ring

### While the device starts

| What you see | Meaning |
| --- | --- |
| A blue arc that grows clockwise from the Action button | Pinna is starting. The arc grows with each step: the storage, the root filesystem, the system, USB, the Wi-Fi driver, `pinnad` |
| The whole ring fills, then fades out | Pinna started and passed its health check |
| Green, slowly getting brighter and dimmer, or steady | Pinna is in rescue mode: it couldn't start. See [Rescue mode](/docs/recovery/#rescue-mode) |
| White, flashing a few times, then a pause, repeating | A start-up step didn't finish in time. Count the flashes: 4 is USB, 5 the Wi-Fi driver, 6 `pinnad`; 1 to 3 belong to rescue mode, where the page names them. The flashes stay while the device is idle, until the next restart |

If a step flashes, the device keeps running — nothing restarts itself. The
number says what to look at first; [Troubleshooting](/docs/troubleshooting/)
has the next steps.

### Once Pinna runs

The ring shows one state at a time. When two states apply, the higher one in
this table wins:

| What you see | Meaning |
| --- | --- |
| Solid red | The microphones are off |
| Amber, pulsing twice a second | A timer has finished. Say the wake word or press the Action button to stop it |
| Blue, with a brighter part pointing at you | The device is listening to you. When it can't tell where you are, for example before you speak or in a noisy room, the whole ring slowly gets brighter and dimmer instead. You can turn the pointing off with the **Listening direction** switch, on the dashboard under **Voice & sound** or in Home Assistant |
| A violet light that runs around the ring | Your request is being answered: by Home Assistant, or in standalone mode by your services |
| Cyan, brighter as the voice gets louder | The device is speaking a reply or an announcement |
| A green arc | Wake word calibration runs. The arc shows the sensitivity, and the volume buttons tune it. It shows in place of a running timer's arc, a notification, the light and the music. See [Calibrate at the device](/docs/dashboard/#calibrate-at-the-device) |
| Two blue lights that turn slowly around the ring | The Bluetooth pairing window is open: a phone nearby can pair now. It shows in place of a running timer's arc, a notification, the light and the music. See [Pair a phone](/docs/bluetooth/#pair-a-phone) |
| An amber arc | A timer is running. The arc shows how much time is left |
| Yellow, the whole ring slowly getting brighter and dimmer | A notification is waiting. See [Notifications](#notifications) |
| Coloured lights that move with the music, the same on the left and the right: bass at the front, treble at the back | The media player is playing |
| Off | The device is idle |

Short signals appear on top of these states:

| What you see | Meaning |
| --- | --- |
| Red, flashing three times | Something went wrong: Home Assistant isn't connected, refused the request or didn't answer it; in standalone mode, a service failed or the device heard no speech; the microphones are off when you press the Action button; or a calibration +/− press changed nothing. See [The ring flashes red three times](/docs/troubleshooting/#the-ring-flashes-red-three-times) |
| Green, one flash | The wake word was heard while calibration runs. No request starts |
| A light blue arc, for 1.5 seconds | The volume changed. The arc shows the new level |

### Use the ring as a light

The ring is also a light: **Light**, under **Voice & sound** on the
[dashboard](/docs/dashboard/), and **LED ring** in Home Assistant. You can set
its colour, its brightness and one of three effects. On the dashboard,
**Colour**, **Light brightness** and **Effect** appear once **Light** is on:

- **Solid**: steady light.
- **Pulse**: slowly getting brighter and dimmer.
- **Flash**: on and off, every half second.

The light shows only while the device is idle. When the device listens, works
on a request or speaks, the ring shows that state, and the light comes back
afterwards. While a timer runs or a notification is waiting, the ring shows
that instead of the light. The light takes the place of the music lights. It
goes off when the device restarts.

**Brightness** on the dashboard, **LED brightness** in Home Assistant, sets
the brightness of the whole ring, including the states above. The light's own
brightness works within it.

### Notifications

The ring can tell you that something happened. While a notification is
waiting and the device is idle, the whole ring slowly gets brighter and dimmer
in yellow.

Notifications need Home Assistant, in either mode: the **Notification** switch
in Home Assistant controls them. Turn it on from an
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
| Action | Starts a request without the wake word. While the device listens or answers, cancels the request. While it speaks an announcement, stops it. While a timer rings, stops it. While calibration runs, ends it. While the ring shows a notification, clears it | Sends a `long_press` event to Home Assistant after 2 seconds |
| Volume up | Raises the volume by 5. While calibration runs, raises the wake word sensitivity to catch the strongest near miss heard in the last 15 seconds instead | Keeps raising the volume |
| Volume down | Lowers the volume by 5. While calibration runs, lowers the wake word sensitivity to ignore the last wake instead | Keeps lowering the volume |
| Microphone off | Turns the microphones off or on | Nothing |

If you use Home Assistant, every press also sends it an event, so you can use
the buttons in automations. While calibration runs, the volume buttons tune the
sensitivity instead and don't send theirs. See
[Home Assistant entities](/docs/entities/#events).

### Microphone off

When the microphones are off, the ring is solid red and Pinna ignores all
audio from the microphones: it doesn't listen for the wake word, and a request
that is running stops.

The device has a hardware switch behind the **Microphone off** button. When
the hardware switch is present, it turns the microphones off, and Pinna
follows it. You can also turn the microphones off with the **Microphones**
switch on the [dashboard](/docs/dashboard/), under **Voice & sound** or on
**Overview**, or with the **Microphone mute** switch in Home Assistant. The
microphones are off when either the button or the software setting says so.

## What's next

- Change the ring brightness, the volume and the wake words on the
  [dashboard](/docs/dashboard/), under **Voice & sound**.
- See all entities in [Home Assistant entities](/docs/entities/).
