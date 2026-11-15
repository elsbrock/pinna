---
title: Home Assistant
description: How Home Assistant discovers Pinna, which entities it exposes, and how ESPHome API encryption works.
---

Pinna speaks the **ESPHome native API**. That is the whole integration story:
Home Assistant treats the Echo Dot like any other ESPHome device — discovery,
adoption, entities, voice — with no YAML, no custom component and no cloud.

## Discovery and adoption

The device advertises `_esphomelib._tcp` over mDNS (via systemd-resolved).
Home Assistant shows it as a discovered device; the project name is
`pinna.satellite` and the device is named after the hardware, e.g. **"Echo Dot
(2nd gen)"**.

Adopt it, then configure a **voice assistant pipeline** for the device in Home
Assistant: speech-to-text, conversation agent and text-to-speech are your
choice. Say the wake word and the pipeline runs.

## Entities

The entity set matches the previous LibreEcho firmware — names are part of the
unique ID — so dashboards and automations keep working after the switch:

| Entity | Type | Notes |
| --- | --- | --- |
| `led_ring` | `light` | Effects: Solid, Pulse, Flash |
| `wake_word` | `switch` | Turn wake word detection on/off |
| `mute` | `switch` | Microphone mute, mirrors the hardware latch |
| `volume` | `number` | 0–100 |
| `reply_volume` | `number` | Volume for spoken replies, 0–100 |
| `wake_word_sensitivity` | `number` | Maps to the detector threshold |
| `led_brightness` | `number` | 0–100 |
| `cancel_request` | `button` | Stops playback and the current request |
| `voice_assistant` | `event` | `wake_word`, `listening`, `thinking`, `replying`, `finished`, `stopped` |
| `playback` | `event` | `started`, `stopped` |
| `timer` | `event` | `started`, `updated`, `cancelled`, `finished` |
| `volume_up` / `volume_down` / `microphone_mute` / `action` | `event` | `press`, `long_press`, `double_press` |
| `last_wake_word` | `text_sensor` | e.g. `alexa` |
| `media_player` | `media_player` | Announcements, play/pause, stop, volume |

## Encryption

The API is **plaintext until a key is set**. To encrypt, put a base64 32-byte
key either in the image config:

```toml
# /etc/pinna/pinnad.toml
[esphome]
encryption_key = "…base64 32-byte key…"
```

or on the running device at `/var/lib/pinna/esphome-key`. Give Home Assistant
the same key when it asks. Once connected over Noise (NNpsk0), Home Assistant
can **rotate the key**; the new one is persisted to
`/var/lib/pinna/esphome-key`, which survives root filesystem updates.

## If nothing shows up

- `systemctl status avahi-daemon` is not relevant — discovery runs through
  `systemd-resolved`. Check `resolvectl status wlan0`.
- Watch the API traffic yourself:
  `journalctl -u pinnad -f` shows clients connecting.
- Compare against ESPHome: the API is conformance-tested against
  `aioesphomeapi` 46.2.0 in CI (`just esphome-conformance`).
