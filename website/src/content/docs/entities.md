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
| Microphone mute | `microphone_mute` | Switch | On, off | Turns the microphones off in software. Shows on when the microphones are off for any reason, including the hardware button or switch. Turning it off doesn't open the hardware switch: after the **Microphone off** button or a request by voice turned the microphones off, press the button; on the Echo Dot (5th gen), move its microphone switch. See [Microphone off](/docs/ring-and-buttons/#microphone-off) |
| Wake word | `wake_word` | Switch (configuration) | On, off | Turns wake word detection on or off. The words themselves are chosen with the **Wake word** and **Wake word 2** selectors on the device page. The Action button still starts a request |
| Listening direction | `listening_direction` | Switch (configuration) | On, off | While the device listens, points the light ring at the person speaking. Off shows the plain listening ring. On by default |
| Wake sound | `wake_sound` | Switch (configuration) | On, off | Plays a short chime when the device starts listening. The dashboard has the same switch, under **Voice & sound** |
| AirPlay | `airplay` | Switch (configuration) | On, off | Turns the AirPlay receiver on or off. While it is on, iPhones, Macs and Music Assistant on your network can play music to the device, without a password. Off by default. The dashboard has the same switch, under **Voice & sound > Media sources** |
| Bluetooth audio | `bluetooth` | Switch (configuration) | On, off | Turns Bluetooth on or off. While it is on, phones paired with the device can play music to it. Off by default. The dashboard has the same switch, under **Voice & sound > Media sources** |
| Bluetooth pairing | `bluetooth_pairing` | Switch | On, off | Turn it on to open the Bluetooth pairing window for two minutes, like **Pair a phone** on the dashboard; turn it off to close it. It turns itself off when the window closes, and stays off unless **Bluetooth audio** is on and ready. See [Pair a phone](/docs/bluetooth/#pair-a-phone) |
| Sendspin | `sendspin` | Switch (configuration) | On, off | Turns the Sendspin player on or off. While it is on, Music Assistant 2.10.7 or later on your network can play to the device, in step with the other speakers of a group, without pairing. Experimental. Off by default. It stays off on a device without a working speaker. The dashboard has the same switch, under **Voice & sound > Media sources**. See [Play music with Sendspin](/docs/sendspin/) |
| Wake word calibration | `wake_word_calibration` | Switch (configuration) | On, off | Starts tuning the wake word sensitivity with the + and − buttons on the device, as [Calibrate at the device](/docs/wake-words/#calibrate-at-the-device) describes. It turns itself off when calibration ends. It won't turn on while the microphones or the wake word are off, or while a request or a timer alarm is active |
| Notification | `notification` | Switch | On, off | Turn it on to show a waiting notification on the ring. It turns off when someone clears it on the device or the dashboard. See [Notifications](/docs/ring-and-buttons/#notifications) |
| Group messages | `group_messages` | Switch | On, off | Lets the device take messages from the other satellites of its group, in either mode. On by default. Turn it off to refuse new messages; messages the device already holds still play. Pinna has no schedule for it: to turn it off at night, use an automation. It does nothing outside a group. See [Pass messages between satellites](/docs/satellite-groups/#pass-messages-between-satellites) |
| Volume | `volume` | Number | 0 to 100, in % | Sets the speaker volume. 0 is silent |
| Reply volume | `reply_volume` | Number | 0 to 100, in % | Sets the volume of replies and announcements, on the same scale as **Volume** and never louder than it. Until it is set, it follows **Volume**; the dashboard's **Replies follow Volume** switch makes it follow again. See [Volume](/docs/audio/#volume) |
| Wake word sensitivity | `wake_word_sensitivity` | Number | 0 to 100 | Sets how easily the wake word triggers. Higher values trigger more easily, and more often by mistake. The default is 68. The dashboard's [Wake word page](/docs/wake-words/#tune-the-wake-word) shows which setting catches or ignores each recent wake and near miss |
| LED brightness | `led_brightness` | Number | 0 to 100, in % | Sets the brightness of the whole ring |
| Cancel request | `cancel_request` | Button | | Stops a running request, a reply or an announcement, and silences a timer alarm. With none of them, it stops the music; with Sendspin, the whole group |
| Media player | `media_player` | Media player | Play, pause, stop, volume, mute, announce | Plays audio from a URL and announcements. See [Media player](#media-player) |
| Updates | `update_channel` | Select (configuration) | `stable`, `beta`, `off` | Where the device looks for new releases, as on the dashboard's **System > Updates** page. While a developer's own update address replaces the channel, its only option is that address ([Offer your own builds as updates](/docs/releases/#offer-your-own-builds-as-updates)). See [How the satellite finds updates](/docs/update/#how-the-satellite-finds-updates) |
| Install updates | `install_updates` | Select (configuration) | `Manually`, `Automatically` | Whether the device installs a new release by itself, inside the install window. `Manually` is the default. The dashboard has the same setting, under **System > Updates**. See [Install updates automatically](/docs/update/#install-updates-automatically) |
| Install window start | `install_window_start` | Time (configuration) | A time of day, 03:00 by default | When the install window opens, in the device's time zone. Used only while **Install updates** is `Automatically` |
| Install window end | `install_window_end` | Time (configuration) | A time of day, 05:00 by default | When the install window closes. It may be earlier than the start, for a window that crosses midnight. The device refuses an end that equals the start, and the entity shows the old time again |
| Voice mode | `assistant` | Select (configuration) | `Home Assistant`, `Standalone` | Who answers voice requests, as **Mode** on the dashboard's **Assistant** page. `Standalone` works only once speech-to-text, the brain and text-to-speech have passed their check on the dashboard; until then the select goes back to `Home Assistant`. A change while the satellite answers a request (with its follow-up), plays an announcement or rings an alarm waits until that has ended. Switching deletes every timer and reconnects Home Assistant. See [Set up standalone mode](/docs/standalone/) |
| Firmware | `firmware` | Update (configuration) | Installed and latest version, summary, release notes, progress | Shows a newer release on the device's channel, and installs it. After an install from Home Assistant, the device restarts on its own once it is Idle with no timer set and nothing playing. Unknown until a check has succeeded, and while **Updates** is off. See [Install an update from Home Assistant](/docs/update/#install-an-update-from-home-assistant) |

Pinna saves the switch, number, select and time values on the device, so they
survive a restart.

## Sensors

| Entity | Object ID | Type | Value |
| --- | --- | --- | --- |
| Last wake word | `last_wake_word` | Text sensor | The wake word heard most recently, for example `Hey Jarvis`. With two words active, it names the one that fired |
| Debugging | `debugging` | Binary sensor | On while debugging runs on the satellite. You can only read it: debugging starts on the dashboard, under **System > Timeline**, and ends there, with **Stop it** on **Overview**, or when its time runs out |

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
  to listen for an answer after the announcement. In standalone mode, only the
  media player's announcements play: there is no Assist satellite to announce
  on or to ask for an answer.

It supports play, pause, stop, mute and volume. The volume up and down
commands change the volume by 5. A new URL replaces what plays once its audio
starts. If it doesn't play, what played before keeps playing.

While [AirPlay](/docs/airplay/#play-music) or
[Bluetooth](/docs/bluetooth/#play-music) plays, the media player's volume is
the music's own, and pause stops the music.

While [Sendspin](/docs/sendspin/#play-music) plays, the media player's
volume and mute are the device's own in Music Assistant, and play, pause and
stop act on the whole group. After a pause from the device, the media player
shows the group as paused, and play and stop act on the group; its volume is
the device's overall **Volume** meanwhile, not its volume in Music Assistant. After a stop, at the end of the queue, or
after a pause in Music Assistant, the media player is idle. With no request
running and no alarm ringing, **Cancel request** stops the group too.

The [dashboard](/docs/music/#play-music-from-a-url) shows the same media
player and plays a URL too. Its **Now playing** shows media that Home Assistant
plays as **From Home Assistant**, without a title or a station's name: Home
Assistant converts the media for the device first, and that removes them. A
radio station's address played from the dashboard shows its titles.

## Voice assistant and timers

In Home Assistant mode, the device works as an Assist satellite. It listens
for the wake words chosen in Home Assistant or on the
[dashboard](/docs/dashboard/), up to two at once;
see [Choose the wake words](/docs/home-assistant/#choose-the-wake-words). It
supports timers: when you set a timer by voice, the ring shows an amber arc
while it runs. When the timer ends, an alarm plays until you say the wake
word, press the Action button, use **Cancel request**, or 5 minutes pass. The
wake word only stops the alarm; it doesn't start a request.

### In standalone mode

In [standalone mode](/docs/standalone/), the device answers requests itself,
with the services you set up on the dashboard. It reports no voice assistant
features to Home Assistant, so Home Assistant shows no Assist satellite for
it, and no wake word selectors. The device doesn't send Home Assistant any
microphone audio.

The other entities on this page keep working: the media player and its
announcements, the volume, the microphone mute, the ring, the buttons and the
events. The `voice_assistant` and `timer` events fire for the device's own
requests and timers.

The device ignores what only an Assist satellite does: Home Assistant's
requests, timers and wake word choice, and its announcements through the
Assist satellite. The device keeps its timers itself, and the dashboard
chooses the wake words. Switching the mode deletes every timer, Home
Assistant's included.

The **Voice mode** select stays, so you can switch back to Home Assistant from
there, or say "switch to Home Assistant" to the satellite. **Group messages**
stays too, so an automation can turn messages from the other satellites off
and on. **Install updates** and the install window stay as well. Standalone mode's other settings have no entities. You set them on
the dashboard.

## What's next

- To change the device name, see
  [Name the device](/docs/settings/#name-the-device).
- To use the buttons and the ring in automations, see
  [Light ring and buttons](/docs/ring-and-buttons/).
