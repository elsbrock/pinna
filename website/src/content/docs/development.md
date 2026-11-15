---
title: Development
description: The just tasks, what each test gate covers, and how to iterate without hardware.
---

## The fast loop

```sh
nix develop

just check     # fmt, clippy (-D warnings), workspace tests
just test      # the full gate below
```

`just check` is what you run on every change; `just test` is what CI runs
before a flash is worth attempting.

## What `just test` covers

| Task | What it proves |
| --- | --- |
| `bootimg-roundtrip` | Our boot image (and the known-good LibreEcho one, if present) unpacks and repacks byte-identically |
| `kernel-config` | Config assertions for the device, QEMU and x86_64 variants |
| `esphome-conformance` | `aioesphomeapi` 46.2.0 drives the demo server: entity set, Noise, key rotation, negative cases |
| `wakeword-parity` | tract scores match onnxruntime to 3e-6 on the fixture set |
| `e2e` | Full voice flow: `pinnad` + the real wake word on a WAV fixture, with `aioesphomeapi` playing Home Assistant |
| `vm-test` | Boots the real armhf image under QEMU and checks the system comes up |
| `vm-test-x86` | Same for the x86_64 image under KVM |

The VM gates are the "no hardware on the desk" substitute: the same userland,
the same systemd units, checked end to end.

## Iterating on one thing

```sh
cargo test -p pinna-wakeword                  # unit tests of one crate
cargo run -p pinnactl -- status               # against a running VM/device
just vm-test                                  # needs: just kernel-qemu image
cargo run -p pinna-bootimg -- info build/pinna-boot.img
```

## Owner-local inputs

`secrets/` and `blobs/` hold everything that must not be committed:
credentials, vendor firmware from your own device backup, wake word models
(CC BY-NC-SA, fetched by `just models`) and the machine-id. Builds fail loudly
when one of them is missing — that is intended, not a bug.
