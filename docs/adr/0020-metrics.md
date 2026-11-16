# ADR 0020: pinnad serves Prometheus metrics on the dashboard listener

Status: proposed (2026-11-16). Amends ADR 0005 (what the web listener serves
and what anyone on the LAN can read).

## Context

- Health questions about a satellite (is it short of memory, did a bridge get
  OOM-killed, is the Wi-Fi weak or dropping packets, does the speaker
  underrun, how long do replies take) have needed SSH and a look at the
  journal. A release-image owner has no SSH key (ADR 0015).
- These metrics are for owners who already run Prometheus, or something that
  scrapes its format (VictoriaMetrics, Grafana Alloy, Telegraf). Most Home
  Assistant users don't. For them the follow-up is diagnostic sensors over the
  ESPHome API (signal, uptime, temperature); pinna-esphome has no numeric
  sensor yet.
- About 480 MB of RAM and a read-only, signed root (ADR 0001). The stock
  `prometheus-node-exporter` is a Go binary of about 20 MB on disk and a second
  port to scrape.

## Decision

- **`GET /metrics` on the web listener** (ADR 0005), in the Prometheus text
  format 0.0.4. It has the dashboard's Host allowlist and no authentication.
  It is off when the listener is off (`[web] listen = ""`); there is no switch
  of its own.
- **pinnad reads the host itself**, from /proc and /sys, when scraped, with the
  `procfs` and `prometheus-client` crates. Nothing new goes into the image.
  The reads run off the async workers, which also run the voice path.
- **Host metrics are named and labelled as node_exporter names them**, so
  Grafana's node dashboards work: `node_uname_info`, `node_time_seconds`,
  `node_cpu_seconds_total`, `node_boot_time_seconds`, `node_load*`,
  `node_memory_*_bytes`,
  `node_pressure_*`, `node_filesystem_{size,avail}_bytes` (file systems on
  block devices), `node_network_*_total` (without loopback),
  `node_thermal_zone_temp`. pinnad's own process has the standard
  `process_*` metrics.
- **What node_exporter doesn't name is `pinna_*`**: per systemd service, from
  its cgroup, memory, memory limit, CPU and OOM kills (`pinna_unit_*`); the
  Wi-Fi signal from /proc/net/wireless, left out without a link.
- **Pinna's own state is `pinna_*`**: the running build and slot, how the last
  boot ended, the microphone switches, Home Assistant's connection and voice
  subscription, whether the wake word is on, timers, a ringing alarm,
  AirPlay and Bluetooth state, paired phones. Counted since pinnad started:
  wakes by word, near misses, Assist runs and errors, speaker underruns, and a
  histogram of the time from the end of speech to the reply's first audio.
- **No voice phase, no volume.** They change faster than a scrape interval
  sees, say nothing about health, and once stored in a time series they become
  a record of who was home and when. For the same reason nothing that follows
  the phase (the detector's armed state, which drops during a request) is
  shown: the wake word metric is the setting.
- **A source that can't be read is left out**, metric by metric; the scrape
  still answers 200. The build's `slot` label is empty where pinna-init
  recorded none. A VM without thermal zones, a kernel without pressure
  stall information, a satellite without Wi-Fi each just lack those series.
- **Names.** The `node_*` names follow node_exporter and don't change. The
  `pinna_*` names may change before 1.0; a release that changes one says so.

## Consequences

- Anyone on the LAN can also read the metrics. The host's load, memory,
  traffic, temperatures, file systems and the services' resource use were not
  readable from the LAN before. How often the wake word fired and requests
  ran is less than the status the dashboard already streams (the phase,
  recent wakes with their times); storing the counts over time is what is
  new.
- A scrape costs a few dozen small file reads in pinnad.
- Adding a host metric means reading it in pinnad, not installing an exporter.
