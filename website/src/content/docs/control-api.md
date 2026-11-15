---
title: Control API
description: pinnad's local ConnectRPC service on port 7470, and pinnactl, the console client that drives it.
---

Besides the ESPHome API (which faces Home Assistant), the `pinnad` daemon
serves a small **ConnectRPC** service for local control and diagnostics.

- Service: `pinna.v1.DeviceService` (protos in `proto/pinna/v1/device.proto`)
- Endpoint: `http://127.0.0.1:7470`
- Authentication: none — it is bound to loopback only. Anything that can reach
  the loopback interface can control the device; treat it like a local socket,
  not like an API to expose.

## pinnactl

`pinnactl` is the console client, shipped in the image and also usable from a
dev machine against a VM or a device via SSH:

```sh
pinnactl status              # state: phase, mute, volume, HA connection, timers
pinnactl status --json       # same, machine-readable
pinnactl volume 40           # speaker volume, 0–100
pinnactl mute on             # software microphone mute (on|off)
pinnactl listen              # start a conversation, like the action button
pinnactl stop                # stop conversation, announcement or timer alarm
pinnactl led-test            # short LED ring animation
pinnactl watch               # status again on every change
```

The API endpoint can be overridden for tests and VMs:

```sh
pinnactl --api http://10.42.0.1:7470 status
```

## RPCs

| RPC | Purpose |
| --- | --- |
| `GetStatus` | Version, node name, MAC, uptime, phase, mute sources, volume, HA connection, active timers, wake word state |
| `SetVolume` | Speaker volume, 0–100 |
| `SetMute` | Software mute (the hardware latch is read-only and reported separately) |
| `StartConversation` | Equivalent to pressing the action button |
| `StopConversation` | Stops the current conversation, announcement or timer alarm |
| `LedTest` | Runs a short LED animation cycle |
| `WatchStatus` | Server-streaming status: current state, then again on every change |

## Where it is used

- The image's **boot health check** — `pinna-boot-ok.service` asks `pinnactl`
  whether the daemon is actually up before the fastboot guard is cleared.
- Scripts and diagnostics on the device (`pinnactl watch` is a good "is
  anything happening?" companion to `journalctl`).
- A future web UI: the protos are ready to be used from a browser with
  `connect-es`, so a small Svelte front end can reuse the same service.
