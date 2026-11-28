---
title: Developer settings
description: The settings in pinnad.toml for developers, such as the audio and LED backends, the ports, an update address and the first-boot defaults. Owners set up a satellite on the dashboard.
---

You don't need this page to use Pinna. Owners set up and change everything on
the [dashboard](/docs/dashboard/): the name, Wi-Fi, the wake words, the volume,
the sensitivity, the ring, the hostname, the time zone, the dashboard address,
the mode and the services of [standalone mode](/docs/standalone/), and the
update channel. Most of them are [entities in Home Assistant](/docs/entities/)
too.

This page is for developers. It covers the configuration file for `pinnad`,
the program that runs the voice satellite: the audio and LED hardware, the
ports, an update address of your own, and the defaults that apply until
someone sets a value on the dashboard or in Home Assistant. The model and the
speaker tuning follow the hardware, so they need no setting. Changing a
setting needs a login on the device; see
[Log in to a device](/docs/development/#log-in-to-a-device).

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

Standalone mode keeps its setup in files beside `settings.toml`:

| File | Contents |
| --- | --- |
| `/var/lib/pinna/standalone.toml` | The connections to your services, the speech-to-text, brain and text-to-speech slots, the conversation settings, the tool switches, the radio stations, the MCP servers, and the satellite group: its switches, its id and its members |
| `/var/lib/pinna/secrets.toml` | The keys, each with the address it was entered for, the MCP server addresses, and the satellite group's key, with the id of its group. Only its owner can read it (mode 0600) |
| `/var/lib/pinna/timers.toml` | The timers set in standalone mode, so that they survive a restart |

You set these on the dashboard's **Assistant** page; the files are for
developers. Pinna writes them itself, like `settings.toml`, and reads them
entry by entry: an entry it can't read, such as one that a newer version
wrote, is kept and written back, and doesn't reset the rest. A
[factory reset](/docs/settings/#reset-the-device) deletes them with the other
settings.

## Change a setting

1. The device has no text editor, so edit the file on your computer. Copy it
   from the device, or start an empty one if the device has none yet. On your
   computer:

   ```sh
   scp root@10.42.0.1:/var/lib/pinna/pinnad.toml . || touch pinnad.toml
   ```

   Over Wi-Fi, use the device's `.local` name instead of `10.42.0.1`.
2. Add the setting under its section. For example, to use a fixed microphone
   gain of +18 dB instead of the automatic gain:

   ```toml
   [audio]
   capture_gain_db = 18.0
   ```

3. Copy the file back to the device. On your computer:

   ```sh
   scp pinnad.toml root@10.42.0.1:/var/lib/pinna/pinnad.toml
   ```

4. Restart Pinna, on the device:

   ```sh
   systemctl restart pinnad
   ```

5. Check that Pinna started:

   ```sh
   systemctl status pinnad
   ```

   If the file contains a setting that Pinna doesn't know, or a value of the
   wrong type, Pinna doesn't start. The log names the problem:
   `journalctl -u pinnad`. Fix the file before the device next restarts:
   then the health check fails, and the device opens fastboot. If it does,
   see [When pinnad doesn't start](/docs/development/#when-pinnad-doesnt-start).

## Settings

### `[device]`

How the device introduces itself to Home Assistant.

| Setting | Default | Description |
| --- | --- | --- |
| `name` | `""` | The node name: the device's ID in the ESPHome connection and in mDNS discovery, shown as **Node name** under **System** on the dashboard. Home Assistant shows `friendly_name` instead. It is also the device's hostname, so `name = "kitchen-dot"` makes it `kitchen-dot.local`. The hostname that someone sets on the dashboard's **Network** page wins over it, so `name` is the developer default. Empty means `pinna-` followed by the last six characters of the Wi-Fi MAC address |
| `friendly_name` | Not set | The name that Home Assistant shows, until someone [names the device on the dashboard](/docs/settings/#name-the-device), which wins. Not set means the model and the last six characters of the MAC address, for example `Echo Dot 2a914e` |
| `model` | Not set | The model that the device reports. Not set means the hardware's: `Echo Dot (2nd Gen)`, `Echo (2nd Gen)` or `Echo Dot (5th Gen)` |
| `manufacturer` | `"Pinna"` | The manufacturer that Home Assistant shows |
| `suggested_area` | Not set | The area that Home Assistant suggests when you add the device |

### `[audio]`

| Setting | Default | Description |
| --- | --- | --- |
| `backend` | `"auto"` | How Pinna reaches the microphones and the speaker. Leave it at `"auto"`. See [Audio backends](#audio-backends) |
| `volume` | `50` | The volume on the first boot, from 0 to 100. After that, Pinna keeps the last volume you set |
| `speaker_profile` | Not set | The speaker tuning of the audio codec, `"Flat"` or `"Radar"`. Not set means the hardware's: `"Radar"` on the Echo (2nd gen), `"Flat"` otherwise |
| `wake_sound` | `false` | Plays a short sound when the device starts listening, until someone sets **Wake sound** on the dashboard or in Home Assistant, which wins |
| `capture_gain_db` | Not set | A fixed gain on the microphone audio before wake word detection and before it goes to Home Assistant or to a standalone request, in dB. It turns off the automatic gain. Not set means automatic gain, which starts at +12 dB on an Echo with PipeWire and at 0 dB otherwise. See [Audio](/docs/audio/) |
| `stream_socket` | `""` in the code, `"/run/pinna/stream.sock"` in the image | A Unix socket that takes live PCM audio into the media lane. See [The stream socket](#the-stream-socket). Empty turns it off |
| `capture_target` | Not set | The PipeWire node to record from, with the `pipewire` backend. Not set means the default source. The virtual Echo Dot's image sets it to `"dev_mic"` |

#### Audio backends

`backend` chooses how `pinnad` reaches the audio hardware. `"auto"` picks the
first that fits; [Audio](/docs/audio/) explains the audio path.

| Backend | Used when |
| --- | --- |
| `echodot-pipewire` | On every supported Echo, when PipeWire runs. The normal case |
| `echodot` | The same, without PipeWire. Microphones and speaker through ALSA |
| `pipewire` | On other computers, through PipeWire, when `$XDG_RUNTIME_DIR/pipewire-0` exists. The virtual Echo Dot sets it in its own configuration |
| `wav:<path>` | Reads the microphone signal from a WAV file. The speaker plays to nothing. For tests |
| `none` | No audio. The wake word and voice requests are off |

#### The stream socket

[AirPlay](/docs/airplay/) and [Bluetooth](/docs/bluetooth/) play through the
stream socket, and other programs on the device can too. A program that writes
to the socket first sends one line with the format, `s16le 44100 2` or
`s16le 48000 2`, then stereo samples.

The line can end with a name for the program: 1 to 16 characters, each
`a-z`, `0-9` or `-`, for example `s16le 44100 2 my-player`. AirPlay and
Bluetooth use `airplay` and `bluetooth`. To show a track on **Now playing**,
the program passes the same name as `source` to `SetStreamMetadata` in the
[control API](/docs/control-api/). The track shows only while that program's
audio plays. Audio sent without a name shows no track.

### `[wake]`

The wake words that Pinna listens for. It listens for two at once, at most.
The ready-made words are Alexa, Hey Jarvis, Hey Mycroft, Hey Rhasspy and Okay
Nabu; each is offered when its model file is in the `models` folder.

`keyword`, `id` and `phrase` name the word that listens on the first boot. After
that, the choice is made in Home Assistant, or on the
[dashboard](/docs/dashboard/) while Home Assistant isn't connected or the
device is in standalone mode, and saved in `/var/lib/pinna/settings.toml`. A `[wake]` word whose `id` isn't one of the
ready-made ones is offered too, if you put its model into the `models` folder
yourself.

| Setting | Default | Description |
| --- | --- | --- |
| `enabled` | `true` | Whether Pinna listens for the wake word |
| `models` | `"/usr/share/pinna/models/openwakeword-0.5.1"` | The folder with the wake word models |
| `speaker_model` | `"/usr/share/pinna/models/speaker/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced-f16.onnx"` | The model that tells people apart by voice. Pinna loads it only once someone is enrolled |
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
| `encryption_key` | `""` | The encryption key, 32 bytes encoded as base64. Empty means none until Home Assistant sets one ([Encryption](/docs/home-assistant/#encryption)). A valid key in `key_file` takes precedence. To build a key into the image, see [A fixed encryption key](#a-fixed-encryption-key) |
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
| `listen` | `""` in the code, `"[::]:80"` in the image | The address of the [dashboard](/docs/dashboard/), of the same control API and of the [metrics](/docs/metrics/), for your network. With a [dashboard password](/docs/dashboard/#who-can-use-the-dashboard) set, it needs a session, `/mcp` needs the agent token and `/metrics` the metrics token. Without a password it has no authentication. `""` turns it off; the dashboard has no switch for this |
| `root` | `"/usr/share/pinna/web"` | The dashboard's files |
| `allowed_hosts` | `[]` | More names to open the dashboard by, besides IP addresses, `localhost`, `.local` names, the hostname and the host of the dashboard address. A setting for developers: to open the dashboard by a name, [set a dashboard address](/docs/settings/#set-a-dashboard-address) on the **Network** page |

### `[update]`

| Setting | Default | Description |
| --- | --- | --- |
| `manifest` | Not set | A manifest address that the satellite checks instead of its update channel, such as `"http://192.168.1.20:8000/manifest.json"` for the output of `just release`. It offers the release there when its version is newer than the running one. The dashboard and Home Assistant show the address in place of the **Updates** setting. See [Offer your own builds as updates](/docs/releases/#offer-your-own-builds-as-updates) |

### `[sendspin]`

Where the Sendspin player listens while its switch is on. For developers: a test
runs a second `pinnad` on a computer beside one that already holds the port.

| Setting | Default | Description |
| --- | --- | --- |
| `port` | `8928` | The TCP port the player listens on, on every address. The record names it, so Music Assistant finds the player on it |
| `record` | `"/run/systemd/dnssd/pinna-sendspin.dnssd"` | The file that announces the player through systemd-resolved. Anywhere else, Pinna writes and removes the file but doesn't tell systemd-resolved, so nothing is announced |

### `[dev]`

| Setting | Default | Description |
| --- | --- | --- |
| `virtual_device` | `false` | Turns the device into the virtual Echo Dot of the development VM, with its buttons, microphone switch, microphone and speaker on the dashboard. Only the x86_64 image sets it; Pinna refuses it on real hardware |

## Settings that the dashboard and Home Assistant control

These values come from the [dashboard](/docs/dashboard/) or Home Assistant
once someone sets them there. After that, changing them in `pinnad.toml` has
no effect:

- The name (`[device] friendly_name`)
- The hostname (`[device] name`)
- The wake sound (`[audio] wake_sound`)
- The volume (`[audio] volume`)
- The wake word sensitivity (`[wake] threshold`)
- The wake words (`[wake] keyword`, `id` and `phrase` name only the first
  boot's; after that the choice is Home Assistant's, or the dashboard's while
  Home Assistant isn't connected or the device is in standalone mode)

To start over with the values from `pinnad.toml`, delete
`/var/lib/pinna/settings.toml` and restart Pinna. This also resets the other
values you set in Home Assistant, and switches the device back to Home
Assistant mode. Standalone mode's files stay.

## A fixed encryption key

Home Assistant sets the encryption key of the ESPHome connection itself (see
[Encryption](/docs/home-assistant/#encryption)). To build a key into the image
instead, save 32 random bytes, encoded as base64, to
`secrets/device/esphome-key` before the build, on your computer:

```sh
openssl rand -base64 32 > secrets/device/esphome-key
```

The image copies it to `/var/lib/pinna/esphome-key` on first boot, and Home
Assistant asks for it when you add the device. A
[factory reset](/docs/settings/#reset-the-device) keeps a key that came with
the image.

## What's next

- [Log in to a device](/docs/development/#log-in-to-a-device)
- [pinnactl and the control API](/docs/control-api/)
- [Contribute](/docs/development/)
