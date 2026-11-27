---
title: pinnactl and the control API
description: Reference for the pinnactl command and the local ConnectRPC API that it uses.
---

`pinnactl` controls a Pinna device from its own command line. It uses the
control API, which `pinnad` serves on the device. Other programs on the device
can use the same API, and so does the [dashboard](/docs/dashboard/).

## pinnactl

Run `pinnactl` on the device, for example over SSH (see
[Log in to a device](/docs/development/#log-in-to-a-device)). By default, it connects to
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
  device. In [standalone mode](/docs/standalone/), the device offers Home
  Assistant no voice assistant: the line says only whether Home Assistant is
  connected, and a `mode:` line before it says `standalone, services ready`
  or `standalone, services not ready`, followed by `last failure:` and the
  last failed request's sentence when there is one.
- The wake word line names the active words, or `none chosen`, and is `armed`
  when the device listens for them right now: the microphones are on, and
  either a timer alarm rings, or the device isn't listening to a request and
  can start one. In Home Assistant mode, that means Home Assistant is
  subscribed; in standalone mode, that speech-to-text, the brain and
  text-to-speech have passed their check.
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
and JSON. A second service, `pinna.v1.AssistantService`, sets up standalone
mode; see [AssistantService](#assistantservice).

`pinnad` serves the API in two places:

- On `127.0.0.1:7470`, which only programs on the device itself can reach.
  It has no authentication: anyone who can reach it can control the device.
  `pinnactl` and the boot health check use this address. It serves
  `DeviceService` only.
- On port 80, with the dashboard, for your network (`[web]` in the
  [developer settings](/docs/configure/#web)). It serves `DeviceService` and
  `AssistantService`, and `PeerService`, which the other satellites of a
  [satellite group](#satellite-group) call. Without a
  [dashboard password](/docs/dashboard/#who-can-use-the-dashboard), it has no
  authentication. With one, every call needs a session, which `POST
  /auth/login` gives you (see [Sign in](#sign-in)), except `GetSecurity`,
  `SetDashboardPassword`, `StartConfirmation`, `WatchConfirmation` and
  `CancelConfirmation`.
  `PeerService` checks the group key instead, `/mcp` needs the agent token and
  `/metrics` the metrics token. It only answers requests for IP
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
| `SetMute` | `muted` (bool) | Turns the software mute on or off. The hardware switch isn't affected, so `muted: false` doesn't turn the microphones on after the **Microphone off** button or a request by voice closed it |
| `StartConversation` | Empty | Starts a request, like the Action button. Returns `failed_precondition` if Home Assistant has no Assist pipeline for the device (in standalone mode: if speech-to-text, the brain and text-to-speech haven't all passed their check), the microphones are off, a timer alarm rings, or a request, a reply or an announcement is already running |
| `StopConversation` | Empty | Stops a request, a reply, an announcement or a timer alarm |
| `LedTest` | Empty | Runs the LED test |
| `ClearNotification` | Empty | Clears a waiting notification, even if the ring shows something else: the **Notification** switch turns off and the `cleared` event fires. Does nothing if no notification is waiting |
| `UpdateSettings` | Any of `friendly_name` (the name Home Assistant shows: 1 to 64 characters, no control characters), `wake_word` and `wake_sound` (bool), `wake_sensitivity` and `led_brightness` (0–100), `wake_words` (up to two ids from `Status.wake_word.available`), `airplay`, `bluetooth` and `sendspin` (bool), `hostname` (one DNS label, stored lowercase), `time_zone` (one of `ListTimeZones`), `url` (the dashboard address: `http://` or `https://`, a host that other devices can reach and an optional port, nothing after it; stored with a `/` at the end; an empty string clears it), `reply_volume` (0–100), `reply_follows_volume` and `listening_direction` (bool), `update_channel` (`UPDATE_CHANNEL_STABLE`, `_BETA` or `_OFF`), `mode` (`MODE_HOME_ASSISTANT` or `MODE_STANDALONE`), and `group_messages` (bool) | Changes those settings through the same paths as Home Assistant's entities; `friendly_name`, `hostname`, `time_zone`, `url` and `mode` have no entity. Home Assistant reads a new `friendly_name` or `hostname` when it next connects: the device makes it reconnect once no request, reply, announcement, music or timer alarm plays. `reply_follows_volume` set to true makes replies follow the volume again; false keeps the current level. `reply_follows_volume` true together with `reply_volume` returns `invalid_argument`. `airplay` and `sendspin` return `failed_precondition` on a device without a working speaker, `bluetooth` also on one without Bluetooth. `update_channel` starts a check of the new channel, and returns `failed_precondition` while `[update] manifest` in `pinnad.toml` replaces the channel. Values above 100, an empty or too long name, an invalid hostname or dashboard address, a time zone the device doesn't have, unknown or repeated wake word ids and more than two return `invalid_argument`. `wake_words` returns `failed_precondition` while Home Assistant is connected in Home Assistant mode: its dropdowns own the choice then. `mode` set to `MODE_STANDALONE` returns `failed_precondition` until `Status.assistant.ready`. A change of `mode` stops a running request and a ringing timer alarm, deletes every timer and forgets the conversation; music keeps playing. If Home Assistant is connected, the device makes it reconnect once no request runs, no alarm rings and nothing but music plays, so it reads the new voice features. `group_messages` set to false refuses messages from the other members of the [satellite group](/docs/satellite-groups/#pass-messages-between-satellites); messages already held still play |
| `ListTimeZones` | Empty | Returns the IANA time zones the device has, from tzdata's `zone1970.tab`, plus `UTC`, that `UpdateSettings` accepts as `time_zone` |
| `ListSatellites` | Empty | Returns the other Pinna devices that the device's background search last found, at once. The device searches once a minute with a one-shot mDNS query over IPv4 and IPv6 and follow-ups for missing records, each sent three times, and lists a device once its dashboard accepts a connection. A device stays listed until it misses two searches in a row: it isn't found, its dashboard doesn't answer, or the search fails. After that, it stays listed while its dashboard still accepts a connection at the addresses it was last listed with, until it has missed ten searches in a row. Returns `satellites`, one per MAC address, each with its ESPHome instance name, the host its SRV record names (its `.local` name), friendly name, model, MAC, version, dashboard port, dashboard address and addresses (without the USB link's `10.42.0.0/24` and without link-local IPv6 addresses); `on_network`, which is false, with an empty list, while the device has no address beyond USB; and `searched`, which is false until the first search after the device starts has finished. Returns `unavailable` when the last search failed and nothing is listed. Each satellite has `group_member`, true only when the entry carries the MAC address of a member of this device's [satellite group](/docs/satellite-groups/), announces the address that member's last signed call came from, and has no dashboard address of its own; any other entry is only what the network announced |
| `SetRingLight` | Any of `on` (bool), `brightness` (0–100), `rgb` (`0xRRGGBB`) and `effect` (one of `Status.ring_light.effects`) | Changes the ring light, as Home Assistant's **LED ring** light does. Out-of-range values and unknown effects return `invalid_argument` |
| `ControlMedia` | `command`: `MEDIA_COMMAND_PLAY` (resume), `_PAUSE`, `_STOP`, `_MUTE` or `_UNMUTE`; `volume` (0–100), alone or with a command | Controls the media player, as Home Assistant's does. `volume` sets the stream's own volume while AirPlay or Bluetooth plays, the [Sendspin](/docs/sendspin/) player's volume, which Music Assistant shows, while Sendspin plays, and the volume otherwise. While Sendspin plays, `_MUTE` and `_UNMUTE` act on the Sendspin player's mute, and `_PLAY`, `_PAUSE` and `_STOP` act on the whole group, as they do while the group waits paused after a `_PAUSE` from the device. Returns `failed_precondition` on a device without a working speaker |
| `PlayMedia` | `url` (an `http://` or `https://` URL, at most 2048 bytes) | Plays the URL on the media player, as Home Assistant's **Play media** does. Other URLs return `invalid_argument`; a device without a working speaker returns `failed_precondition`. What plays keeps playing until the URL's audio starts. If the audio can't play, it keeps playing, and `Status.media.error` says why |
| `StartCalibration` | Empty | Starts tuning the wake word sensitivity with the + and − buttons on the device. Returns `failed_precondition` while the microphones or the wake word are off, or while a request or a timer alarm is active |
| `StopCalibration` | Empty | Ends it, keeping the sensitivity as it is. Does nothing if none runs |
| `AddWakeWord` | `phrase` (2 to 40 characters, no control characters, different from every other word's phrase ignoring case, spaces and punctuation; saved with single spaces) and `model` (the bytes of an openWakeWord model, `.onnx` or `.tflite`, at most 2 MiB) | Tests the model in a separate process, at the lowest CPU priority, with at most 256 MiB of memory, 30 s of processor time and 60 s in all. The model must load as a classifier from 16 embeddings to one score, take at most 8 ms per 80 ms block, and stay below the wake threshold at the current sensitivity on generated silence and noise. A model that passes is saved, and returns the new word's `id`: it is in `Status.wake_word.available` with `custom` set, and isn't active. If Home Assistant is connected, the device makes it reconnect once no request, reply, announcement, music or timer alarm plays, so it reads the new list. A phrase that breaks a rule, an empty or too large model, a model that fails the test, and a test that the device can't stage or start return `invalid_argument` with the reason. More than 8 custom words, a second upload while one is tested, and a model that the device can't save after the test return `failed_precondition` |
| `RemoveWakeWord` | `id` | Deletes a custom word and its model. If the device listened for it, it stops. Home Assistant reconnects as after `AddWakeWord`. An id that isn't a custom word returns `failed_precondition` |
| `StartBluetoothPairing` | Empty | Opens the Bluetooth pairing window for two minutes, like the **Pair a phone** button. Returns `failed_precondition` unless Bluetooth is on and ready |
| `StopBluetoothPairing` | Empty | Closes it. Does nothing if it is closed |
| `ForgetBluetoothDevice` | `address` (from `Status.bluetooth.devices`) | Removes a paired phone, which has to pair again to play. Returns `failed_precondition` while Bluetooth is off |
| `ReportBluetooth` | `ready`, `address`, `devices`, `error` | Used by `pinna-bt` to report the adapter and the paired phones. Only the device's own programs can call it, on `127.0.0.1` |
| `GetNetwork` | Empty | Returns the hostname; each interface: name, MAC, addresses, and for Wi-Fi the network, signal, band and bitrate; and `mdns_hostname`, the name the device announces over mDNS, which differs from the hostname for a few minutes after a name conflict. `mdns_hostname` is empty when the device doesn't publish its mDNS record |
| `WatchLogs` | Empty | Streams the last 1000 lines of the journal of `pinnad` and of the AirPlay and Bluetooth services, then new lines. Debug and trace lines are left out |
| `StartDebugging` | `minutes` (15, 60 or 480), `levels`, `text_grant` | Turns debugging on for `minutes`, or replaces the time and what it records while it runs. It records the phases and events, and the metadata of every request the device sends to a service. `levels` adds the audio levels in 100 ms buckets; `text_grant`, from a `CONFIRMATION_PURPOSE_DEBUGGING_TEXT` confirmation this session asked for, adds the text of the requests. Both need a dashboard password and a session, and stop after an hour at most or when the microphones are turned off. Other `minutes` return `invalid_argument`; levels or text without a password, or while the microphones are off, return `failed_precondition`, without a session `unauthenticated` |
| `StopDebugging` | Empty | Turns debugging off and clears what it kept, here and for every `WatchTimeline` follower |
| `WatchTimeline` | Empty | Streams the debugging state (`TimelineState`), what debugging kept in the last 10 minutes as `TimelineBatch` messages of events, requests and audio levels, then what it records next. A request comes again each time it changes, whole. While debugging is off, it sends the state and what is still kept, then waits. A state with `cleared` set means that `StopDebugging`, a new password or `SignOutEverywhere` cleared what was kept. A follower that falls behind is cut off |
| `GetUpdates` | Empty | Returns the A/B slots (version, state, tries left, which one runs and which one starts next), whether an install runs, the last install's result, and a suggested manifest URL. Also: `check`, the update channel's checks (`checking`, `finished_unix_ms`, `error`, `no_release`, `next_unix_ms`); `release`, what the last successful check found (`version`, `channel`, `summary`, `notes_url`, and `action`, what `InstallUpdate` does with it: `RELEASE_ACTION_INSTALL`, `_UP_TO_DATE`, `_OLDER`, `_START` (held back in the other slot), `_RESTART` (installed, starts next), `_FAILED` (failed its trial start) or `_BLOCKED`, with `note` saying why for the last two); `manifest_override` (the `[update] manifest` that replaces the channel); `restart_pending` (an install from Home Assistant, or one the brain started in standalone mode, waits for the device to be Idle with no timer set and nothing playing); `development_overlay` (a `just push` extension stops installs); and `development_build`. Each slot has `older_than_running`, true when its version is older than the running one, by the order that updates use; the dashboard warns before it starts one while a dashboard password is set |
| `CheckForUpdates` | Empty | Checks the update channel now, and answers when the check has finished. A failed check isn't an error here: `GetUpdates.check.error` says why. Returns `failed_precondition` while **Updates** is off or an install runs |
| `InstallUpdate` | `manifest` (an `http://` or `https://` URL), or `version` and `older` | With `manifest`: starts `pinnactl update` on that manifest, which installs it after the signature checks, an older version too. Without: acts on `GetUpdates.release` by its `action`, only if `version` is the release's version. `RELEASE_ACTION_INSTALL` installs it with the channel's rules; `_START` makes the other slot, which already holds it, start next; `_OLDER` installs it only with `older` set; `_RESTART` (already installed) does nothing. Other actions, a different `version`, or another install running return `failed_precondition` |
| `WatchInstall` | Empty | Streams the output of the running or last install, each line with its `stage`, and ends with a message that has `done` set |
| `SetActiveSlot` | `slot` (`a` or `b`) | Chooses the slot that starts next. The other slot: its image is checked, it starts next as a trial, and the device restarts; if the restart fails, the slots are put back. Only a slot that is good or hasn't been started yet. The running slot: it starts next again, which cancels a pending update, without a restart. Refused while an install runs or while the running slot is on trial |
| `Reboot` | Empty | Restarts the device |
| `FactoryReset` | `name` (the device's name or node name, as typed to confirm; case and surrounding spaces don't matter) | Forgets the settings, standalone mode's setup and keys, the dashboard password, its sessions and the tokens, the saved Wi-Fi networks, the encryption key that Home Assistant set, SSH keys added on the device and its SSH host key, and restarts the device. The log stays. Another name returns `invalid_argument`. See [Reset the device](/docs/settings/#reset-the-device) |
| `OpenHomeAssistantWindow` | Empty | With a dashboard password set and no ESPHome key yet, lets Home Assistant connect to the ESPHome API for 15 minutes; it refuses connections otherwise. Returns the `Security` state, whose `home_assistant_window_left_s` counts the seconds left. Returns `failed_precondition` when no password is set, or Home Assistant has set its key already |
| `AllowGroupJoin` | Empty | With a dashboard password set, lets another satellite add this one to its [satellite group](/docs/satellite-groups/) for 10 minutes; a `Join` is refused otherwise. Returns the `Security` state, whose `group_join_left_s` counts the seconds left. Returns `failed_precondition` when no password is set: the *Satellite group* switch is consent then |
| `GetSecurity` | Empty | Returns the `Security` state: whether a password is set, whether the owner chose to leave the dashboard open, whether a session is signed in, and the waits (`login_retry_after_s`, `confirmation_cooldown_s`). For a signed-in caller it also returns `confirmation_waiting_from`, the address that asked for the confirmation that waits, and `confirmation_left_s`, the seconds it still waits; both are empty and 0 when none waits. Needs no session. Without a session, with a password set, it returns only `password_set` and the waits; the failures, the tokens' last four characters, the names of agents, the windows and the waiting confirmation are for a signed-in caller |
| `SetDashboardPassword` | `new_password` (8 to 128 characters), and `current_password` or `grant` | Sets or changes the dashboard password, and ends every session. Needs no session: it checks its own credentials. With a password set it needs `current_password` or a `grant`; without one, a `grant` from a confirmation. A wrong current password counts as a failed sign-in |
| `RemoveDashboardPassword` | `current_password` or `grant` | Removes the password, and ends every session. Needs a session and the current password or a grant |
| `LeaveDashboardOpen` | Empty | Records that the owner chose **Leave it open**, so the dashboard stops offering a password. Returns `failed_precondition` when a password is set |
| `StartConfirmation` | `purpose` (optional) | Asks for someone at the satellite: for 60 seconds the ring shows the confirm pattern, and the next press of **Action** confirms. With `purpose` `CONFIRMATION_PURPOSE_RECORDING` or `CONFIRMATION_PURPOSE_DEBUGGING_TEXT`, the ring circles in the Listening colour instead, the call needs a dashboard password and a session, and the grant is good for 10 seconds, for that session only. Returns the `id` to pass to `WatchConfirmation` and the `seconds` it waits. Records the address of the caller. Without a `purpose`, it needs no session. Returns `resource_exhausted` while another confirmation waits or one ended in the last 60 seconds (10 seconds after a cancel), and says how long to wait |
| `WatchConfirmation` | `id` | Streams the confirmation: `CONFIRMATION_STATE_WAITING` with `seconds_left`, then `_CONFIRMED` with a one-time `grant` that is valid for 5 minutes, `_EXPIRED`, or `_CANCELED` after `CancelConfirmation`. Needs no session |
| `CancelConfirmation` | `id` (optional) | Ends a confirmation: the ring stops at once, its watchers see `CONFIRMATION_STATE_CANCELED`, and the next confirmation can start 10 seconds later. With the `id` of the confirmation, it needs no session: the `id` is the proof. With an empty `id`, it ends whichever confirmation waits, and needs a session; without one it returns `unauthenticated`. An unknown `id` returns `not_found`. Ending a confirmation that isn't waiting is not an error |
| `CreateToken` | `kind`: `TOKEN_KIND_METRICS` or `TOKEN_KIND_AGENT` | Creates the token for `/metrics` or for `/mcp`. Returns the `token`, once, and the `Security` state. A new token replaces the old one, which stops working at once. Only the SHA-256 of a token is kept |
| `SignOutEverywhere` | Empty | Ends every session, in every browser, and closes the streams they hold open. Tokens keep working. Returns the `Security` state |
| `ScanWifi` | Empty | Scans for Wi-Fi networks, which takes a few seconds. Returns `networks`, strongest first, one per name, each with `ssid`, `signal_dbm`, `frequency_mhz`, `security`, `saved` and `current`. Networks that hide their name are left out. Returns `failed_precondition` while a join runs |
| `ListWifi` | Empty | Returns the `saved` networks in the order the device tries them (each with `ssid`, `security`, `hidden` and `current`; never the password), whether a join runs (`joining`), the last join's final step (`last_join`), and the network on `trial` |
| `JoinWifi` | `ssid`, `security` (`WIFI_SECURITY_OPEN` or `_WPA_PSK`), `passphrase` (8 to 63 characters or 64 hex digits; empty for an open network) and `hidden` | Tries a network and streams the steps: `JOIN_STAGE_CONNECTING`, `_ADDRESS`, then `_TRIAL` or `_FAILED` (with `failure`). Without an address within 45 seconds, the device goes back to the network it was on. On trial, it keeps the network once the dashboard is opened at the new `address` or `KeepWifi` keeps it, and goes back after `keep_seconds` (5 minutes) otherwise. The join goes on when the caller goes away. Returns `failed_precondition` while another join runs or a network is on trial. See [Change the Wi-Fi network](/docs/settings/#change-the-wi-fi-network) |
| `KeepWifi` | `keep` (bool) | Keeps and saves the network on trial, or with `false` goes back now. Returns the join's final step |
| `ForgetWifi` | `ssid` | Forgets a saved network and returns the saved ones. Forgetting the network in use disconnects the device. Returns `not_found` for a network that isn't saved, and `failed_precondition` while a join runs or a network is on trial |

`SetVolume`, `SetStreamVolume`, `SetStreamMetadata`, `SetMute`, `StartConversation`, `StopConversation`,
`ClearNotification`, `UpdateSettings`, `StartCalibration`, `StopCalibration`,
`StartBluetoothPairing`, `StopBluetoothPairing` and `ForgetBluetoothDevice`
return the `Status` after the change.

### Sign in

With a dashboard password set, a browser or a script signs in over plain HTTP,
not Connect, because the answer sets a cookie. Both calls take only the
names that the dashboard answers to, and a request from a web page must come
from one of them:

| Call | What it does |
| --- | --- |
| `POST /auth/login` | Takes `{"password": "..."}` as `application/json`, at most 4 KiB. Answers `204` and sets a session cookie that lasts 30 days. Answers `401` for a wrong password, `409` when the satellite has no password, and `429` with `retry_after_s` while sign-ins wait |
| `POST /auth/logout` | Clears the session cookie of this browser. Answers `204`. Use `SignOutEverywhere` to end every session |

Send the cookie with later calls. Without a session, a call to `DeviceService`
or `AssistantService` returns `unauthenticated`.

### Example

To read the status with `curl` on the device:

```sh
curl -s -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7470/pinna.v1.DeviceService/GetStatus
```

To read standalone mode's setup, which only the dashboard's listener serves:

```sh
curl -s -X POST -H 'Content-Type: application/json' -d '{}' \
  http://localhost/pinna.v1.AssistantService/GetAssistant
```

### Status

| Field | Type | Description |
| --- | --- | --- |
| `version` | string | The version of `pinnad` |
| `name` | string | The ESPHome node name, for example `pinna-2a914e` |
| `mac` | string | The Wi-Fi MAC address, in lowercase |
| `uptime_seconds` | uint64 | Seconds since `pinnad` started |
| `phase` | `Phase` | `PHASE_IDLE`, `PHASE_LISTENING`, `PHASE_THINKING` or `PHASE_SPEAKING` |
| `mode` | `Mode` | `MODE_HOME_ASSISTANT` or `MODE_STANDALONE`: which service answers voice requests |
| `muted` | bool | The microphones are off, by either the button or software |
| `hardware_muted` | bool | The hardware switch has turned the microphones off |
| `software_muted` | bool | The software mute is on |
| `volume` | uint32 | The volume, 0–100 |
| `ha_connected` | bool | Home Assistant is connected |
| `voice_subscribed` | bool | Home Assistant has an Assist pipeline for the device |
| `wake_word` | `WakeWord` | `phrase` (the active words, for example `Alexa or Hey Jarvis`); `enabled`: the device listens for a wake word right now; `available`: every word the device offers, each with `id`, `phrase`, `custom` (added with `AddWakeWord`) and `error` (why the device can't listen for it, for example a model that failed to load; such a word can't be chosen, and Home Assistant isn't offered it); `threshold`: the score a wake needs at the current sensitivity, 0–1; `recent`: the last 20 wakes and near misses since `pinnad` started, oldest first. Each `WakeEvent` has `unix_ms`, `woke`, `phrase` (the word that woke the device; empty for a near miss), `score` (0–1), `wakes_from` (the lowest sensitivity at which that score wakes the device, unset when even 100 doesn't) and `reached_threshold` (a near miss that the detector's other checks rejected). `calibration` is set while a calibration runs: `started_from` (the sensitivity Undo returns to) and `events`, oldest first. A `CalibrationEvent` has `id`, `kind`, `raise` (the + or − button), `score`, and `from`/`to` (equal when nothing changed) |
| `timers` | `Timer` list | Each with `id`, `name`, `seconds_left`, `total_seconds` and `active` |
| `audio_backend` | string | `echodot-pipewire` (the normal value on the Echo and Echo Dot), `echodot`, `pipewire`, `wav` or `none` |
| `led_backend` | string | `is31`, `mock`, `virtual` (the virtual Echo Dot) or `none` |
| `alarm_ringing` | bool | A finished timer's alarm rings. `StopConversation` stops it |
| `notification` | bool | A notification is waiting. `ClearNotification` clears it |
| `settings` | `Settings` | `wake_word`, `wake_sensitivity`, `led_brightness`, `wake_words` (the active ids, at most two), `friendly_name`, `default_name` (nobody chose a name yet), `wake_sound`, `url` (the dashboard address; empty when none is set), `airplay`, `bluetooth` and `sendspin` (the switches), `time_zone` (`UTC` until one is set), `reply_volume` (unset while replies follow the volume), `listening_direction`, `update_channel` (the **Updates** setting in effect), `mode` and `group_messages` (the satellite takes messages from its [satellite group](/docs/satellite-groups/#pass-messages-between-satellites); on by default) |
| `assistant` | `AssistantStatus` | Standalone mode's services, in either mode: `ready` (speech-to-text, the brain and text-to-speech are set up and have passed their check, so standalone mode can be switched on), `stt_ready`, `brain_ready` and `tts_ready`, and `label` (the brain's preset and model, such as `openai: gpt-4o-mini`, which the dashboard shows as `gpt-4o-mini via OpenAI`; empty until all three are set up). The last standalone request that failed: `last_error` (one sentence, such as `No credit is left on this account.`; never a key, an address or what was said), `last_error_slot` (`stt`, `brain`, `tts` or `tool`), `last_error_preset` (the preset of the connection that failed, such as `openrouter`) and `last_error_unix_ms`. They are empty, and `last_error_unix_ms` is 0, when no request has failed since `pinnad` started or a request has been answered since |
| `ring_light` | `RingLight` | The ring as a light: `on`, `brightness` (0–100), `rgb` (`0xRRGGBB`), `effect` and `effects` (every effect it takes) |
| `media` | `Media` | The media player: `state` (`MEDIA_STATE_IDLE`, `_PLAYING`, `_PAUSED` or `_ANNOUNCING`), `url` (what plays, or the URL that is opening while nothing plays; empty for AirPlay, Bluetooth and Sendspin), `stream` (AirPlay, Bluetooth or Sendspin plays), `muted` (the music, not the microphones), `error` (why the last play failed to open or ended badly, until the next play or a stop), `error_url` (with `error`, the URL that failed; what still plays stays in `url`), `available` (the device has a working speaker), `volume` (0–100: the stream's own while AirPlay or Bluetooth plays, the Sendspin player's while Sendspin plays, the volume otherwise), `title`, `artist` and `album` (what plays: the sender's track, a file's tags or a radio station's title; empty when none is known, while nothing plays, and for media that Home Assistant plays), `station` (an internet radio station's name) and `source` (while a stream plays, the name its program gives, such as `airplay` or `bluetooth`, or `sendspin`; `sendspin` also while its group waits paused after a pause from the device, when `state` is `MEDIA_STATE_PAUSED` and the track is the one it last played) |
| `airplay` | `AirPlay` | The AirPlay receiver as it runs: `state` (`AIRPLAY_STATE_OFF`, `_STARTING`, `_READY`, `_FAILED`, or `_UNAVAILABLE` on a device without a working speaker), `name` (what senders list it as, while it is on) and `reason` (systemd's result when it failed, for example `exit-code`) |
| `bluetooth` | `Bluetooth` | Bluetooth as it runs: `state` (`BLUETOOTH_STATE_OFF`, `_STARTING`, `_READY`, `_FAILED`, or `_UNAVAILABLE` on a device without Bluetooth or a working speaker), `name` (what phones list it as, while it is on), `address`, `pairing_until_ms` (when the pairing window closes, in Unix milliseconds; 0 while it is closed), `pairing_seconds_left` (the same as seconds left when the `Status` was sent, to count down from whatever the clocks say), `devices` (the paired phones while it is ready: `address`, `name`, `connected`) and `reason` (why it failed) |
| `sendspin` | `Sendspin` | The [Sendspin](/docs/sendspin/) player as it runs: `state` (`SENDSPIN_STATE_OFF`, `_READY` (listening, with no server connected), `_CONNECTED` (a server is connected and nothing plays), `_PLAYING`, `_FAILED`, or `_UNAVAILABLE` on a device without a working speaker), `name` (what Music Assistant lists it as, while it is on), `server` (the connected server's name, while one is), `reason` (why it failed, such as `port 8928 is in use`; while it is ready, why the last server couldn't connect, such as `Music Assistant must be 2.10.7 or later`; a lower-case phrase without a full stop, empty otherwise) and `exact_timing` (the speaker's hardware clock times the audio, so it plays in step with its group; false on the virtual Echo Dot and other computers) |
| `model` | string | `"echo-dot-2"` on an Echo Dot (2nd gen), `"echo-2"` on an Echo (2nd gen), `"echo-dot-5"` on an Echo Dot (5th gen), empty when the hardware is unknown |
| `virtual_device` | bool | The device is the virtual Echo Dot of the development VM |
| `last_boot` | `LastBoot` | How the start before this one ended: `outcome` (`LAST_BOOT_OUTCOME_COLD_START`, `_RESTARTED`, `_RESCUE`, `_CRASHED` or `_RESET`), `reason` (for example `update`) and `summary`, one line such as `restarted (update)` |
| `api_encrypted` | bool | The connection to Home Assistant is encrypted |
| `held_messages` | uint32 | Messages from the other members of the [satellite group](/docs/satellite-groups/#pass-messages-between-satellites) that wait to be said, at most 5. They play once the device is idle and the microphones are on, and are lost when `pinnad` restarts |
| `api_key_from_image` | bool | The encryption key came with the image, so a factory reset keeps it |
| `led_test` | bool | The ring runs its test pattern (`LedTest`, or **Identify** on the dashboard) |
| `debugging` | `Debugging` | Set while debugging runs: `seconds_left`, and `levels_seconds_left` and `text_seconds_left` for the audio levels and the text, 0 when they aren't recorded. See `StartDebugging` |

## AssistantService

`pinna.v1.AssistantService`, defined in
[`proto/pinna/v1/assistant.proto`](https://github.com/elsbrock/pinna/blob/main/proto/pinna/v1/assistant.proto),
sets up [standalone mode](/docs/standalone/): the connections to your
services, the speech-to-text, brain and text-to-speech slots that use them,
the conversation, the tools and your MCP servers. The dashboard's
**Assistant** page uses it. `pinnad` serves it on port 80 with the dashboard,
not on `127.0.0.1:7470`. It works in either mode, so you can set up standalone
mode before you switch to it with `UpdateSettings`.

Keys and MCP server addresses are write-only: no response contains them. A
connection shows only `key_set` and `key_last4`, the key's last four
characters. An MCP server shows only its `host`, because its address can carry
a token. A key belongs to the address it was entered for: a new `base_url`
without a new `key` removes the stored key.

A refused change changes nothing. Its message is one sentence for the owner,
with one of these codes:

- `invalid_argument`: the request is wrong in itself, for example an empty
  model or a prompt over 4,000 characters.
- `failed_precondition`: a service's check failed, or the device refuses it
  now.
- `aborted`: the request names a `revision` that changed meanwhile. Read the
  setup again with `GetAssistant`, then retry.

| Method | Request | What it does |
| --- | --- | --- |
| `GetAssistant` | Empty | Returns the `Assistant`: `ready` (all three slots have passed their check), `connections`, the `stt`, `brain` and `tts` slots, `brain_override`, `language`, `place`, `prompt` (unset means `voice_rules`), `follow_ups`, `ack` and `ack_spoken` (what a slow agent's acknowledgment says), the tool switches `tools` (a tool without an entry is on, unless its `device_tools` entry has `off_by_default` set, as `install_update` has), `stations`, `mcp_servers`, `agents` (the last 8 MCP clients that called `/mcp` since `pinnad` started, each with the name it gave and its address), the `presets` a connection can use, `voice_rules` and `device_tools` (each with `name`, `description`, `brain`, `mcp` and `off_by_default`) |
| `SetConnection` | `id` (empty adds a connection), `revision`, `preset` (an id from `presets`, such as `openai` or `ollama`), `base_url`, and `key` or `clear_key` | Adds or changes a connection. An empty `base_url` takes the preset's address. For the `ollama`, `lmstudio`, `hermes` and `openclaw` presets, the computer's name or address is enough: the preset adds what is missing (OpenClaw needs the port too). The `custom` preset takes the whole address. The slots that use the connection are checked again with the new address and key. In standalone mode, a failed check refuses the change with `failed_precondition`; in Home Assistant mode, the change is saved, and the slot isn't ready until it passes. At most 16 connections. `key` together with `clear_key` returns `invalid_argument` |
| `DeleteConnection` | `id`, `revision` | Removes a connection and its key. In standalone mode, a connection that a slot uses returns `failed_precondition`. In Home Assistant mode, the slots that use it are cleared with it |
| `SetSlot` | `slot` (`ASSISTANT_SLOT_STT`, `_BRAIN` or `_TTS`), `connection`, `model` (the model, or for text-to-speech the voice; empty for a preset with a fixed brain model), and for text-to-speech `speech_model` (the model that speaks the voice; empty is the preset's `tts_model`) | Checks the slot and saves it only when the check passes; the slot is then ready. A check asks for the model list, transcribes half a second of silence, or speaks one word. It never sends a prompt. A failed check returns `failed_precondition` with the reason |
| `UpdateAssistant` | Any of `language` (a BCP 47 tag such as `de` or `en-US`; empty clears it), `prompt` (at most 4,000 characters; empty sends only the context) or `default_prompt` (back to `voice_rules`), `follow_ups` (bool), `ack` (`enabled` and a `phrase` of at most 100 characters), `tools` (replaces every switch), `brain_override` (replaces every Advanced setting: `delegation`, `history` with `history_limit` (1 to 10 turns, or at most 65536 bytes), `first_text_seconds`, 1 to 300, and `reasoning`, sent as `reasoning_effort` to a brain whose preset has `thinking`), `place` (a place name to look up, such as a town; empty clears it) and `stations` (replaces every station: at most 50, each a `name` of at most 60 characters and an `http://` or `https://` `url`) | Changes the settings that need no check. A place that the search doesn't find returns `invalid_argument`. `prompt` together with `default_prompt` returns `invalid_argument`. With `prompt` or `default_prompt`, `prompt_revision` names the `Assistant.prompt_revision` the edit started from; if the prompt changed since, the edit returns `aborted` and the prompt stays as it is |
| `SetMcpServer` | `id` (empty adds a server), `revision`, `name` (1 to 24 lowercase letters, digits and hyphens, not used by another server), `url` (the whole address, with any token it carries; needed to add a server, unset keeps the stored one), `enabled`, and `credential` or `clear_credential` | Adds or changes one of your MCP servers. The brain may call its tools as `<name>_<tool>`. At most 8. `credential` is one of `bearer` (`token`), `basic` (`user`, `password`) or `header` (`name`, `value`), sent with every request to the server; unset keeps the stored one, unless `url` changes, which removes it. `credential` together with `clear_credential`, or a credential the satellite refuses, such as a header it sets itself, returns `invalid_argument`. `McpServer.credential` reports the stored one's `kind`, its `label` (the user name or the header's name) and the `last4` characters of a token or header value of 16 or more, never the secret itself; `plain_http` says the address sends it unencrypted. After the change, the device asks each enabled server for its tools, and `McpServer.tools`, `listed` (each tool by the name the brain calls it, which is also the name of its switch in `tools`) and `problem` say how it answered |
| `DeleteMcpServer` | `id`, `revision` | Removes an MCP server and its address |
| `PlaySample` | `text` (at most 500 characters) | Says the text with the text-to-speech slot, in either mode, and answers once it sounds, or with why it couldn't. Music gets quieter under it. Returns `failed_precondition` while a request runs, a timer alarm rings or the device already says something, before text-to-speech is set up, and on a device without a working speaker |
| `AskText` | `text` (at most 1,000 characters) | Asks the brain a typed question, as if it was spoken. The reply plays on the device, and no follow-up comes after it. Returns `failed_precondition` in Home Assistant mode, while a request runs or a timer alarm rings, while the microphones are off, and until all three slots have passed their check |
| `ListChoices` | `slot`, `connection`, and `kind` (`CHOICE_KIND_MODEL` or `CHOICE_KIND_VOICE`) | What the connection's service lists for the slot now, as `choices` with an `id` (what goes into the field) and a `label`: the brain's models, an OpenAI-compatible server's speech models, ElevenLabs' voices and the models that speak them. At most 500. Empty when the service lists none for the slot. A list that fails returns `failed_precondition` with the check's sentence |
| `GetSatelliteGroup` | Empty | Returns the satellite group as a `SatelliteGroupView`: `on` (the *Satellite group* switch, off by default), `in_group`, `control` (the *Control from the group* switch, on by default), the `members` with their `mac`, `name` and `this_satellite`, and while the switch is on, the `candidates`: the other satellites the search found, except the members of this group, each with its `satellite` as `ListSatellites` returns it, a `state` (`STATE_ADDABLE`, `STATE_IN_ANOTHER_GROUP`, `STATE_OFF_OR_OLDER` or `STATE_LEFT_THE_GROUP`), `addable`, `default_name`, and `name_refusal`, the sentence why its name can't be added. A member counts as outside the group, and is listed in both `members` and `candidates`, only when its record doesn't name this group and it refused this satellite's call as not in this group after the latest search: a record can be older than the member's Add. Its `state` is then `STATE_IN_ANOTHER_GROUP` when its record names another group, and `STATE_LEFT_THE_GROUP` otherwise. The group's key is never returned. Each other member has its `mode` from its last signed call or answer, unset until one came, and `last_seen_unix_ms`, when the search or a signed call or answer last heard it since `pinnad` started, 0 until then. Each other member has `has_password`, from its last signed call or answer, unset until one came |
| `SetSatelliteGroup` | Any of `on` and `control` (bool) | Turns the switches on or off and returns the `SatelliteGroupView`. Turning `on` off leaves the group: the other members the search lists are told first, then this satellite forgets the group |
| `AddGroupMember` | `mac` (a candidate's `satellite.mac`) | Adds a candidate that the search lists as addable to the group, or starts a group of the two, and returns the `SatelliteGroupView`. The satellite added takes the group first, and only while its *Satellite group* switch is on and it is in no group; then this satellite saves it, and the other members are told. If this satellite can't save it, the satellite added is told to forget the group. A refusal is `failed_precondition` with the reason: the switch is off on either satellite, the satellite is in another group, its name can't be added (`name_refusal`), it doesn't answer, or it is adding a satellite itself. Returns `aborted` when this satellite's group changed meanwhile, `invalid_argument` when the satellite's entry in the member list can't change any more, and `unavailable` while the satellite group isn't running |
| `RemoveGroupMember` | `mac` (a member's `mac`, not this satellite's) | Removes the member and returns the `SatelliteGroupView`. Every member the search lists is told, the removed one too, so it forgets the group. A group left with one member ends. To take this satellite out, turn the *Satellite group* switch off: its own `mac` returns `invalid_argument`, as does a member whose entry can't change any more. Returns `failed_precondition` when this satellite is in no group or the satellite isn't a member, and `unavailable` while the satellite group isn't running |

### Satellite group

`pinna.v1.PeerService`, defined in
[`proto/pinna/v1/peer.proto`](https://github.com/elsbrock/pinna/blob/main/proto/pinna/v1/peer.proto),
is how the satellites of a group call each other, by IP address,
on the dashboard's port. Browsers and scripts don't need it. `Join` hands a
satellite the group's id, key and member list during `AddGroupMember`. Every
other call and answer is signed with the group's key: `GetGroup` returns the
member list, and `PutGroup` hands one over. A call with a wrong signature
returns `unauthenticated`, a call for a group this satellite isn't in returns
`failed_precondition`, and a caller that isn't a member returns
`permission_denied`.

`Deliver` hands the satellite a message to say in its own voice, such as
"Message from Kitchen: dinner is ready.". Its signed answer, a
`PeerDelivery`, comes once the satellite has taken the message, before it
plays: `OUTCOME_PLAYED` (it plays now), `OUTCOME_HELD` (it plays once the
satellite is idle and the microphones are on) or `OUTCOME_REFUSED` with a
`reason`, one sentence. The satellite refuses a message in Home Assistant
mode or while it waits to switch to it, with `group_messages` off, before its
text-to-speech has passed its check, without a working speaker, when the text
is empty or longer than 500 characters, and when it is busy: its queue is
nearly full, or it doesn't take the message within 2 seconds. A message it
didn't take in time never plays later.

`Call` has the satellite run one of its device tools for another member's
brain, as a `PeerToolCall` with the tool's name and its arguments as a JSON
object. Its signed answer, a `PeerToolResult`, is the tool's text and whether
it failed. The tools are the volume (`set_volume`, `change_volume`), the reply
volume, the ring light, the media player (`play_url`, `pause_media`,
`resume_media`, `stop_media`), radio (`play_station`, `list_stations`),
timers (`set_timer`, `list_timers`, `cancel_timer`) and `cancel_request`. A
tool runs as an [MCP](#mcp-server) call would: a tool switched off with
`UpdateAssistant` is refused, and the call counts among the two that run at
once. A call that takes longer than 7 seconds ends with an error, though what
it started may still happen. The member that called waits 9.5 seconds in all,
so the answer reaches it in time. The 7 seconds count from the call's arrival.
For a call from a member it doesn't list yet, the satellite first fetches that
member's list. When that takes all 7 seconds, the tool doesn't run.
`cancel_request` from another member also stops a
message or `speak` that plays, and drops the message. The satellite refuses,
as a result with `is_error` set and one sentence, in Home Assistant mode, with
`control` off in `SetSatelliteGroup`, for any other tool, and for arguments
that name a `satellite`: it never passes a call on.

After each search for the other satellites, which runs once a minute, a member
asks every other member that the search lists for its member list, and merges
them. A member that the others' lists mark removed forgets the group. When
every other member has answered two searches in a row outside the group and
refused its `GetGroup`, it leaves the group too. A call from another satellite
doesn't keep a new Wi-Fi network on trial: only opening the dashboard does.

## MCP server

In standalone mode, `pinnad` also serves an MCP server at `/mcp`, on port 80
with the dashboard, so that an AI agent can use the device. Without a
dashboard password it has no authentication, like the dashboard; with one, it
needs the agent token in an `Authorization: Bearer` header. It answers only to
the same names.

- It speaks MCP 2025-06-18 and 2025-03-26, as JSON over HTTP `POST`: no event
  stream, and no batches. A request must be `application/json`, at most
  64 KiB. A request from a web page must come from a name the dashboard
  answers to.
- In Home Assistant mode, every call gets an error that says MCP works in
  standalone mode only.
- Its tools are the device's own, as `device_tools` lists them with `mcp` set:
  volume, the ring light, media and radio, the notification, muting the
  microphones, switching back to Home Assistant mode (`switch_assistant`),
  timers and the time, `updates` and `check_for_updates`, and `speak`,
  `cancel_request` and `status`. `install_update` is for the brain only and
  isn't offered over MCP. A tool switched off with `UpdateAssistant` isn't
  offered. No tool turns the microphones on or makes the device listen.
- It runs two tool calls at once, those from the other members of a
  [satellite group](#satellite-group) included. Another call is told the
  device is busy.

## What's next

- [Set up standalone mode](/docs/standalone/)
- [Home Assistant entities](/docs/entities/)
- [Troubleshooting](/docs/troubleshooting/)
