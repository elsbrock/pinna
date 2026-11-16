---
title: Prometheus metrics
description: Scrape the device's host and Pinna metrics with Prometheus from the /metrics address.
---

The device serves metrics in the Prometheus text format at `/metrics`, on the
same port as the [dashboard](/docs/dashboard/). They show how the device is
doing (CPU, memory, temperature, Wi-Fi, its services) and how Pinna is doing
(wakes, requests, reply times, Home Assistant's connection).

Use them if you run Prometheus or a program that reads its format. Home
Assistant doesn't need them.

## Scrape the device

Add a job to `prometheus.yml` with the device's IP address. Reserve that
address for the device in your router, so that it doesn't change:

```yaml
scrape_configs:
  - job_name: pinna
    static_configs:
      - targets: ["192.168.1.40:80"]
```

A `.local` name such as `pinna-2a914e.local` works only if the machine that
runs Prometheus resolves `.local` names. Prometheus's own builds and its
Docker image don't.

To check the address, open `http://192.168.1.40/metrics` in a browser. Then,
in Prometheus, open **Status > Targets**: the `pinna` job shows **UP**.

The device answers only to the names the dashboard answers to, such as its IP
addresses and `.local` names. If you scrape it by another DNS name, add that
name first, as in
[Open the dashboard by another name](/docs/dashboard/#open-the-dashboard-by-another-name),
or the device answers `421 Misdirected Request`.

A scrape every 15 to 60 seconds is fine. The device reads the numbers when
it's scraped.

## Host metrics

The host metrics have the names and labels that node_exporter uses, so
Grafana's node_exporter dashboards, such as
[Node Exporter Full](https://grafana.com/grafana/dashboards/1860), show them.
Node Exporter Full shows uptime, CPU, load, memory, pressure, network and file
systems. Its disk I/O, context switch and other detailed panels stay empty:
the device doesn't serve those metrics.

| Metric | What it is |
| --- | --- |
| `node_uname_info` | Kernel version and host name |
| `node_time_seconds`, `node_boot_time_seconds` | The time now, and when the device started |
| `node_cpu_seconds_total{cpu,mode}` | CPU time per core and mode |
| `node_load1`, `node_load5`, `node_load15` | Load average |
| `node_memory_*_bytes` | `MemTotal`, `MemFree`, `MemAvailable`, `Buffers`, `Cached`, `SwapTotal`, `SwapFree` |
| `node_pressure_*_seconds_total` | Time tasks waited for CPU, memory or storage |
| `node_filesystem_size_bytes`, `node_filesystem_avail_bytes` | Size and free space of each file system on the storage |
| `node_network_{receive,transmit}_{bytes,errs,drop}_total{device}` | Traffic, errors and dropped packets per network interface |
| `node_thermal_zone_temp{zone,type}` | Temperature of each thermal zone, in °C |
| `pinna_wifi_signal_dbm{device}` | Wi-Fi signal level. Missing while not connected |
| `pinna_unit_memory_bytes{unit}` | Memory each system service uses |
| `pinna_unit_memory_max_bytes{unit}` | A service's memory limit, if it has one |
| `pinna_unit_cpu_seconds_total{unit}` | CPU time each service used |
| `pinna_unit_oom_kills_total{unit}` | Times the kernel stopped a service's process because memory ran out: the service's own limit, or the device's |
| `process_*` | Memory, CPU time, threads and open files of `pinnad` itself |

## Pinna metrics

Counters start at 0 when `pinnad` starts. Those with a `word` or `code`
label appear after the first wake or error.

| Metric | What it is |
| --- | --- |
| `pinna_build_info{version,model,slot}` | The running version, the device model and the A/B slot |
| `pinna_last_boot_info{outcome}` | How the boot before this one ended: `clean` (a restart), `cold` (power cut or first start), `crashed` (a kernel crash), `reset` (stopped without shutting down) or `rescue` (ended in rescue mode) |
| `pinna_home_assistant_connected` | 1 while Home Assistant is connected |
| `pinna_voice_subscribed` | 1 while Home Assistant takes voice requests from the device |
| `pinna_wake_word_enabled` | 1 while the wake word is on and at least one wake word is chosen |
| `pinna_microphone_muted{switch}` | 1 while the microphones are off, by the `hardware` button or the `software` switch |
| `pinna_timers{state}` | Timers that are `running` or `paused` |
| `pinna_alarm_ringing` | 1 while a timer alarm rings |
| `pinna_airplay_state{state}`, `pinna_bluetooth_state{state}` | 1 for the current state: `off`, `starting`, `ready`, `failed` or `unavailable` |
| `pinna_bluetooth_paired_devices` | Phones paired over Bluetooth |
| `pinna_wakes_total{word}` | Wake word detections, per wake word ID, such as `alexa_v0.1` |
| `pinna_wake_near_misses_total` | Sounds that almost woke the device |
| `pinna_assist_runs_total` | Voice requests started |
| `pinna_assist_errors_total{code}` | Voice requests that Home Assistant ended with an error, by its error code, such as `stt-no-text-recognized` or `intent-failed`. Other codes count as `other` |
| `pinna_reply_first_audio_seconds` | Histogram of the time from the end of your speech to the first sound of the reply, in buckets from 0.25 to 13 seconds |
| `pinna_speaker_underruns_total` | Times the speaker ran out of sound and restarted, which can click |

For example, wakes in the last hour, and the time 9 replies in 10 take to
start:

```promql
increase(pinna_wakes_total[1h])
histogram_quantile(0.9, sum by (le) (rate(pinna_reply_first_audio_seconds_bucket[1h])))
```

The `pinna_*` names can change before Pinna 1.0. The release notes say when
one does. The `node_*` names follow node_exporter.

## Who can read them

Like the dashboard, `/metrics` has no password. Anyone on your network can
read it, including how often the wake word fired. It doesn't show what was
said, or whether a request is running. It has no switch of its own: it is off
only when the dashboard is off (`[web] listen = ""`, a developer setting, see
[Configure Pinna](/docs/configure/)).
