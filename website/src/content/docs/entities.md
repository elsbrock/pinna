---
title: Home Assistant entities
description: Every entity that a Pinna device adds to Home Assistant, with its type, values and effect on the device.
---

When you add a Pinna device to Home Assistant, it creates the entities on this
page. Object IDs are shown in code font. Home Assistant builds entity IDs from
the device name and the entity name, for example
`light.echo_dot_2a914e_led_ring` for a device named **Echo Dot 2a914e**. They
keep the name the device had when you added it.

## Controls

| Entity | Object ID | Type | Values | What it does |
| --- | --- | --- | --- | --- |
| LED ring | `led_ring` | Light | Colour, brightness, effects Solid, Pulse and Flash | Uses the ring as a light while the device is idle. See [Light ring and buttons](/docs/ring-and-buttons/#use-the-ring-as-a-light) |
| Microphone mute | `microphone_mute` | Switch | On, off | Turns the microphones off in software. Shows on when the microphones are off for any reason, including the hardware button |
| Wake word | `wake_word` | Switch (configuration) | On, off | Turns wake word detection on or off. The words themselves are chosen with the **Wake word** and **Wake word 2** selectors on the device page. The Action button still starts a request |
| Listening direction | `listening_direction` | Switch (configuration) | On, off | While the device listens, points the light ring at the person speaking. Off shows the plain listening ring. On by default |
| Wake sound | `wake_sound` | Switch (configuration) | On, off | Plays a short chime when the device starts listening. The dashboard has the same switch, under **Voice & sound** |
| AirPlay | `airplay` | Switch (configuration) | On, off | Turns the AirPlay receiver on or off. While it is on, iPhones, Macs and Music Assistant on your network can play music to the device, without a password. Off by default. The dashboard has the same switch, under **Voice & sound > Media sources** |
| Bluetooth audio | `bluetooth` | Switch (configuration) | On, off | Turns Bluetooth on or off. While it is on, phones paired with the device can play music to it. Off by default. The dashboard has the same switch, under **Voice & sound > Media sources** |
| Bluetooth pairing | `bluetooth_pairing` | Switch | On, off | Turn it on to open the Bluetooth pairing window for two minutes, like **Pair a phone** on the dashboard; turn it off to close it. It turns itself off when the window closes, and stays off unless **Bluetooth audio** is on and ready. See [Pair a phone](/docs/bluetooth/#pair-a-phone) |
| Wake word calibration | `wake_word_calibration` | Switch (configuration) | On, off | Starts tuning the wake word sensitivity with the + and − buttons on the device, as [Calibrate at the device](/docs/dashboard/#calibrate-at-the-device) describes. It turns itself off when calibration ends. It won't turn on while the microphones or the wake word are off, or while a request or a timer alarm is active |
| Notification | `notification` | Switch | On, off | Turn it on to show a waiting notification on the ring. It turns off when someone clears it on the device or the dashboard. See [Notifications](/docs/ring-and-buttons/#notifications) |
| Volume | `volume` | Number | 0 to 100, in % | Sets the speaker volume. 0 is silent |
| Reply volume | `reply_volume` | Number | 0 to 100, in % | Sets the volume of replies and announcements relative to the speaker volume. Replies are never louder than the speaker volume |
| Wake word sensitivity | `wake_word_sensitivity` | Number | 0 to 100 | Sets how easily the wake word triggers. Higher values trigger more easily, and more often by mistake. The default is 68. The dashboard's [Wake word page](/docs/dashboard/#tune-the-wake-word) shows which setting catches or ignores each recent wake and near miss |
| LED brightness | `led_brightness` | Number | 0 to 100, in % | Sets the brightness of the whole ring |
| Cancel request | `cancel_request` | Button | | Stops a running request, a reply or an announcement, and silences a timer alarm |
| Media player | `media_player` | Media player | Play, pause, stop, volume, mute, announce | Plays audio from a URL and announcements. See [Media player](#media-player) |

Pinna saves the switch and number values on the device, so they survive a
restart.

## Sensors

| Entity | Object ID | Type | Value |
| --- | --- | --- | --- |
| Last wake word | `last_wake_word` | Text sensor | The wake word heard most recently, for example `Hey Jarvis`. With two words active, it names the one that fired |

## Events

Events don't have a state. Use them as triggers in automations.

| Entity | Object ID | Event types | Fires when |
| --- | --- | --- | --- |
| Action button | `action` | `press`, `double_press`, `long_press` | The Action button is pressed. `long_press` fires after holding it for 2 seconds |
| Volume up button | `volume_up` | `press`, `double_press` | The Volume up button is pressed |
| Volume down button | `volume_down` | `press`, `double_press` | The Volume down button is pressed |
| Microphone mute button | `microphone_mute` | `press`, `double_press` | The Microphone off button is pressed |
| Voice assistant | `voice_assistant` | `wake_word`, `listening`, `thinking`, `replying`, `finished`, `stopped` | A voice request moves to the next step |
| Playback | `playback` | `started`, `stopped` | The media player starts or stops playing |
| Timer | `timer` | `started`, `updated`, `cancelled`, `finished` | A timer on the device changes |
| Notification | `notification` | `cleared` | Someone clears the notification with the Action button or on the dashboard. It doesn't fire when Home Assistant turns the switch off |

`replying` fires when the reply starts to play, not when Home Assistant
sends it. If the reply can't play, there is no `replying`, and the request
ends with `finished`.

A `double_press` fires when you press the same button twice within 0.4
seconds. The button still does its normal job for both presses. While wake
word calibration runs, the volume buttons tune the sensitivity instead and
don't fire their events.

## Media player

The media player plays two kinds of audio:

- **Media**, such as music or a radio stream, from a URL. Pinna plays MP3,
  WAV, FLAC and Ogg Vorbis. While the device listens, works on a request or
  speaks, media gets quieter and comes back afterwards.
- **Announcements**, from Home Assistant's announcement actions. An
  announcement interrupts a running request. Home Assistant can ask the device
  to listen for an answer after the announcement.

It supports play, pause, stop, mute and volume. The volume up and down
commands change the volume by 5.

While [AirPlay](/docs/airplay/#play-music) or
[Bluetooth](/docs/bluetooth/#play-music) plays, the media player's volume is
the music's own, and pause stops the music.

## Voice assistant and timers

The device works as an Assist satellite. It listens for the wake words chosen
in Home Assistant or on the [dashboard](/docs/dashboard/), up to two at once;
see [Choose the wake words](/docs/home-assistant/#choose-the-wake-words). It
supports timers: when you set a timer by voice, the ring shows an amber arc
while it runs. When the timer ends, an alarm plays until you say the wake
word, press the Action button, use **Cancel request**, or 5 minutes pass. The
wake word only stops the alarm; it doesn't start a request.

## What's next

- To change the device name, see
  [Name the device](/docs/dashboard/#name-the-device).
- To control the device from its command line, see
  [pinnactl and the control API](/docs/control-api/).
