---
title: Configure Pinna
description: The developer settings in pinnad.toml, such as the audio and LED backends, the ports and the first-boot defaults. Owners set the name and the rest on the dashboard.
---

You don't need this page to use Pinna. The device's name, the wake words,
the wake sound, the volume, the sensitivity, the ring brightness and the names
to open the dashboard by are on the [dashboard](/docs/dashboard/), and most of
them are [entities in Home Assistant](/docs/entities/) too.

This page is for developers. It covers the configuration file for `pinnad`,
the program that runs the voice satellite: the audio and LED hardware, the
ports, and the defaults that apply until someone sets a value on the dashboard
or in Home Assistant. The model and the speaker tuning follow the hardware, so
they need no setting either.

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

Pinna also keeps the values you change on the dashboard or in Home Assistant,
such as the name and the volume, in `/var/lib/pinna/settings.toml`. Don't edit
that file: Pinna writes it itself.

## Change a setting

1. On the device, open `/var/lib/pinna/pinnad.toml` in an editor. Create it if
   it doesn't exist.
2. Add the setting under its section. For example, to raise the microphone
   gain:

   ```toml
   [audio]
   capture_gain_db = 18.0
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
| `name` | `""` | The node name: the device's ID in the ESPHome connection and in mDNS discovery, shown as **Node name** under **System** on the dashboard. Home Assistant shows `friendly_name` instead. It is also the device's hostname, so `name = "kitchen-dot"` makes it `kitchen-dot.local`. The hostname that someone sets on the dashboard's **Network** page wins over it, so `name` is the developer default. Empty means `pinna-` followed by the last six characters of the Wi-Fi MAC address |
| `friendly_name` | Not set | The name that Home Assistant shows, until someone [names the device on the dashboard](/docs/dashboard/#name-the-device), which wins. Not set means the model and the last six characters of the MAC address, for example `Echo Dot 2a914e` |
| `model` | Not set | The model that the device reports. Not set means the hardware's: `Echo Dot (2nd Gen)` or `Echo (2nd Gen)` |
| `manufacturer` | `"Pinna"` | The manufacturer that Home Assistant shows |
| `suggested_area` | Not set | The area that Home Assistant suggests when you add the device |

### `[audio]`

| Setting | Default | Description |
| --- | --- | --- |
| `backend` | `"auto"` | How Pinna reaches the microphones and the speaker. Leave it at `"auto"`. See [Audio](/docs/audio/) |
| `volume` | `50` | The volume on the first boot, from 0 to 100. After that, Pinna keeps the last volume you set |
| `speaker_profile` | Not set | The speaker tuning of the audio codec, `"Flat"` or `"Radar"`. Not set means the hardware's: `"Radar"` on the Echo (2nd gen), `"Flat"` otherwise |
| `wake_sound` | `false` | Plays a short sound when the device starts listening, until someone sets **Wake sound** on the dashboard or in Home Assistant, which wins |
| `capture_gain_db` | Not set | The gain applied to the microphone audio before wake word detection and before it goes to Home Assistant, in dB. Not set means +12 dB on the Echo Dot with PipeWire, and 0 dB otherwise |
| `stream_socket` | `""` in the code, `"/run/pinna/stream.sock"` in the image | A Unix socket that takes live PCM audio into the media lane: a first line `s16le 44100 2` or `s16le 48000 2`, then stereo samples. Empty turns it off |

### `[wake]`

The wake words that Pinna listens for. It listens for two at once, at most.
The ready-made words are Alexa, Hey Jarvis, Hey Mycroft, Hey Rhasspy and Okay
Nabu; each is offered when its model file is in the `models` folder.

`keyword`, `id` and `phrase` name the word that listens on the first boot. After
that, the choice is made in Home Assistant, or on the
[dashboard](/docs/dashboard/) while Home Assistant isn't connected, and saved in
`/var/lib/pinna/settings.toml`. A `[wake]` word whose `id` isn't one of the
ready-made ones is offered too, if you put its model into the `models` folder
yourself.

| Setting | Default | Description |
| --- | --- | --- |
| `enabled` | `true` | Whether Pinna listens for the wake word |
| `models` | `"/usr/share/pinna/models/openwakeword-0.5.1"` | The folder with the wake word models |
| `keyword` | `"alexa_v0.1.onnx"` | The model file of the first-boot wake word |
| `id` | `"alexa_v0.1"` | The first-boot wake word's ID, as Home Assistant sees it |
| `phrase` | `"Alexa"` | The first-boot wake word as Home Assistant shows it |
| `threshold` | `0.55` | How sure the model must be, on the first boot. After that, the **Wake word sensitivity** entity in Home Assistant sets it |

### `[esphome]`

The connection to Home Assistant. See
[Connect to Home Assistant](/docs/home-assistant/).

| Setting | Default | Description |
| --- | --- | --- |
| `port` | `6053` | The port Home Assistant connects to |
| `mdns` | `true` | Whether Pinna announces itself on the network, so Home Assistant finds it |
| `encryption_key` | `""` | The encryption key, 32 bytes encoded as base64. Empty means none until Home Assistant sets one ([Encryption](/docs/home-assistant/#encryption)). A valid key in `key_file` takes precedence |
| `key_file` | `"/var/lib/pinna/esphome-key"` | The file that holds the encryption key Home Assistant set |

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
| `listen` | `"[::]:80"` | The address of the [dashboard](/docs/dashboard/), of the same control API and of the [metrics](/docs/metrics/), for your network. It has no authentication. `""` turns it off; the dashboard has no switch for this |
| `root` | `"/usr/share/pinna/web"` | The dashboard's files |
| `allowed_hosts` | `[]` | Names to open the dashboard by, besides IP addresses, `localhost`, `.local` names and the hostname. Names added on the dashboard's **Network** page count too. See [Open the dashboard by another name](/docs/dashboard/#open-the-dashboard-by-another-name) |

## Settings that the dashboard and Home Assistant control

These values come from the [dashboard](/docs/dashboard/) or Home Assistant
once someone sets them there. After that, changing them in `pinnad.toml` has
no effect:

- The name (`[device] friendly_name`)
- The wake sound (`[audio] wake_sound`)
- The volume (`[audio] volume`)
- The wake word sensitivity (`[wake] threshold`)
- The wake words (`[wake] keyword`, `id` and `phrase` name only the first
  boot's; after that the choice is Home Assistant's, or the dashboard's while
  Home Assistant isn't connected)

To start over with the values from `pinnad.toml`, delete
`/var/lib/pinna/settings.toml` and restart Pinna. This also resets the other
values you set in Home Assistant.

## What's next

- [Connect to Wi-Fi](/docs/flash/#connect-to-wi-fi)
- [Troubleshooting](/docs/troubleshooting/)
