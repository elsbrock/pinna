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

`pinnactl update` doesn't use the API. It runs on the device as `root` and
writes the update directly. It takes these options:

| Option | Default | Description |
| --- | --- | --- |
| `--reboot` | Off | Restarts the device after the update is written |
| `--tries <number>` | `3` | How many times the device tries to start the new version before it goes back |

### Status

`pinnactl status` prints the device's state. For example:

```text
device:     pinna-2a914e (00:fc:8b:2a:91:4e)
version:    2026.11.15.2312-aeafbda
uptime:     3h 12m 05s
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
- `phase` is `idle`, `listening`, `thinking` or `speaking`.
- `muted` is `no`, `yes (hardware)`, `yes (software)` or
  `yes (hardware, software)`.
- `voice subscribed` means Home Assistant has an Assist pipeline for the
  device.
- The wake word line names the active words, or `none chosen`, and is `armed`
  when the device listens for them right now: the microphones are on, and
  either a timer alarm rings, or the device is idle or speaking a reply or an
  announcement and Home Assistant is subscribed.
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
  addresses, `localhost`, `.local` names, the device's own hostname (its node name, such as `pinna-2a914e`)
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
| `SetMute` | `muted` (bool) | Turns the software mute on or off. The hardware switch isn't affected |
| `StartConversation` | Empty | Starts a request, like the Action button. Returns `failed_precondition` if Home Assistant has no Assist pipeline for the device, the microphones are off, a timer alarm rings or a request is already running |
| `StopConversation` | Empty | Stops a request, a reply, an announcement or a timer alarm |
| `LedTest` | Empty | Runs the LED test |
| `ClearNotification` | Empty | Clears a waiting notification, even if the ring shows something else: the **Notification** switch turns off and the `cleared` event fires. Does nothing if no notification is waiting |
| `UpdateSettings` | Any of `wake_word` (bool), `wake_sensitivity` and `led_brightness` (0–100), `wake_words` (up to two ids from `Status.wake_word.available`), `airplay` and `bluetooth` (bool), `hostname` (one DNS label, stored lowercase), `time_zone` (one of `ListTimeZones`), `url` (the dashboard address: `http://` or `https://`, a host and an optional port, nothing after it; stored with a `/` at the end; an empty string clears it) | Changes those settings, as Home Assistant's entities do. `airplay` returns `failed_precondition` on a device without a working speaker, `bluetooth` also on one without Bluetooth. Values above 100, an invalid hostname or dashboard address, a time zone the device doesn't have, unknown or repeated wake word ids and more than two return `invalid_argument`. `wake_words` returns `failed_precondition` while Home Assistant is connected: its dropdowns own the choice then |
| `ListTimeZones` | Empty | Returns the IANA time zones the device has, from tzdata's `zone1970.tab`, plus `UTC`, that `UpdateSettings` accepts as `time_zone` |
| `ListSatellites` | Empty | Asks the network for the other Pinna devices with a one-shot mDNS query and at most one follow-up, which takes about a second and a half. Returns `satellites`, each with its node name, friendly name, model, MAC, version, dashboard port, dashboard address and addresses (without the USB link's `10.42.0.0/24` and without link-local IPv6 addresses), and `on_network`, which is false, with no query and an empty list, while the device has no address beyond USB. Returns `unavailable` when the query can't be sent |
| `StartCalibration` | Empty | Starts tuning the wake word sensitivity with the + and − buttons on the device. Returns `failed_precondition` while the microphones or the wake word are off, or while a request or a timer alarm is active |
| `StopCalibration` | Empty | Ends it, keeping the sensitivity as it is. Does nothing if none runs |
| `StartBluetoothPairing` | Empty | Opens the Bluetooth pairing window for two minutes, like the **Pair a phone** button. Returns `failed_precondition` unless Bluetooth is on and ready |
| `StopBluetoothPairing` | Empty | Closes it. Does nothing if it is closed |
| `ForgetBluetoothDevice` | `address` (from `Status.bluetooth.devices`) | Removes a paired phone, which has to pair again to play. Returns `failed_precondition` while Bluetooth is off |
| `ReportBluetooth` | `ready`, `address`, `devices`, `error` | Used by `pinna-bt` to report the adapter and the paired phones. Only the device's own programs can call it, on `127.0.0.1` |
| `GetNetwork` | Empty | Returns the hostname and each interface: name, MAC, addresses, and for Wi-Fi the network, signal, band and bitrate |
| `WatchLogs` | Empty | Streams the last 1000 lines of `pinnad`'s journal, then new lines. Debug and trace lines are left out |
| `GetUpdates` | Empty | Returns the A/B slots (version, state, tries left, which one runs and which one starts next), whether an install runs, the last install's result, and a suggested manifest URL |
| `InstallUpdate` | `manifest` (an `http://` or `https://` URL) | Starts `pinnactl update` on the device. Returns `failed_precondition` while another install runs |
| `WatchInstall` | Empty | Streams the output of the running or last install, and ends with a message that has `done` set |
| `Reboot` | Empty | Restarts the device |

`SetVolume`, `SetStreamVolume`, `SetMute`, `StartConversation`, `StopConversation`,
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
| `wake_word` | `WakeWord` | `phrase` (the active words, for example `Alexa or Hey Jarvis`); `enabled`: the device listens for a wake word right now; `available`: every word the device can listen for; `threshold`: the score a wake needs at the current sensitivity, 0–1; `recent`: the last 20 wakes and near misses since `pinnad` started, oldest first. Each `WakeEvent` has `unix_ms`, `woke`, `score` (0–1), `wakes_from` (the lowest sensitivity at which that score wakes the device, unset when even 100 doesn't) and `reached_threshold` (a near miss that the detector's other checks rejected). `calibration` is set while a calibration runs: `started_from` (the sensitivity Undo returns to) and `events`, oldest first. A `CalibrationEvent` has `id`, `kind`, `raise` (the + or − button), `score`, and `from`/`to` (equal when nothing changed) |
| `timers` | `Timer` list | Each with `id`, `name`, `seconds_left`, `total_seconds` and `active` |
| `audio_backend` | string | `echodot-pipewire` (the normal value on the Echo and Echo Dot), `echodot`, `pipewire`, `wav` or `none` |
| `led_backend` | string | `is31`, `mock`, `virtual` (the virtual Echo Dot) or `none` |
| `alarm_ringing` | bool | A finished timer's alarm rings. `StopConversation` stops it |
| `notification` | bool | A notification is waiting. `ClearNotification` clears it |
| `settings` | `Settings` | `wake_word`, `wake_sensitivity`, `led_brightness`, `wake_words` (the active ids, at most two), `airplay` and `bluetooth` (the switches) |
| `airplay` | `AirPlay` | The AirPlay receiver as it runs: `state` (`AIRPLAY_STATE_OFF`, `_STARTING`, `_READY`, `_FAILED`, or `_UNAVAILABLE` on a device without a working speaker), `name` (what senders list it as, while it is on) and `reason` (systemd's result when it failed, for example `exit-code`) |
| `bluetooth` | `Bluetooth` | Bluetooth as it runs: `state` (`BLUETOOTH_STATE_OFF`, `_STARTING`, `_READY`, `_FAILED`, or `_UNAVAILABLE` on a device without Bluetooth or a working speaker), `name` (what phones list it as, while it is on), `address`, `pairing_until_ms` (when the pairing window closes, in Unix milliseconds; 0 while it is closed), `pairing_seconds_left` (the same as seconds left when the `Status` was sent, to count down from whatever the clocks say), `devices` (the paired phones while it is ready: `address`, `name`, `connected`) and `reason` (why it failed) |
| `model` | string | `"echo-dot-2"` on an Echo Dot (2nd gen), `"echo-2"` on an Echo (2nd gen), empty when the hardware is unknown |

## What's next

- [Home Assistant entities](/docs/entities/)
- [Troubleshooting](/docs/troubleshooting/)
