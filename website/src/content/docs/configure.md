---
title: Configure Pinna
description: Change the device name, audio, wake word, Home Assistant connection and other settings in pinnad.toml.
---

Most everyday settings, such as the volume, the wake word sensitivity and the
ring brightness, are [entities in Home Assistant](/docs/entities/). This page
covers the settings that aren't: the device name, the audio and LED hardware,
the wake word model, and the network ports. They live in a configuration file
for `pinnad`, the program that runs the voice satellite.

## Where the settings are

`pinnad` reads two files at startup:

| File | Contents |
| --- | --- |
| `/etc/pinna/pinnad.toml` | The defaults that come with the image. Read-only |
| `/var/lib/pinna/pinnad.toml` | Your settings for this device. They override the defaults, setting by setting |

Put your changes into `/var/lib/pinna/pinnad.toml`. Updates replace the image
but keep this file. To give every device that you install the same settings,
put the file into `secrets/device/pinnad.toml` before you build. Pinna copies it
to `/var/lib/pinna/pinnad.toml` on first boot.

Pinna also keeps the values you change in Home Assistant, such as the volume,
in `/var/lib/pinna/settings.toml`. Don't edit that file: Pinna writes it
itself.

## Change a setting

1. On the device, open `/var/lib/pinna/pinnad.toml` in an editor. Create it if
   it doesn't exist.
2. Add the setting under its section. For example, to name the device:

   ```toml
   [device]
   name = "kitchen-dot"
   friendly_name = "Kitchen"
   ```

3. Restart Pinna:

   ```sh
   systemctl restart pinnad
   ```

4. Check that Pinna started:

   ```sh
   systemctl status pinnad
   ```

   If the file contains a setting that Pinna doesn't know, or a value of the
   wrong type, Pinna doesn't start. The log names the problem:
   `journalctl -u pinnad`.

## Settings

### `[device]`

How the device introduces itself to Home Assistant.

| Setting | Default | Description |
| --- | --- | --- |
| `name` | `""` | The device name on the network and in Home Assistant. Empty means `pinna-` followed by the last six characters of the Wi-Fi MAC address |
| `friendly_name` | `"Echo Dot"` | The name that Home Assistant shows |
| `model` | `"Echo Dot (2nd Gen)"` | The model that Home Assistant shows |
| `manufacturer` | `"Pinna"` | The manufacturer that Home Assistant shows |
| `suggested_area` | Not set | The area that Home Assistant suggests when you add the device |

### `[audio]`

| Setting | Default | Description |
| --- | --- | --- |
| `backend` | `"auto"` | How Pinna reaches the microphones and the speaker. Leave it at `"auto"` on the Echo Dot. See [Audio](/docs/audio/) |
| `volume` | `50` | The volume on the first boot, from 0 to 100. After that, Pinna keeps the last volume you set |
| `speaker_profile` | `"Flat"` | The speaker tuning of the audio codec. `"Flat"` is the tuning for the Echo Dot (2nd gen); any other value selects the tuning for the Echo (2nd gen) |
| `wake_sound` | `false` | Plays a short sound when the device starts listening |

### `[wake]`

| Setting | Default | Description |
| --- | --- | --- |
| `enabled` | `true` | Whether Pinna listens for the wake word |
| `models` | `"/usr/share/pinna/models/openwakeword-0.5.1"` | The folder with the openWakeWord models |
| `keyword` | `"alexa_v0.1.onnx"` | The wake word model file in that folder |
| `id` | `"alexa_v0.1"` | The wake word ID that Home Assistant sees |
| `phrase` | `"Alexa"` | The wake word that Home Assistant shows |
| `threshold` | `0.55` | How sure the model must be, on the first boot. After that, the **Wake word sensitivity** entity in Home Assistant sets it |

### `[esphome]`

The connection to Home Assistant. See
[Connect to Home Assistant](/docs/home-assistant/).

| Setting | Default | Description |
| --- | --- | --- |
| `port` | `6053` | The port Home Assistant connects to |
| `mdns` | `true` | Whether Pinna announces itself on the network, so Home Assistant finds it |
| `encryption_key` | `""` | The encryption key, 32 bytes encoded as base64. Empty means no encryption. A valid key in `key_file` takes precedence |
| `key_file` | `"/var/lib/pinna/esphome-key"` | The file that holds the encryption key. Home Assistant writes new keys here |

### `[leds]`

| Setting | Default | Description |
| --- | --- | --- |
| `backend` | `"auto"` | How Pinna drives the ring: `"auto"`, `"is31"` (the ring's LED driver), `"mock"` (prints frames, for testing) or `"none"` |
| `brightness` | `255` | The highest brightness the LED driver may use. Pinna treats values above 100 as 100, which is full brightness. To dim the ring, use the **LED brightness** entity instead |

### `[api]`

| Setting | Default | Description |
| --- | --- | --- |
| `listen` | `"127.0.0.1:7470"` | The address of the control API that `pinnactl` uses. The API has no authentication, so keep it on `127.0.0.1` |

### `[web]`

| Setting | Default | Description |
| --- | --- | --- |
| `listen` | `"[::]:80"` | The address of the [dashboard](/docs/dashboard/) and of the same control API, for your network. It has no authentication. `""` turns it off |
| `root` | `"/usr/share/pinna/web"` | The dashboard's files |
| `allowed_hosts` | `[]` | Names to open the dashboard by, besides IP addresses, `localhost`, `.local` names and the hostname. See [Open the dashboard by another name](/docs/dashboard/#open-the-dashboard-by-another-name) |

## Settings that Home Assistant controls

These values come from Home Assistant or the [dashboard](/docs/dashboard/)
after the first boot, so changing them in `pinnad.toml` has no effect once the
device has run:

- The volume (`[audio] volume`)
- The wake word sensitivity (`[wake] threshold`)

To start over with the values from `pinnad.toml`, delete
`/var/lib/pinna/settings.toml` and restart Pinna. This also resets the other
values you set in Home Assistant.

## What's next

- [Connect to Wi-Fi](/docs/flash/#connect-to-wi-fi)
- [Troubleshooting](/docs/troubleshooting/)
