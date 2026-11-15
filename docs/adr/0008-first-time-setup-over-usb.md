# ADR 0008: First-time setup in the dashboard, over USB

Status: proposed (2026-11-15). Needs ADR 0006 and ADR 0007; it is built after both.

## Context

- A satellite flashed without `secrets/wifi/` is reachable over USB only: NCM at `10.42.0.1`, a serial console, and SSH. The dashboard already answers there.
- Installing still needs a terminal (unlock, build, fastboot), so today whoever sets up a satellite is already at one. The owner wants installation from the web someday, with prebuilt images and a browser installer. That is a separate, later goal, and it would end in this setup.

## Decision

- When the satellite has no saved Wi-Fi network, the dashboard opens on a setup flow instead of the Overview:
  1. Name the satellite (ADR 0006).
  2. Join a Wi-Fi network (ADR 0007). The page then says the new address.
  3. Say how Home Assistant finds it.
- The USB link is the setup channel. No access point mode: it adds a second network stack and a captive portal for a step that already has a cable.
- The flow can be left at any point. Every step is also in its normal section.

## Consequences

- A satellite works without `secrets/wifi/` in the image, so one image can serve several networks.
- The flow is the end point for a future browser installer.
