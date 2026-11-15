---
title: Troubleshooting
description: Find out why a Pinna device doesn't connect, doesn't hear the wake word or doesn't start, and what to do about it.
---

Start with the device's own view of the problem. Log in over SSH or the USB
serial console, and run:

```sh
pinnactl status
systemctl --failed
journalctl -b -u pinnad
```

`pinnad` is the program that runs the voice satellite. Its log records what
it does and any errors. The sections below quote the lines to look for.

## You can't log in

- **Over USB:** check that the computer sees the USB device **Pinna (Echo Dot
  2nd gen)**. Use a cable that carries data. The device is at `10.42.0.1`, and
  your computer gets an address between `10.42.0.10` and `10.42.0.29`.
- **Over SSH:** Pinna only accepts SSH keys, no passwords. The key comes from
  `secrets/authorized_keys` at build time. Without it, use the serial console:
  `picocom -b 115200 /dev/ttyACM0`.
- **At `pinna.local`:** your computer needs mDNS. If the name doesn't resolve,
  use the IP address.
- **After a reinstall:** the device has a new SSH host key, and `ssh` refuses
  to connect. Remove the old key from your computer:

  ```sh
  ssh-keygen -R 10.42.0.1
  ssh-keygen -R pinna.local
  ```

## The device has no Wi-Fi

1. Check the Wi-Fi hardware and the connection:

   ```sh
   journalctl -b -u pinna-wifi -u wpa_supplicant@wlan0
   ip addr show wlan0
   ```

2. If `wlan0` doesn't exist, the Wi-Fi firmware is probably missing. The build
   warns about it. Add the firmware to `blobs/firmware/`, as described in
   [What you need](/docs/requirements/#files-from-your-device), and build the
   image again. Then install the new root filesystem over the USB cable with
   [Update Pinna](/docs/update/), which keeps your settings.
3. If `wlan0` exists but has no address, check the network name and password
   in `/var/lib/pinna/wifi/wpa_supplicant-wlan0.conf`. See
   [Connect to Wi-Fi](/docs/flash/#connect-to-wi-fi).

## Home Assistant doesn't find the device

1. Check that Pinna announces itself. The log contains
   `published _esphomelib._tcp as …`:

   ```sh
   journalctl -b -u pinnad | grep _esphomelib
   ```

2. Home Assistant finds devices by mDNS, which doesn't cross network
   boundaries. If Home Assistant runs in another network, VLAN or container
   network, add the device by IP address: **Add integration > ESPHome**, port
   `6053`.
3. If Home Assistant connects and then fails, check the encryption key. The
   log says `ESPHome API encryption: Noise` when the device expects a key, and
   `none (plaintext)` when it doesn't. The key in Home Assistant must match
   `/var/lib/pinna/esphome-key`.

## The device doesn't react to the wake word

Check each condition with `pinnactl status`:

| Line | Must show | If not |
| --- | --- | --- |
| `muted` | `no` | Press the **Microphone off** button, or run `pinnactl mute off` |
| `home assistant` | `connected, voice subscribed` | Add the device in Home Assistant and choose an Assist pipeline for it. See [Connect to Home Assistant](/docs/home-assistant/) |
| `wake word` | `armed` | Turn on the **Wake word** switch in Home Assistant. The wake word is also off while a request runs or a timer rings |
| `audio` | Anything but `none` | `none` means Pinna found no audio device. Check for `microphone unavailable` in the log |

If all lines look right:

- Check that the wake word model loaded. The log line
  `wake word disabled: …` means the model files are missing. Run
  `just models`, build the image again, and install it with
  [Update Pinna](/docs/update/).
- Watch the log while you say the wake word:

  ```sh
  journalctl -f -u pinnad
  ```

  Each detection logs `wake word detected (score …)`. If nothing appears,
  raise **Wake word sensitivity** in Home Assistant. If the device triggers
  too often, lower it.
- Press the **Action** button. If a request starts, the microphones and Home
  Assistant work, and only the wake word detection needs attention.

## The ring flashes red three times

The request didn't work. The log says why:

| Log line | Cause |
| --- | --- |
| `Home Assistant refused the run` | Home Assistant has no Assist pipeline for the device, or the pipeline failed to start |
| `Assist error: …` | The pipeline reported an error, for example from speech-to-text |
| `Assist run timed out` | Home Assistant didn't answer in time: 5 seconds to start, 15 seconds of listening, 30 seconds of thinking |

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

If the ring stays dark, no USB device appears, or the device restarts over
and over, Pinna doesn't get as far as its services:

1. Unplug the device and open fastboot with **Volume Down**. See
   [Recover a device](/docs/recovery/).
2. Install the root filesystem again, and try the boot image from memory with
   `scripts/flash-dot.sh try`.

Pinna's messages from early in the boot only go to the board's internal
serial port, at 921600 baud, not to USB.

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
