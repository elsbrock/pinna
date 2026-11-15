# Pinna

An open, local-first voice satellite OS for the Amazon Echo (2nd gen) family,
starting with the **Echo Dot 2nd gen** (MediaTek MT8163).

Pinna turns the device into a Home Assistant voice satellite with on-device wake
word, LED ring feedback and button control. It is a modern image-based Linux
system: Debian userland built with mkosi, systemd, a read-only erofs root,
and small Rust daemons.

> Status: early bring-up. Console only. Expect sharp edges.

## Layout

| Path | What |
|---|---|
| `crates/` | Rust workspace: `pinnad` (device daemon), ESPHome API, wake word, HAL, audio, boot image tooling |
| `kernel/` | Kernel config fragment and build (packaged as a Debian `.deb`) |
| `mkosi/` | Debian trixie image definition |
| `docs/` | Decision records and runbooks |
| `secrets/`, `blobs/` | Owner-local inputs (credentials, vendor firmware, models) — never committed |

## Development

```sh
nix develop      # or install: rustup, just, mkosi, cargo-zigbuild, qemu
just             # list tasks
just all         # kernel, root filesystem, boot image for the Echo Dot
just test        # everything: unit/lint, ESPHome conformance, wake word parity,
                 # end-to-end voice flow, armhf + x86_64 VM boot gates
```

See `docs/` for the platform design and the first-flash runbook.
