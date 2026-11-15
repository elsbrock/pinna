---
title: Build
description: Prerequisites and the just tasks that produce the kernel, the erofs root filesystem and the boot image.
---

Pinna builds on any Linux with the dev shell: a Nix flake provides Rust, `just`,
`mkosi`, `cargo-zigbuild`, QEMU and the cross toolchain. Everything else is
routine.

## Prerequisites

- An Echo Dot (2nd gen) already unlocked with Kaeru/amonet. Pinna is installed
  *next to* the previous firmware, over fastboot.
- Owner-local inputs that are never committed to the repository:
  - `blobs/` — Wi-Fi/BT firmware extracted from your own device backup, the
    known-good DTB, and the wake word models (`just models` fetches those;
    they are CC BY-NC-SA and stay out of the repo).
  - `secrets/machine-id` — a machine-id for the image build.

## Build everything

```sh
git clone https://github.com/elsbrock/pinna
cd pinna
nix develop

just              # list every task
just kernel models image bootimg
```

The individual steps, in case you are iterating:

| Task | What it produces |
| --- | --- |
| `just kernel` | Kernel built from LibreEcho-Linux + patches, packaged as `build/debs/linux-image-*.deb` |
| `just models` | openWakeWord models into `blobs/models/` (not committed) |
| `just image` | Debian trixie armhf root filesystem (erofs) via mkosi: `mkosi/mkosi.output/pinna.root-arm.raw` |
| `just bootimg` | `build/pinna-boot.img` — kernel + known-good DTB, no initramfs |
| `just all` | All of the above, ending with a list of the two files you flash |

## Test builds without hardware

The same userland builds for x86_64 and boots under QEMU/KVM, which is what CI
and `just test` use:

```sh
just kernel-qemu image        # armhf virt kernel + image
just kernel-x86  image-x86    # x86_64 KVM variant
just vm-test vm-test-x86      # boot them and check the system comes up
```

Next: [flash the device](/docs/flash/).
