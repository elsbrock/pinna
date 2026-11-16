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
   dashboard's **Home Assistant** page: **Add integration > ESPHome**.
3. If Home Assistant connects and then fails, its key and the device's don't
   match, for example after a factory reset or a reinstall. The dashboard's
   **Home Assistant** page shows whether the device has a key. Remove the
   device from Home Assistant and add it again; if the device has a key that
   Home Assistant doesn't know,
   [reset the device](/docs/dashboard/#reset-the-device) first.

## The device doesn't react to the wake word

Check each condition with `pinnactl status`:

| Line | Must show | If not |
| --- | --- | --- |
| `muted` | `no` | Press the **Microphone off** button, or run `pinnactl mute off` |
| `home assistant` | `connected, voice subscribed` | Add the device in Home Assistant and choose an Assist pipeline for it. See [Connect to Home Assistant](/docs/home-assistant/) |
| `wake word` | `armed` | Turn on the **Wake word** switch in Home Assistant or on the dashboard's **Voice & sound** page. The wake word is also off while a request runs |
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

  The log shows the scores too, in the lines `wake word detected (score …)`
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
| `Home Assistant refused the run` | Home Assistant has no Assist pipeline for the device, or the pipeline failed to start |
| `Assist error: …` | The pipeline reported an error, for example from speech-to-text |
| `Assist run timed out` | Home Assistant didn't answer in time: 5 seconds to start, 15 seconds of listening, 30 seconds of thinking, including the wait for the reply to start |

The ring also flashes red when you press the Action button while the
microphones are off.

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
and over, Pinna doesn't get as far as its services:

1. Unplug the device and open fastboot with **Volume Down**. See
   [Recover a device](/docs/recovery/).
2. Install the root filesystem and the boot image again with
   `scripts/flash-gen2.sh root` and `scripts/flash-gen2.sh boot`.

After an update, the device goes back to the previous version on its own after
three failed attempts. See
[Recover from a failed update](/docs/recovery/#recover-from-a-failed-update).

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
