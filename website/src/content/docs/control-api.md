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
version:    0.1.0
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
- The wake word is `armed` when the device listens for it right now: it's
  idle, the microphones are on and Home Assistant is subscribed.
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
  addresses, `localhost`, `.local` names, the device's own hostname (`pinna`)
  and the names in `[web] allowed_hosts`. Other names get
  `421 Misdirected Request`.

### Methods

| Method | Request | What it does |
| --- | --- | --- |
| `GetStatus` | Empty | Returns the current `Status` |
| `WatchStatus` | Empty | Streams the current `Status`, then a new one after each change |
| `SetVolume` | `percent` (0–100) | Sets the volume. Values above 100 return `invalid_argument` |
| `SetMute` | `muted` (bool) | Turns the software mute on or off. The hardware switch isn't affected |
| `StartConversation` | Empty | Starts a request, like the Action button. Returns `failed_precondition` if Home Assistant has no Assist pipeline for the device, the microphones are off, a timer alarm rings or a request is already running |
| `StopConversation` | Empty | Stops a request, a reply, an announcement or a timer alarm |
| `LedTest` | Empty | Runs the LED test |
| `ClearNotification` | Empty | Clears a waiting notification, even if the ring shows something else: the **Notification** switch turns off and the `cleared` event fires. Does nothing if no notification is waiting |
| `UpdateSettings` | Any of `wake_word` (bool), `wake_sensitivity` and `led_brightness` (0–100) | Changes those settings, as Home Assistant's entities do. Values above 100 return `invalid_argument` |
| `GetNetwork` | Empty | Returns the hostname and each interface: name, MAC, addresses, and for Wi-Fi the network, signal, band and bitrate |
| `WatchLogs` | Empty | Streams the last 1000 lines of `pinnad`'s journal, then new lines. Debug and trace lines are left out |
| `GetUpdates` | Empty | Returns the A/B slots (version, state, tries left, which one runs and which one starts next), whether an install runs, the last install's result, and a suggested manifest URL |
| `InstallUpdate` | `manifest` (an `http://` or `https://` URL) | Starts `pinnactl update` on the device. Returns `failed_precondition` while another install runs |
| `WatchInstall` | Empty | Streams the output of the running or last install, and ends with a message that has `done` set |
| `Reboot` | Empty | Restarts the device |

`SetVolume`, `SetMute`, `StartConversation`, `StopConversation`,
`ClearNotification` and `UpdateSettings` return the `Status` after the change.

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
| `wake_word` | `WakeWord` | `phrase`, and `enabled`: the device listens for the wake word right now |
| `timers` | `Timer` list | Each with `id`, `name`, `seconds_left`, `total_seconds` and `active` |
| `audio_backend` | string | `echodot-pipewire` (the normal value on an Echo Dot), `echodot`, `pipewire`, `wav` or `none` |
| `led_backend` | string | `is31`, `mock` or `none` |
| `alarm_ringing` | bool | A finished timer's alarm rings. `StopConversation` stops it |
| `notification` | bool | A notification is waiting. `ClearNotification` clears it |
| `settings` | `Settings` | `wake_word`, `wake_sensitivity` and `led_brightness` |
| `model` | string | `"echo-dot-2"` on an Echo Dot (2nd gen), empty when the hardware is unknown |

## What's next

- [Home Assistant entities](/docs/entities/)
- [Troubleshooting](/docs/troubleshooting/)
