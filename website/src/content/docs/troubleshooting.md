---
title: Troubleshooting
description: Find out why a satellite doesn't connect, doesn't hear the wake word or doesn't start, and what to do about it.
---

Start with the satellite's own view of the problem, on the
[dashboard](/docs/dashboard/):

- **Overview** shows what the satellite is doing, whether Home Assistant or
  your standalone services are ready, and, under the **Wake word** switch,
  whether it listens for the wake word and why not.
- **System > Logs** shows the log of `pinnad`, the program that runs the
  satellite, as it is written. The sections below quote the lines to look for.
- **System**, under **Troubleshooting**, says how the last start ended, for
  example `restarted (update)` or `crashed: a kernel panic or oops`.

If the dashboard doesn't open, start with
[The dashboard doesn't open](#the-dashboard-doesnt-open).

## The dashboard doesn't open

- **At its `.local` name**, such as `http://pinna-2a914e.local/`: your
  computer or phone needs mDNS, which Android often lacks. Use the device's IP
  address from your router instead.
- **Over Wi-Fi at all**: the device may not be on your network. Connect it to
  your computer with the USB cable and open `http://10.42.0.1/`. See
  [The device has no Wi-Fi](#the-device-has-no-wi-fi).
- **Over USB**: check that your computer sees the USB device **Pinna (Echo Dot
  2nd gen)**, or **Pinna (Echo 2nd gen)** on an Echo. Use a cable that carries
  data. Your computer gets an address between `10.42.0.10` and `10.42.0.29`.
- **The USB device is named Pinna rescue, or the ring is green**: Pinna
  couldn't start. See [The device doesn't boot Pinna](#the-device-doesnt-boot-pinna).
- **The ring flashes white, pauses and repeats**: a start-up step didn't
  finish. The number of flashes says which; see
  [While the device starts](/docs/ring-and-buttons/#while-the-device-starts).

## The device has no Wi-Fi

Connect the device to your computer with the USB cable, open
`http://10.42.0.1/` and select **Network**. The page says which of these cases
applies:

- **Not connected to a Wi-Fi network**: join your network under **Join a
  network**. If the password is wrong or the network gives the device no
  address, the device goes back to the network it was on, and the page says
  why. See [Change the Wi-Fi network](/docs/dashboard/#change-the-wi-fi-network).
- **Connected to a network, but no IPv4 address yet**: the network's DHCP
  server hasn't answered. This usually sorts itself out within a minute. If it
  doesn't, check that your router gives addresses to new devices, or join
  another network.
- **No Wi-Fi interface**: powering up the Wi-Fi failed, or the Wi-Fi firmware
  is missing from the image. The Wi-Fi chip allows one power-up attempt per
  start, so restart the device to try again: select **Restart…** on **System**.

  The dashboard's log doesn't show the Wi-Fi service yet. As a stopgap, a
  developer can read it on the satellite, over SSH or the USB serial console
  (see [Log in to a device](/docs/development/#log-in-to-a-device)):

  ```sh
  journalctl -b -u pinna-wifi -u wpa_supplicant@wlan0
  ```

  The `pinna-wifi` lines show the step that failed, for example
  `wlan0 did not appear`.

## Home Assistant doesn't find the device

1. Check that Pinna announces itself. On the dashboard, open **System >
   Logs**: it contains `published _esphomelib._tcp as …`, written when Pinna
   starts. If the log doesn't reach back that far, restart the device.

2. Home Assistant finds devices by mDNS, which doesn't cross network
   boundaries. If Home Assistant runs in another network, VLAN or container
   network, add the device by hand with the host and port from the
   dashboard's **Assistant** page: **Add integration > ESPHome**.
3. If Home Assistant connects and then fails, its key and the device's don't
   match, for example after a factory reset or a reinstall. The dashboard's
   **Assistant** page shows whether the device has a key. Remove the
   device from Home Assistant and add it again; if the device has a key that
   Home Assistant doesn't know,
   [reset the device](/docs/dashboard/#reset-the-device) first.

## AirPlay doesn't show the device

Check **Voice & sound > Media sources** on the dashboard. It says whether the
receiver runs and under which name. A sender on another network, VLAN or guest
Wi-Fi doesn't see the device. See
[If the device doesn't show up](/docs/airplay/#if-the-device-doesnt-show-up).

## A phone doesn't find the device over Bluetooth

Check **Voice & sound > Media sources** on the dashboard. A phone that hasn't
paired finds the device only while the pairing window is open. See
[If a phone doesn't find the device or doesn't connect](/docs/bluetooth/#if-a-phone-doesnt-find-the-device-or-doesnt-connect).

## The device doesn't react to the wake word

Open **Overview** on the dashboard. The hint under the **Wake word** switch
says whether the satellite listens, and why not:

| Hint | What to do |
| --- | --- |
| `Off. Press the action button to talk.` | Turn on **Wake word** |
| `No wake word is chosen.` | Choose a wake word. In Home Assistant mode, while Home Assistant is connected, choose it there: see [Choose the wake words](/docs/home-assistant/#choose-the-wake-words). Otherwise choose it on the dashboard, under **Voice & sound**. If **Voice & sound** says **No wake word models are installed on this satellite**, the image was built without them: run `just models`, build the image again, and install it as an [update](/docs/update/) |
| `Not listening for "Alexa" while the microphones are off` | Press **Microphone off** until the ring is no longer red. After a mute by voice, only the button turns the microphones back on; **Microphones** on the dashboard helps only when the dashboard or Home Assistant turned them off |
| `Not listening for "Alexa" until Home Assistant has a voice pipeline for this satellite` | Add the device in Home Assistant and choose an Assist pipeline for it. See [Connect to Home Assistant](/docs/home-assistant/). The **Home Assistant** tile on **Overview** then shows **Connected** and **Voice pipeline ready** |
| `Not listening for "Alexa" until the standalone services pass their check` | Open **Assistant > Services**: it shows which service isn't ready, and why. See [Standalone mode doesn't answer](#standalone-mode-doesnt-answer) |
| `Not listening for "Alexa" during the conversation` | Wait until the request ends |
| `Not listening for "Alexa" yet` | Wait a moment: the satellite is getting ready to listen |

If the hint says `Listening for "Alexa"` and the device still doesn't react:

- Check that the wake word model loaded. A line `wake word disabled: …` in
  **System > Logs** means the model files are there but didn't load.
- Check the microphones. If **System** shows **Audio backend** `none` under
  **Details**, Pinna found no audio device, and the log has the line
  `no audio backend: wake word and speech disabled`. With another backend, a
  `microphone unavailable` line means the microphones failed to open.
- Test the wake word on the **Wake word sensitivity** page:

  1. Open **Voice & sound > Sensitivity** in the
     [dashboard](/docs/dashboard/#tune-the-wake-word).
  2. Say the wake word from where you usually speak. Wait until the ring is
     off between tries.
  3. Each try shows in **Recent wakes**. A try that wakes the device shows at
     once. Don't select its **Set to** button: that is for false wakes. A near
     miss shows after about two seconds, with the setting that catches it.
     **Set to** sets it.
  4. If a try doesn't show, its score was below 0.20, and no setting helps.
     Speak closer, or with less noise around.

  You can tune with feedback at the device too: select **Calibrate** on the
  same page, then press **+** when it missed you and **−** when it woke by
  itself, as
  [Calibrate at the device](/docs/dashboard/#calibrate-at-the-device)
  describes. The ring flashes green on every wake while it runs.

  The log shows the scores too, in the lines `wake word "Alexa" detected (score …)`
  and `wake word near miss (Alexa): peak score …`, which names the word that
  scored. With two wake words active they share one sensitivity, so raising it
  affects both.

## The device wakes by itself

1. Soon after it happens, open **Voice & sound > Sensitivity** in the
   [dashboard](/docs/dashboard/#tune-the-wake-word). **Recent wakes** keeps
   only the last 20 entries, and it starts again when `pinnad` or the device
   restarts.
2. Find the wake in **Recent wakes** by its time, and select its **Set to**
   button. The entry says what else the new setting changes. If it also
   ignores your own wakes, no setting fixes both. Move the device away from the
   TV or the radio instead.
3. If the entry says **Every setting wakes on this**, move the device away
   from the sound, or turn off the wake word while it plays.

## The reply starts late or pauses

In Home Assistant mode, the log under **System > Logs** has a line for each
reply:

```text
reply: first audio 1.42 s after the end of speech (0.31 s after Home Assistant started it)
```

- The first number counts from the moment Home Assistant noticed that you
  stopped talking until the reply starts to play.
- The second number counts from the moment Home Assistant sent the link to the
  reply.

If the second number is small, most of the wait is speech-to-text and the
conversation agent. If it is close to the first number, the conversation
agent or the text-to-speech is slow to make the reply, or the Wi-Fi signal is
weak.

If the reply pauses in the middle, the log also has a line such as
`voice stream: fell behind 2 time(s), 1.3 s of silence`. The reply arrives
slower than it plays, for the same reasons.

To fix it:

- In Home Assistant, open **Settings > Voice assistants**, select the
  assistant that the device uses, and choose a faster speech-to-text,
  conversation agent or text-to-speech.
- Check the signal strength on the **Network** page of the
  [dashboard](/docs/dashboard/#what-the-dashboard-shows). Move the device
  closer to the access point if the signal is weak.

In standalone mode, the log has one line per request with the time each
service took; see **Replies are slow** under
[Standalone mode doesn't answer](#standalone-mode-doesnt-answer).

## The ring flashes red three times

The request didn't work. The log under **System > Logs** on the dashboard says
why:

| Log line | Cause |
| --- | --- |
| `no Home Assistant voice subscription; cannot start a run` | Home Assistant isn't connected, or has no Assist pipeline for the device |
| `Home Assistant refused the run` | Home Assistant has no Assist pipeline for the device, or the pipeline failed to start |
| `Home Assistant did not answer the run start` | Home Assistant didn't start the request within 5 seconds |
| `Assist error: …` | The pipeline reported an error, for example from speech-to-text |
| `Assist run timed out` | Home Assistant didn't answer within 30 seconds of thinking, including the wait for the reply to start. After 15 seconds of listening, the device stops the microphone and waits for the answer |
| `standalone request failed: …` | In standalone mode, a service failed. The line names the part that failed (`stt`, `brain` or `tts`), the service and why, for example `brain via openrouter: HTTP 402`. See [Standalone mode doesn't answer](#standalone-mode-doesnt-answer) |
| `no standalone request: the assistant isn't set up` | In standalone mode, speech-to-text, the brain or text-to-speech hasn't passed its check |

The ring also flashes red when you press the Action button while the
microphones are off. In standalone mode, it also flashes red when a request
heard no speech within 5 seconds of the wake word, so nothing was uploaded, or
when speech-to-text heard only silence.

## Standalone mode doesn't answer

Open the dashboard's **Assistant** page.

- **You can't select Standalone.** The mode switch stays unavailable until
  speech-to-text, the brain and text-to-speech are each set up and have
  passed their check. **Services** shows which one isn't ready, and why its
  check failed.
- **A request fails.** The satellite plays the error sound and the ring flashes
  red. **Last failure** shows why, with the service and the time, until a
  request is answered again. The log has the same failure in a
  `standalone request failed` line, followed by what the service said, such
  as `; the service said: "'whisper-1' is not a valid model_id."`. A check
  that fails is logged the same way, in a line that starts with `the stt
  check failed`, `the brain check failed` or `the tts check failed`. The log
  masks anything that looks like a key. These lines, the tool lines and the
  line for each request are information, not warnings: under **System** ›
  **Logs**, select **All** to see them.
- **The brain says it did something, but nothing happened.** Each tool the
  brain calls leaves a line in the log, such as `request 12: tool set_timer:
  ok, 2 ms`. If there is no such line, the model only said it did it. Choose a
  model that supports tool calls, or try **Thinking** under **Services** ›
  **Brain** › **Advanced**, then select **Save**. A line that ends in `error`
  means the tool refused; the brain got the reason and usually says it. For
  the weather, set **Place** under **Conversation**, or ask for a place by
  name.
- **The brain's check passes, but every request fails.** The check only lists
  the models; a request also offers the brain the satellite's tools. A model
  that doesn't take tools refuses each request: **Last failure** says `The
  service refused the request (400). …` or `The service found nothing for this
  request (404), …`, and the log line says what the service said, such as
  OpenRouter's `No endpoints found that support tool use`. Choose a model that
  supports tool calls. A reasoning model may need **Thinking** set instead.
- **Replies are slow.** Each request ends with one line in the log, such as
  `request 12: speech-to-text 0.6 s (2.5 s of audio), brain 1.4 s to its first
  words, text-to-speech 0.4 s for the first sentence, 260 tokens in, 21 out,
  62 characters spoken; answered`. The `pinna_standalone_stage_seconds`
  [metric](/docs/metrics/) shows which part takes the time: speech-to-text,
  the brain's first text, or text-to-speech. A smaller or nearer model, or a
  faster speech service, helps. For an agent, the acknowledgment says it heard
  you while it works.

A check or a request that fails gives one of these sentences:

| Sentence | What to do |
| --- | --- |
| `The key is wrong. Enter it again.` | Enter the key on the connection again. A key belongs to the address it was entered for: after you change the address, enter the key again too |
| `No credit is left on this account.` | Add credit to your account with the service, or choose another connection |
| `Nothing answered at this address. An OpenAI-compatible server's address usually ends in /v1.` | Add `/v1` to the end of your server's address |
| `The clock isn't set yet. Connect the satellite to the network, then try again.` | The satellite sets its clock from a time server on the internet after it starts. Check that its network reaches the internet, wait a minute, and try again |
| `The secure connection failed. Check the address and the server's certificate.` | Pinna trusts only the certificate authorities in its image. For a server on your network with a certificate of its own, use an `http://` address for now |
| `The service can't be reached. Check the address and that the server is running.` | Check the address, that the server runs, and that it listens on the network, not only on its own computer (for Ollama, `OLLAMA_HOST=0.0.0.0`) |
| `The service didn't answer in time. Check the address and that the server is running.` | The server took more than 10 seconds for the check, or longer than the time limit for a request. Check that it runs and isn't busy |
| `The model … isn't offered by this service.` | Enter the model exactly as the service lists it, for Ollama as `ollama list` shows it |
| `The address is wrong. Check the connection's address.` | The service answered that nothing is there. Check the address on the connection |
| `The address is wrong. OpenClaw answers chat requests only after its chat endpoint is turned on; …` | Turn on OpenClaw's chat endpoint, and check the address and port |
| `The service moved to another address. Enter its new address.` | The satellite doesn't follow a redirect, so a key never goes to another host. Enter the address the service gives now |
| `The service refused the request (400). Check the model's name.` | The service didn't take a name it was sent. In a check, the sentence goes on with what the service said, such as which models it has. For a brain, the sentence goes on: `A reasoning model may need Thinking set, such as None, …`. Check that the model supports tool calls; for a reasoning model, choose **Thinking** under **Services** › **Brain** › **Advanced** and select **Save**. OpenAI's `gpt-6-luna` uses tools only with **None** |
| `The service found nothing for this request (404), though the check passed. …` | The brain's service took the model in its check but has nothing that answers this request, usually because the model doesn't take tool calls. The log says what the service said. Choose a model that supports tool calls |
| `ElevenLabs has no voice with this ID. …` | Enter the voice's ID, not its name. ElevenLabs shows it in your voice library, next to each voice |
| `The service answered …. Check the key, the address and the account.` | The service answered with a status that has no sentence of its own. The log line `the … check failed` or `standalone request failed` says what the service said |
| `The service had an error (500). Try again later, or check the server's log.` | The service failed on its side. For your own server, its log says why |
| `The brain didn't answer in time. …` | The brain took longer than its time limit. If the server runs, the model may be slow: raise **Time to answer** under **Services** › **Brain** › **Advanced** |
| `The service didn't answer the way this check expects. Check the address and the model.` | The address points at something that isn't the service, or the model is wrong |
| `The text-to-speech service sent no audio.` | The service answered the check without sound. Check the voice and the model |
| `The service sent too much data to check. Check the address.` | The address points at something other than the service's API, which answered with far more than a model list |
| `The check couldn't run. Try again.` | Something on the satellite went wrong during the check. Try again; if it stays, the log says more |

For the setup, see [Use Pinna without Home Assistant](/docs/standalone/).

## The device opens fastboot on its own

Pinna's health check failed, so Pinna restarted the device into fastboot: its
services didn't answer in time. See
[The fastboot guard](/docs/recovery/#the-fastboot-guard).

1. Start Pinna again, on your computer:

   ```sh
   fastboot reboot
   ```

2. If the dashboard opens, look at **System > Logs** for errors, and at how
   the last start ended under **System > Troubleshooting**.
3. If the device opens fastboot again, Pinna's services don't start. Install
   the root filesystem again from fastboot, as described in
   [Recover a device](/docs/recovery/). This erases your settings, which also
   removes a setting that stops Pinna.

A developer can read why `pinnad` didn't start before reinstalling; see
[When pinnad doesn't start](/docs/development/#when-pinnad-doesnt-start).

## The device doesn't boot Pinna

If the ring is green, or your computer sees a USB device named **Pinna
rescue**, Pinna is in rescue mode: it couldn't start from the root
filesystem. Open `http://10.42.0.1`, which says why, and download the
diagnostics. If the ring keeps flashing white instead, USB didn't come up
either, and the number of flashes says why. See
[Rescue mode](/docs/recovery/#rescue-mode).

If the ring stays dark, no USB device appears, or the device restarts over
and over, Pinna doesn't get as far as its services.

If this started after an update, unplug the device and plug it in again, up
to three times: after the third start, the bootloader starts the previous
version. See
[Recover from a failed update](/docs/recovery/#recover-from-a-failed-update).

Otherwise, install Pinna again. This erases the data partition:

1. Unplug the device and open fastboot with **Volume Down**. See
   [Recover a device](/docs/recovery/).
2. Install the root filesystem and the boot image again with
   `scripts/flash-gen2.sh root` and `scripts/flash-gen2.sh boot`.

## Report a problem

If you can't solve it, [open an issue](https://github.com/elsbrock/pinna/issues)
and include:

- What you did, and what happened.
- The version, from the **Software** tile on the dashboard's **Overview**,
  and whether the device is an Echo Dot (2nd gen) or an Echo (2nd gen).
- How the last start ended, from **System > Troubleshooting**.
- The log, from **System > Logs**: select **All**, then **Copy**, and paste it
  into the issue. Check it first: issues are public.

In rescue mode, the dashboard doesn't run. Attach the file from **Download
diagnostics** on `http://10.42.0.1` instead. Read it before you post it: the
file holds the device's serial number and the kernel log.

If you can log in to the satellite, also include the output of
`pinnactl status` and `pinna-bootctl slot-status`; see
[Log in to a device](/docs/development/#log-in-to-a-device).
