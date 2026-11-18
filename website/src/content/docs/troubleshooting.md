---
title: Troubleshooting
description: Find out why a Pinna device doesn't connect, doesn't hear the wake word or doesn't start, and what to do about it.
---

Start with the device's own view of the problem. The
[dashboard](/docs/dashboard/) shows the state under **Overview** and the log of
`pinnad` under **System > Logs**, without a login. If the dashboard doesn't
open, log in over SSH or the USB serial console, and run:

```sh
pinnactl status
systemctl --failed
journalctl -b -u pinnad
```

`pinnad` is the program that runs the voice satellite. Its log records what
it does and any errors. The sections below quote the lines to look for.

`pinnactl status` also says how the last start ended — `restarted (update)`,
`crashed: a kernel panic or oops`, `reset without shutting down`, or
`rescue mode (no-root)`. The dashboard's **System** page shows the same line
under **Troubleshooting**.

## You can't log in

- **In rescue mode:** if the USB device is named **Pinna rescue** or the ring
  is green, Pinna couldn't start, and SSH doesn't run. See
  [The device doesn't boot Pinna](#the-device-doesnt-boot-pinna).
- **Over USB:** check that the computer sees the USB device **Pinna (Echo Dot
  2nd gen)** — **Pinna (Echo 2nd gen)** on an Echo. Use a cable that carries
  data. The device is at `10.42.0.1`, and your computer gets an address
  between `10.42.0.10` and `10.42.0.29`.
- **Over SSH:** Pinna only accepts SSH keys, no passwords. The key comes from
  `secrets/authorized_keys` at build time. Without it, use the serial console:
  `picocom -b 115200 /dev/ttyACM0`.
- **At its `.local` name**, such as `pinna-2a914e.local`: your computer needs
  mDNS. If the name doesn't resolve, use the IP address.
- **After a reinstall:** the device has a new SSH host key, and `ssh` refuses
  to connect. Remove the old key from your computer:

  ```sh
  ssh-keygen -R 10.42.0.1
  ssh-keygen -R pinna-2a914e.local   # your device's name
  ```

## The device has no Wi-Fi

1. Check the Wi-Fi hardware and the connection:

   ```sh
   journalctl -b -u pinna-wifi -u wpa_supplicant@wlan0
   ip addr show wlan0
   ```

2. If `wlan0` doesn't exist, powering up the Wi-Fi failed. The `pinna-wifi`
   lines in the log show the step that failed, for example
   `wlan0 did not appear`. The Wi-Fi chip allows one power-up attempt per
   start, so restart the device to try again.

3. If `wlan0` exists but has no address, connect the device over USB, open
   `http://10.42.0.1/` and join your network again on the **Network** page.
   See [Change the Wi-Fi network](/docs/dashboard/#change-the-wi-fi-network).

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

Check each condition with `pinnactl status`:

| Line | Must show | If not |
| --- | --- | --- |
| `muted` | `no` | Press the **Microphone off** button, or run `pinnactl mute off` |
| `home assistant` | `connected, voice subscribed` | Add the device in Home Assistant and choose an Assist pipeline for it. See [Connect to Home Assistant](/docs/home-assistant/). In standalone mode, this line doesn't matter: see [Standalone mode doesn't answer](#standalone-mode-doesnt-answer) |
| `wake word` | `armed` | Turn on the **Wake word** switch in Home Assistant or on the dashboard's **Voice & sound** page. The wake word is also off while the device listens to a request. In standalone mode it is armed only once speech-to-text, the brain and text-to-speech have passed their check: the `mode` line says `services ready`, and so does the dashboard's **Overview** |
| `audio` | Anything but `none` | `none` means Pinna found no audio device. Check for `microphone unavailable` in the log |

If all lines look right:

- Check that the wake word model loaded. The log line
  `wake word disabled: …` means the model files are missing. Run
  `just models`, build the image again, and install it with
  [Update Pinna](/docs/update/).
- Test the wake word on the dashboard's **Wake word** page:

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

The log of `pinnad` has a line for each reply. Find it under **System > Logs**
in the dashboard, or with `journalctl -b -u pinnad | grep reply:`:

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

## The ring flashes red three times

The request didn't work. The log says why:

| Log line | Cause |
| --- | --- |
| `no Home Assistant voice subscription; cannot start a run` | Home Assistant isn't connected, or has no Assist pipeline for the device |
| `Home Assistant refused the run` | Home Assistant has no Assist pipeline for the device, or the pipeline failed to start |
| `Home Assistant did not answer the run start` | Home Assistant didn't start the request within 5 seconds |
| `Assist error: …` | The pipeline reported an error, for example from speech-to-text |
| `Assist run timed out` | Home Assistant didn't answer within 30 seconds of thinking, including the wait for the reply to start. After 15 seconds of listening, the device stops the microphone and waits for the answer |
| `standalone request failed: …` | In standalone mode, a service failed. The line names the part that failed (`stt`, `brain`, `tts` or `tool`), the service and why, for example `brain via openrouter: HTTP 402`. See [Standalone mode doesn't answer](#standalone-mode-doesnt-answer) |
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
- **A request fails.** The device plays the error sound and the ring flashes
  red. **Last failure** shows why, with the service and the time, until a
  request is answered again. The log has the same failure in a
  `standalone request failed` line.
- **The brain's check passes, but every request fails with `The service
  answered 400`.** A local model that doesn't support tool calls refuses each
  request, because the satellite offers it its tools. Choose a model that
  supports them.
- **Replies are slow.** The log line `reply: first audio … after the end of
  speech` gives the delay. The `pinna_standalone_stage_seconds`
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
| `The clock isn't set yet. Connect the satellite to the network, then try again.` | The device sets its clock from a time server on the internet after it starts. Check that its network reaches the internet, wait a minute, and try again |
| `The secure connection failed. Check the address and the server's certificate.` | Pinna trusts only the certificate authorities in its image. For a server on your network with a certificate of its own, use an `http://` address for now |
| `The service can't be reached. Check the address and that the server is running.` | Check the address, that the server runs, and that it listens on the network, not only on its own computer (for Ollama, `OLLAMA_HOST=0.0.0.0`) |
| `The service didn't answer in time. Check the address and that the server is running.` | The server took more than 10 seconds for the check, or longer than the time limit for a request. Check that it runs and isn't busy |
| `The model … isn't offered by this service.` | Enter the model exactly as the service lists it, for Ollama as `ollama list` shows it |
| `The address is wrong. Check the connection's address.` | The service answered that nothing is there. Check the address on the connection |
| `The address is wrong. OpenClaw answers chat requests only after its chat endpoint is turned on; …` | Turn on OpenClaw's chat endpoint, and check the address and port |
| `The service moved to another address. Enter its new address.` | The satellite doesn't follow a redirect, so a key never goes to another host. Enter the address the service gives now |
| `The service answered 400. Check the key, the address and the account.` | The service refused the request. Check the model name, and for a local model that it supports tool calls |
| `The service had an error (500). Try again later, or check the server's log.` | The service failed on its side. For your own server, its log says why |
| `The service refused the request (400). …` | The brain refused the request. Check the model name. A reasoning model may need **Thinking** set, such as **None**, under **Services** › **Brain** › **Advanced**; OpenAI's `gpt-6-luna` uses tools only then |
| `The brain didn't answer in time. …` | The brain took longer than its time limit. If the server runs, the model may be slow: raise **Time to answer** under **Services** › **Brain** › **Advanced** |
| `The service didn't answer the way this check expects. Check the address and the model.` | The address points at something that isn't the service, or the model is wrong |

For the setup, see [Use Pinna without Home Assistant](/docs/standalone/).

## The device opens fastboot on its own

Pinna's health check failed, so Pinna restarted the device into fastboot. See
[The fastboot guard](/docs/recovery/#the-fastboot-guard). Common causes are an
invalid `pinnad.toml` or encryption key file, which stop `pinnad`.

1. Boot Pinna again:

   ```sh
   fastboot reboot
   ```

2. Log in over the USB serial console right away: the health check restarts
   the device about 40 seconds after Pinna's services start. The serial
   console doesn't need an SSH key.
3. Read the log of the previous boot, and fix the cause:

   ```sh
   journalctl -b -1 -u pinnad
   ```

If you can't fix it in time, install the root filesystem again from fastboot,
as described in [Recover a device](/docs/recovery/). This erases your
settings, which also removes a broken configuration.

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

## Pinna doesn't start after a configuration change

`pinnad` refuses to start if `pinnad.toml` contains a setting it doesn't know,
a value of the wrong type, or an invalid encryption key file. The log names
the problem:

```sh
journalctl -b -u pinnad
```

Fix the file and run `systemctl restart pinnad`. See
[Configure Pinna](/docs/configure/). If the device restarts into fastboot
before you can fix it, see
[The device opens fastboot on its own](#the-device-opens-fastboot-on-its-own).

## Report a problem

If you can't solve it, [open an issue](https://github.com/elsbrock/pinna/issues)
and include:

- What you did, and what happened.
- The output of `pinnactl status` and `pinna-bootctl slot-status`.
- The log: `journalctl -b -u pinnad`.
- The image version, from `/etc/os-release` (`IMAGE_VERSION`).

In rescue mode, none of these commands run. Attach the file from
**Download diagnostics** on `http://10.42.0.1` instead. Read it before you
post it: issues are public, and the file holds the device's serial number
and the kernel log.
