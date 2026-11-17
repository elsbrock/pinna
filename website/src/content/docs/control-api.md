---
title: pinnactl and the control API
description: Reference for the pinnactl command and the local ConnectRPC API that it uses.
---

`pinnactl` controls a Pinna device from its own command line. It uses the
control API, which `pinnad` serves on the device. Other programs on the device
can use the same API, and so does the [dashboard](/docs/dashboard/).

## pinnactl

Run `pinnactl` on the device, for example over SSH. By default, it connects to
the API at `http://127.0.0.1:7470`. To use another address, add
`--api <url>` before the command.

| Command | What it does | Output |
| --- | --- | --- |
| `pinnactl status` | Shows the device's state | One line per value, see [Status](#status) |
| `pinnactl status --json` | Shows the state as JSON | The `Status` message as JSON |
| `pinnactl volume <0-100>` | Sets the volume | `volume 40%` |
| `pinnactl mute on`, `pinnactl mute off` | Turns the microphones off or on in software | `software mute on` |
| `pinnactl listen` | Starts a request, like the Action button | `listening` |
| `pinnactl stop` | Stops a request, a reply, an announcement or a timer alarm | `stopped` |
| `pinnactl led-test` | Shows the listening, thinking, speaking and muted animations, one second each | `LED test started` |
| `pinnactl watch` | Prints a line each time the state changes, until you press Ctrl+C | See [Watch](#watch) |
| `pinnactl watch --json` | The same, as JSON | One `Status` message per line |
| `pinnactl update <manifest>` | Installs an update. See [Update Pinna](/docs/update/) | The steps of the update |
| `pinnactl update --check <manifest>` | Fetches a manifest and checks its signature, without installing anything. `pinnad` runs it for its update checks | One line of JSON: the manifest and why an install would be refused, or why the check failed |

`pinnactl update` doesn't use the API. It runs on the device as `root` and
writes the update directly. It takes these options:

| Option | Default | Description |
| --- | --- | --- |
| `--reboot` | Off | Restarts the device after the update is written |
| `--tries <number>` | `3` | How many times the device tries to start the new version before it goes back |
| `--channel <stable\|beta>` | None | Installs as from that update channel: only a version newer than the running one, and on `stable` no beta. Without it, an older version installs too, and `pinnactl` says so |
| `--expect-version <version>` | None | Installs only that version: the one a check showed |

Every install checks the manifest's signature (`manifest.json.sig`) first,
and refuses a release for another model, a release that needs a newer boot
image than the one that would start it, and an install while a `just push`
extension is in `/var/lib/extensions/pinna-dev`.

### Status

`pinnactl status` prints the device's state. For example:

```text
device:     pinna-2a914e (00:fc:8b:2a:91:4e)
version:    0.4.1-dev.202611152312.gaeafbda
uptime:     3h 12m 05s
last start: restarted (update)
phase:      idle
muted:      no
volume:     40%
home assistant: connected, voice subscribed
wake word:  "Alexa" (armed)
audio:      echodot-pipewire
leds:       is31
timers:
  Pasta: 4m 32s left of 10m 00s
```

- `version` is the version of `pinnad`, and `uptime` is the time since
  `pinnad` started.
- `last start` says how the start before this one ended, for example
  `restarted (update)`. It is left out when the device can't tell.
- `phase` is `idle`, `listening`, `thinking` or `speaking`.
- `muted` is `no`, `yes (hardware)`, `yes (software)` or
  `yes (hardware, software)`.
- `voice subscribed` means Home Assistant has an Assist pipeline for the
  device.
- The wake word line names the active words, or `none chosen`, and is `armed`
  when the device listens for them right now: the microphones are on, and
  either a timer alarm rings, or the device isn't listening to a request and
  Home Assistant is subscribed.
- A paused timer shows `(paused)`.

### Watch

`pinnactl watch` prints one line per change:

```text
listening volume  40%  muted no  ha yes  voice yes  timers 0
thinking  volume  40%  muted no  ha yes  voice yes  timers 0
speaking  volume  40%  muted no  ha yes  voice yes  timers 0
idle      volume  40%  muted no  ha yes  voice yes  timers 0
```

## Control API

The API is a [ConnectRPC](https://connectrpc.com/) service,
`pinna.v1.DeviceService`, defined in
[`proto/pinna/v1/device.proto`](https://github.com/elsbrock/pinna/blob/main/proto/pinna/v1/device.proto).
You can call it with any Connect, gRPC or gRPC-Web client, or with plain HTTP
and JSON.

The API has no authentication: anyone who can reach it can control the
device. `pinnad` serves it in two places:

- On `127.0.0.1:7470`, which only programs on the device itself can reach.
  `pinnactl` and the boot health check use this address.
- On port 80, with the dashboard, for your network (`[web]` in the
  [configuration](/docs/configure/#web)). This one only answers requests for IP
  addresses, `localhost`, `.local` names, the device's own hostname (its node name, such as `pinna-2a914e`),
  the host of the dashboard address set on the **Network** page, and the names
  in `[web] allowed_hosts`. Other names get `421 Misdirected Request`.

### Methods

| Method | Request | What it does |
| --- | --- | --- |
| `GetStatus` | Empty | Returns the current `Status` |
| `WatchStatus` | Empty | Streams the current `Status`, then a new one after each change |
| `WatchRing` | Empty | Streams the LED ring as the device shows it: `frame`, 3 bytes (red, green, blue) for each of the 12 LEDs in ring order, where LEDs 10 and 11 are either side of the front, with the brightness setting applied. The current frame comes first, then at most 25 a second while the ring changes |
| `SetVolume` | `percent` (0–100) | Sets the volume. Values above 100 return `invalid_argument` |
| `SetStreamVolume` | `percent` (0–100) | Sets the volume of live audio on the stream socket, on the same curve as the volume and under it. Only the device's own programs can call it, on `127.0.0.1`; from the network it returns `permission_denied`. [AirPlay](/docs/airplay/) and [Bluetooth](/docs/bluetooth/) use it for the sender's volume |
| `SetStreamMetadata` | `source`, `title`, `artist`, `album` | Sets the track of one program's live audio on the stream socket. `source` is the name that the program gives in the first line it sends there ([Audio](/docs/audio/)): 1 to 16 characters, each `a-z`, `0-9` or `-`. A name that breaks this rule returns `invalid_argument`. The call works before and after the program connects. The track stays until the program sets another, and empty fields clear it. `media` shows it only while audio with that name plays. Each field is cut to 256 bytes, at a character boundary. Only the device's own programs can call it, on `127.0.0.1`; from the network it returns `permission_denied`. [AirPlay](/docs/airplay/) and [Bluetooth](/docs/bluetooth/) use it for the sender's track |
| `SetMute` | `muted` (bool) | Turns the software mute on or off. The hardware switch isn't affected |
| `StartConversation` | Empty | Starts a request, like the Action button. Returns `failed_precondition` if Home Assistant has no Assist pipeline for the device, the microphones are off, a timer alarm rings, or a request, a reply or an announcement is already running |
| `StopConversation` | Empty | Stops a request, a reply, an announcement or a timer alarm |
| `LedTest` | Empty | Runs the LED test |
| `ClearNotification` | Empty | Clears a waiting notification, even if the ring shows something else: the **Notification** switch turns off and the `cleared` event fires. Does nothing if no notification is waiting |
| `UpdateSettings` | Any of `friendly_name` (the name Home Assistant shows: 1 to 64 characters, no control characters), `wake_word` and `wake_sound` (bool), `wake_sensitivity` and `led_brightness` (0–100), `wake_words` (up to two ids from `Status.wake_word.available`), `airplay` and `bluetooth` (bool), `hostname` (one DNS label, stored lowercase), `time_zone` (one of `ListTimeZones`), `url` (the dashboard address: `http://` or `https://`, a host that other devices can reach and an optional port, nothing after it; stored with a `/` at the end; an empty string clears it), `reply_volume` (0–100), `reply_follows_volume` and `listening_direction` (bool), and `update_channel` (`UPDATE_CHANNEL_STABLE`, `_BETA` or `_OFF`) | Changes those settings through the same paths as Home Assistant's entities; `friendly_name`, `hostname`, `time_zone` and `url` have no entity. Home Assistant reads a new `friendly_name` or `hostname` when it next connects: the device makes it reconnect once no request, reply, announcement, music or timer alarm plays. `reply_follows_volume` set to true makes replies follow the volume again; false keeps the current level. `reply_follows_volume` true together with `reply_volume` returns `invalid_argument`. `airplay` returns `failed_precondition` on a device without a working speaker, `bluetooth` also on one without Bluetooth. `update_channel` starts a check of the new channel, and returns `failed_precondition` while `[update] manifest` in `pinnad.toml` replaces the channel. Values above 100, an empty or too long name, an invalid hostname or dashboard address, a time zone the device doesn't have, unknown or repeated wake word ids and more than two return `invalid_argument`. `wake_words` returns `failed_precondition` while Home Assistant is connected: its dropdowns own the choice then |
| `ListTimeZones` | Empty | Returns the IANA time zones the device has, from tzdata's `zone1970.tab`, plus `UTC`, that `UpdateSettings` accepts as `time_zone` |
| `ListSatellites` | Empty | Returns the other Pinna devices that the device's background search last found, at once. The device searches once a minute with a one-shot mDNS query over IPv4 and IPv6 and follow-ups for missing records, and lists a device once its dashboard accepts a connection. A device stays listed until it misses two searches in a row: it isn't found, its dashboard doesn't answer, or the search fails. Returns `satellites`, one per MAC address, each with its ESPHome instance name, the host its SRV record names (its `.local` name), friendly name, model, MAC, version, dashboard port, dashboard address and addresses (without the USB link's `10.42.0.0/24` and without link-local IPv6 addresses); `on_network`, which is false, with an empty list, while the device has no address beyond USB; and `searched`, which is false until the first search after the device starts has finished. Returns `unavailable` when the last search failed and nothing is listed |
| `SetRingLight` | Any of `on` (bool), `brightness` (0–100), `rgb` (`0xRRGGBB`) and `effect` (one of `Status.ring_light.effects`) | Changes the ring light, as Home Assistant's **LED ring** light does. Out-of-range values and unknown effects return `invalid_argument` |
| `ControlMedia` | `command`: `MEDIA_COMMAND_PLAY` (resume), `_PAUSE`, `_STOP`, `_MUTE` or `_UNMUTE`; `volume` (0–100), alone or with a command | Controls the media player, as Home Assistant's does. `volume` sets the stream's own volume while AirPlay or Bluetooth plays, and the volume otherwise. Returns `failed_precondition` on a device without a working speaker |
| `PlayMedia` | `url` (an `http://` or `https://` URL, at most 2048 bytes) | Plays the URL on the media player, as Home Assistant's **Play media** does. Other URLs return `invalid_argument`; a device without a working speaker returns `failed_precondition`. What plays keeps playing until the URL's audio starts. If the audio can't play, it keeps playing, and `Status.media.error` says why |
| `StartCalibration` | Empty | Starts tuning the wake word sensitivity with the + and − buttons on the device. Returns `failed_precondition` while the microphones or the wake word are off, or while a request or a timer alarm is active |
| `StopCalibration` | Empty | Ends it, keeping the sensitivity as it is. Does nothing if none runs |
| `StartBluetoothPairing` | Empty | Opens the Bluetooth pairing window for two minutes, like the **Pair a phone** button. Returns `failed_precondition` unless Bluetooth is on and ready |
| `StopBluetoothPairing` | Empty | Closes it. Does nothing if it is closed |
| `ForgetBluetoothDevice` | `address` (from `Status.bluetooth.devices`) | Removes a paired phone, which has to pair again to play. Returns `failed_precondition` while Bluetooth is off |
| `ReportBluetooth` | `ready`, `address`, `devices`, `error` | Used by `pinna-bt` to report the adapter and the paired phones. Only the device's own programs can call it, on `127.0.0.1` |
| `GetNetwork` | Empty | Returns the hostname; each interface: name, MAC, addresses, and for Wi-Fi the network, signal, band and bitrate; and `mdns_hostname`, the name the device announces over mDNS, which differs from the hostname for a few minutes after a name conflict. `mdns_hostname` is empty when the device doesn't publish its mDNS record |
| `WatchLogs` | Empty | Streams the last 1000 lines of the journal of `pinnad` and of the AirPlay and Bluetooth services, then new lines. Debug and trace lines are left out |
| `GetUpdates` | Empty | Returns the A/B slots (version, state, tries left, which one runs and which one starts next), whether an install runs, the last install's result, and a suggested manifest URL. Also: `check`, the update channel's checks (`checking`, `finished_unix_ms`, `error`, `no_release`, `next_unix_ms`); `release`, what the last successful check found (`version`, `channel`, `summary`, `notes_url`, and `action`, what `InstallUpdate` does with it: `RELEASE_ACTION_INSTALL`, `_UP_TO_DATE`, `_OLDER`, `_START` (held back in the other slot), `_RESTART` (installed, starts next), `_FAILED` (failed its trial start) or `_BLOCKED`, with `note` saying why for the last two); `manifest_override` (the `[update] manifest` that replaces the channel); `restart_pending` (an install from Home Assistant waits for the device to be Idle); `development_overlay` (a `just push` extension stops installs); and `development_build` |
| `CheckForUpdates` | Empty | Checks the update channel now, and answers when the check has finished. A failed check isn't an error here: `GetUpdates.check.error` says why. Returns `failed_precondition` while **Updates** is off or an install runs |
| `InstallUpdate` | `manifest` (an `http://` or `https://` URL), or `version` and `older` | With `manifest`: starts `pinnactl update` on that manifest, which installs it after the signature checks, an older version too. Without: acts on `GetUpdates.release` by its `action`, only if `version` is the release's version. `RELEASE_ACTION_INSTALL` installs it with the channel's rules; `_START` makes the other slot, which already holds it, start next; `_OLDER` installs it only with `older` set. Other actions, a different `version`, or another install running return `failed_precondition` |
| `WatchInstall` | Empty | Streams the output of the running or last install, each line with its `stage`, and ends with a message that has `done` set |
| `SetActiveSlot` | `slot` (`a` or `b`) | Chooses the slot that starts next. The other slot: its image is checked, it starts next as a trial, and the device restarts; if the restart fails, the slots are put back. Only a slot that is good or hasn't been started yet. The running slot: it starts next again, which cancels a pending update, without a restart. Refused while an install runs or while the running slot is on trial |
| `Reboot` | Empty | Restarts the device |
| `FactoryReset` | `name` (the device's name or node name, as typed to confirm; case and surrounding spaces don't matter) | Forgets the settings, the saved Wi-Fi networks, the encryption key that Home Assistant set, SSH keys added on the device and its SSH host key, and restarts the device. The log stays. Another name returns `invalid_argument`. See [Reset the device](/docs/dashboard/#reset-the-device) |
| `ScanWifi` | Empty | Scans for Wi-Fi networks, which takes a few seconds. Returns `networks`, strongest first, one per name, each with `ssid`, `signal_dbm`, `frequency_mhz`, `security`, `saved` and `current`. Networks that hide their name are left out. Returns `failed_precondition` while a join runs |
| `ListWifi` | Empty | Returns the `saved` networks in the order the device tries them (each with `ssid`, `security`, `hidden` and `current`; never the password), whether a join runs (`joining`), the last join's final step (`last_join`), and the network on `trial` |
| `JoinWifi` | `ssid`, `security` (`WIFI_SECURITY_OPEN` or `_WPA_PSK`), `passphrase` (8 to 63 characters or 64 hex digits; empty for an open network) and `hidden` | Tries a network and streams the steps: `JOIN_STAGE_CONNECTING`, `_ADDRESS`, then `_TRIAL` or `_FAILED` (with `failure`). Without an address within 45 seconds, the device goes back to the network it was on. On trial, it keeps the network once the dashboard is opened at the new `address` or `KeepWifi` keeps it, and goes back after `keep_seconds` (5 minutes) otherwise. The join goes on when the caller goes away. Returns `failed_precondition` while another join runs or a network is on trial. See [Change the Wi-Fi network](/docs/dashboard/#change-the-wi-fi-network) |
| `KeepWifi` | `keep` (bool) | Keeps and saves the network on trial, or with `false` goes back now. Returns the join's final step |
| `ForgetWifi` | `ssid` | Forgets a saved network and returns the saved ones. Forgetting the network in use disconnects the device. Returns `not_found` for a network that isn't saved, and `failed_precondition` while a join runs or a network is on trial |

`SetVolume`, `SetStreamVolume`, `SetStreamMetadata`, `SetMute`, `StartConversation`, `StopConversation`,
`ClearNotification`, `UpdateSettings`, `StartCalibration`, `StopCalibration`,
`StartBluetoothPairing`, `StopBluetoothPairing` and `ForgetBluetoothDevice`
return the `Status` after the change.

### Example

To read the status with `curl` on the device:

```sh
curl -s -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7470/pinna.v1.DeviceService/GetStatus
```

### Status

| Field | Type | Description |
| --- | --- | --- |
| `version` | string | The version of `pinnad` |
| `name` | string | The ESPHome node name, for example `pinna-2a914e` |
| `mac` | string | The Wi-Fi MAC address, in lowercase |
| `uptime_seconds` | uint64 | Seconds since `pinnad` started |
| `phase` | `Phase` | `PHASE_IDLE`, `PHASE_LISTENING`, `PHASE_THINKING` or `PHASE_SPEAKING` |
| `muted` | bool | The microphones are off, by either the button or software |
| `hardware_muted` | bool | The hardware switch has turned the microphones off |
| `software_muted` | bool | The software mute is on |
| `volume` | uint32 | The volume, 0–100 |
| `ha_connected` | bool | Home Assistant is connected |
| `voice_subscribed` | bool | Home Assistant has an Assist pipeline for the device |
| `wake_word` | `WakeWord` | `phrase` (the active words, for example `Alexa or Hey Jarvis`); `enabled`: the device listens for a wake word right now; `available`: every word the device can listen for; `threshold`: the score a wake needs at the current sensitivity, 0–1; `recent`: the last 20 wakes and near misses since `pinnad` started, oldest first. Each `WakeEvent` has `unix_ms`, `woke`, `phrase` (the word that woke the device; empty for a near miss), `score` (0–1), `wakes_from` (the lowest sensitivity at which that score wakes the device, unset when even 100 doesn't) and `reached_threshold` (a near miss that the detector's other checks rejected). `calibration` is set while a calibration runs: `started_from` (the sensitivity Undo returns to) and `events`, oldest first. A `CalibrationEvent` has `id`, `kind`, `raise` (the + or − button), `score`, and `from`/`to` (equal when nothing changed) |
| `timers` | `Timer` list | Each with `id`, `name`, `seconds_left`, `total_seconds` and `active` |
| `audio_backend` | string | `echodot-pipewire` (the normal value on the Echo and Echo Dot), `echodot`, `pipewire`, `wav` or `none` |
| `led_backend` | string | `is31`, `mock`, `virtual` (the virtual Echo Dot) or `none` |
| `alarm_ringing` | bool | A finished timer's alarm rings. `StopConversation` stops it |
| `notification` | bool | A notification is waiting. `ClearNotification` clears it |
| `settings` | `Settings` | `wake_word`, `wake_sensitivity`, `led_brightness`, `wake_words` (the active ids, at most two), `friendly_name`, `default_name` (nobody chose a name yet), `wake_sound`, `url` (the dashboard address; empty when none is set), `airplay` and `bluetooth` (the switches), `time_zone` (`UTC` until one is set), `reply_volume` (unset while replies follow the volume), `listening_direction` and `update_channel` (the **Updates** setting in effect) |
| `ring_light` | `RingLight` | The ring as a light: `on`, `brightness` (0–100), `rgb` (`0xRRGGBB`), `effect` and `effects` (every effect it takes) |
| `media` | `Media` | The media player: `state` (`MEDIA_STATE_IDLE`, `_PLAYING`, `_PAUSED` or `_ANNOUNCING`), `url` (what plays, or the URL that is opening while nothing plays; empty for AirPlay and Bluetooth), `stream` (AirPlay or Bluetooth plays), `muted` (the music, not the microphones), `error` (why the last play failed to open or ended badly, until the next play or a stop), `error_url` (with `error`, the URL that failed; what still plays stays in `url`), `available` (the device has a working speaker), `volume` (0–100: the stream's own while AirPlay or Bluetooth plays, the volume otherwise), `title`, `artist` and `album` (what plays: the sender's track, a file's tags or a radio station's title; empty when none is known, while nothing plays, and for media that Home Assistant plays), `station` (an internet radio station's name) and `source` (while a stream plays, the name its program gives, such as `airplay` or `bluetooth`) |
| `airplay` | `AirPlay` | The AirPlay receiver as it runs: `state` (`AIRPLAY_STATE_OFF`, `_STARTING`, `_READY`, `_FAILED`, or `_UNAVAILABLE` on a device without a working speaker), `name` (what senders list it as, while it is on) and `reason` (systemd's result when it failed, for example `exit-code`) |
| `bluetooth` | `Bluetooth` | Bluetooth as it runs: `state` (`BLUETOOTH_STATE_OFF`, `_STARTING`, `_READY`, `_FAILED`, or `_UNAVAILABLE` on a device without Bluetooth or a working speaker), `name` (what phones list it as, while it is on), `address`, `pairing_until_ms` (when the pairing window closes, in Unix milliseconds; 0 while it is closed), `pairing_seconds_left` (the same as seconds left when the `Status` was sent, to count down from whatever the clocks say), `devices` (the paired phones while it is ready: `address`, `name`, `connected`) and `reason` (why it failed) |
| `model` | string | `"echo-dot-2"` on an Echo Dot (2nd gen), `"echo-2"` on an Echo (2nd gen), empty when the hardware is unknown |
| `virtual_device` | bool | The device is the virtual Echo Dot of the development VM |
| `last_boot` | `LastBoot` | How the start before this one ended: `outcome` (`LAST_BOOT_OUTCOME_COLD_START`, `_RESTARTED`, `_RESCUE`, `_CRASHED` or `_RESET`), `reason` (for example `update`) and `summary`, one line such as `restarted (update)` |
| `api_encrypted` | bool | The connection to Home Assistant is encrypted |
| `api_key_from_image` | bool | The encryption key came with the image, so a factory reset keeps it |
| `led_test` | bool | The ring runs its test pattern (`LedTest`, or **Identify** on the dashboard) |

## What's next

- [Home Assistant entities](/docs/entities/)
- [Troubleshooting](/docs/troubleshooting/)
