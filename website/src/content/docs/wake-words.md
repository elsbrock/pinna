---
title: Wake words
description: Choose the wake words the satellite listens for, add your own, and tune how easily it wakes.
---

The satellite listens for at most two wake words at once. Until it hears one,
no audio leaves the device. A standard image has five: Alexa, Hey Jarvis, Hey
Mycroft, Hey Rhasspy and Okay Nabu. A new satellite listens for Alexa. You
can [add your own](#add-your-own-wake-word).

## Choose the wake words

Who chooses depends on the mode:

- In Home Assistant mode, while Home Assistant is connected, Home Assistant
  chooses. See [In Home Assistant](#in-home-assistant).
- In standalone mode, or while Home Assistant isn't connected, the dashboard
  chooses.

### On the dashboard

Open **Voice & sound**, and choose the **Wake word** and the **Second wake
word**. The lists offer the wake words whose models the image ships, and the
ones you [added](#add-your-own-wake-word).

### In Home Assistant

While Home Assistant is connected in Home Assistant mode, it chooses the
wake words. Open the device page in **Settings > Devices & services >
ESPHome**: under **Configuration**, the **Wake word** and **Wake word 2**
selectors list every word the device can listen for — Alexa, Hey Jarvis, Hey
Mycroft, Hey Rhasspy and Okay Nabu on a standard image — and the words you
[added on the dashboard](#add-your-own-wake-word), unless
their model failed to load. Set one or
both, or set both to **No wake word** to listen for none.

Home Assistant reads the list of words when it connects. After you add or
remove a word, the device makes Home Assistant reconnect once no request,
reply, announcement, music or timer alarm plays, and the selectors show the
new list.

While Home Assistant is connected, it owns this choice: the device's
[dashboard](/docs/dashboard/) shows the active words but doesn't change them.
The selectors read the device's words when Home Assistant connects; a word
chosen there while Home Assistant was away is adopted only if it is the single
active word, and otherwise shows as **No wake word** until you choose it in Home Assistant.
The device keeps listening for its own words meanwhile, and the choice is saved
on the device, so the satellite wakes without Home Assistant and survives
restarts.

## Add your own wake word

You can add up to eight wake words of your own. Each is an openWakeWord
model, an `.onnx` or `.tflite` file of at most 2 MiB. Pinna can't run
microWakeWord models, which ESPHome devices such as Voice PE use.

To get a model:

- Download one from the
  [Home Assistant Wake Word Collection](https://github.com/fwartner/home-assistant-wakewords-collection).
  It has about 120 words that Home Assistant users trained and shared, sorted
  by language. Most words come as both `.onnx` and `.tflite`, and Pinna takes
  either. The quality varies from word to word.
- Train your own with
  [Create your own wake word](https://www.home-assistant.io/voice_control/create_wake_word/)
  in the Home Assistant documentation. The training runs in Google Colab, in
  your browser, and gives you a `.tflite` and an `.onnx` file. The guide's
  last steps add the word to Home Assistant's openWakeWord add-on. To use
  the word on the satellite, add it here instead.

1. Open **Voice & sound > Wake words**.
2. Under **Add a wake word**, enter the **Phrase**: the phrase the model was
   trained on, 2 to 40 characters. It must be different from the phrase of
   every other wake word on the satellite, ignoring case, spaces and
   punctuation.
3. Under **Model file**, select **Choose file** and choose the model.
4. Select **Add**.

The satellite tests the model before it keeps it. The test runs in a process
of its own, with limits on memory and time, so a model that breaks the test
doesn't stop the satellite. The model must load, take at most 8 ms for each
80 ms of audio, and stay below the wake threshold at the current sensitivity
on silence and noise. The test takes from a few seconds up to a minute. If
the satellite refuses the model, the page says why, and nothing is kept. See
[The device refuses a wake word](/docs/troubleshooting/#the-device-refuses-a-wake-word).

The satellite doesn't listen for the new word yet. Choose it like the
built-in words: in Home Assistant mode, while Home Assistant is connected, on
the device page in Home Assistant (see
[In Home Assistant](#in-home-assistant));
otherwise under **Wake word** on **Voice & sound**. The test shows that the
model stays quiet when nobody speaks, not that it hears its phrase, so try
it:

1. Choose the new word alone, without a second wake word. **Recent wakes**
   names the word on a wake but not on a near miss.
2. Say it from where you usually speak, and watch **Recent wakes** on the
   **Wake word sensitivity** page. See [Tune the wake word](#tune-the-wake-word).

To remove a word you added, select **Remove** next to it on the **Wake words**
page. The satellite deletes its model and stops listening for it. If it was
listening for the word in Home Assistant mode while Home Assistant is
connected, choose another word in Home Assistant.

A word that the satellite can't listen for shows the reason under its phrase:
its model file is missing or changed, or its model didn't load. A model that
passed the test runs inside the satellite's software; if loading it still
stops that software, the satellite restarts it without the word, and the
word stays marked. You can't choose such a word. Remove it, and add a new
model for the phrase if you have one. A [factory reset](/docs/settings/#reset-the-device)
deletes the words you added.

## Tune the wake word

**Voice & sound > Sensitivity** opens the **Wake word sensitivity** page. The slider sets
how easily the device wakes: a higher setting wakes more easily, and more
often by mistake. Its hint shows the score that the setting needs.
The score is how sure the device is that it heard the wake word, from 0 to 1.
While you move the slider, the hint and the entries show what the new value
does.

**Recent wakes** lists the last 20 wakes and near misses. A near miss is a
sound that scored at least 0.20 but didn't wake the device; it may have
reached the setting and been rejected for another reason. Each entry shows
when it happened and its score; a wake also names the word that woke the
device. Each entry says what its score means at the current setting:

- **Woke**: the entry says which sensitivity ignores that score. If it wasn't
  you, select **Set to**.
- **Almost woke**: the entry says which sensitivity catches that score. If it
  was you, select **Set to**.

After the change, the same button says **Back to** and the old value. Select
it to undo the change.

The entry also says what else the new setting changes, for example "But it
also ignores 2 other wakes listed here". Check the times of those entries. If
ignoring a false wake also ignores your own wakes, or catching your near miss
also catches the TV, no setting fixes both. Move the device away from the TV or
the radio instead.

An entry without a button says why:

- **Ignored at this setting** or **Wakes at this setting**: the current
  setting already does what you want.
- **Too faint for any setting**: even 100 doesn't catch it. Speak closer, or
  with less noise around.
- **High enough at the time, but too quiet against the background or too short
  to count**: the device didn't accept it. Speak closer or louder, with less
  noise around.
- **Every setting wakes on this**: the score is 0.85 or more. If it wasn't
  you, move the device away from the TV or the radio, or turn off the wake word
  while it plays.

The list holds scores and wake word names, not what was said. It records only while the
device listens for the wake word. It starts again when `pinnad` or the device
restarts.

### Calibrate at the device

Select **Calibrate**, next to the slider, to tune the sensitivity from where
you usually stand, using the buttons on the device. The volume buttons become
feedback buttons while calibration runs: the ring shows the setting as a green
arc.

1. Say the wake word. When the device hears it, the ring flashes green and the
   wake sound plays, if you turned it on. Nothing else happens: no request
   starts, and a false wake costs nothing.
2. If it missed you, press **+** within 15 seconds. The sensitivity rises just
   enough to catch what it almost heard.
3. If it woke on something that wasn't you, such as the TV, press **−**, also
   within 15 seconds. The sensitivity drops just enough to ignore that.

The panel under the slider lists what each press did, for example
`+ raised the sensitivity 68 → 90, to catch a near miss at 0.48`. If a **+**
and a **−** would undo each other, because your wake and the false one score
about the same, the second press changes nothing: the ring flashes red at the
device, and the panel says why. No setting settles both, so move the device
away from the sound instead.

Select **Done**, press the Action button, or leave it 5 minutes without a
press to finish. Muting the microphones or turning the wake word off ends it
too. **Back to 68** returns the sensitivity to the value it had when you
started. Home Assistant has the same switch, **Wake word calibration**.
Calibration won't start while the microphones or the wake word are off, or
while a request or a timer alarm is active. It also works before Home
Assistant connects, because a calibration wake starts no request.

## What's next

- If the satellite doesn't wake, or wakes by itself, see
  [Troubleshooting](/docs/troubleshooting/#the-device-doesnt-react-to-the-wake-word).
- Learn what the [light ring and the buttons](/docs/ring-and-buttons/) show
  and do.
