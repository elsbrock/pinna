---
title: Build Pinna
description: Build the boot image and the signed root filesystem image that you install on your device.
---

This page shows how to build the two files you install on an Echo (2nd gen)
or an Echo Dot (2nd gen): the boot image and the root filesystem image.
[Releases](https://github.com/elsbrock/pinna/releases) carry prebuilt ones;
build your own to develop Pinna or to sign with a key of your own.

## Before you begin

- Use an x86_64 Linux computer. [Nix](https://nixos.org/download/) with
  flakes enabled is the easiest way to get the tools; without it, install the
  tools that `flake.nix` lists yourself. The build needs network access to
  download the toolchain, the Debian packages and the kernel source.

## Get the source and the tools

1. Clone the repository:

   ```sh
   git clone https://github.com/elsbrock/pinna.git
   cd pinna
   ```

2. Start the development shell. It provides the Rust toolchain, the ARM cross
   compilers, mkosi, QEMU and fastboot:

   ```sh
   nix develop
   ```

   The first time, it downloads the tools and takes a few minutes. Without
   Nix, skip this step and install the tools that `flake.nix` lists from your
   distribution.

Run all of the following commands with the development shell's tools, or your
own equivalents.

## Create a signing key

Pinna installs and boots only root filesystem images that are signed with the
release key. Releases are signed with the project key in `keys/release.pub`.
To sign your own images with your own key, run once:

```sh
just keygen
```

The command writes two files:

- `secrets/release.key`: the private key. Git ignores it. Keep it safe and
  make a backup: you need the same key to build updates for devices that run
  your images.
- `secrets/release.pub`: the public key. The build compiles it in instead of
  the project key `keys/release.pub` when it exists, so a device accepts only
  images signed with your private key. Such a build starts with **Updates**
  off, because the project's releases don't install on it. See
  [Offer your own builds as updates](/docs/releases/#offer-your-own-builds-as-updates).

If `secrets/release.key` already exists, `just keygen` keeps it and only
writes the public key again.

Installing your own build on a device that runs a release is a key change:
flash the root filesystem and the boot image together. `pinnactl update`
cannot cross a key change.

If you lose the private key, devices that run your images can't accept updates
any more. To recover, build with a new key and install both the boot image and
the root filesystem again from fastboot.

## Add your own files

1. Put your own files into `secrets/`. Git ignores the folder, so they never
   end up in the repository.

   | File | What it's for | How to create it |
   | --- | --- | --- |
   | `secrets/release.key` | Signs your own root filesystem images. Only to sign your own builds | `just keygen` (writes `secrets/release.pub` too; the build compiles it in) |
   | `secrets/machine-id` | Not needed: each device derives its machine id from its serial number | — |
   | `secrets/authorized_keys` | Your SSH public key. Without it, you can't log in over SSH | Copy your public key, for example `~/.ssh/id_ed25519.pub` |
   | `secrets/wifi/wpa_supplicant-wlan0.conf` | Your Wi-Fi network, so the device connects on first boot. Optional: you can also set it up on the dashboard over USB | See [Connect to Wi-Fi](/docs/flash/#connect-to-wi-fi) |
   | `secrets/device/pinnad.toml` | Developer settings for this device. Optional: owners set the device up on the dashboard | See [Developer settings](/docs/configure/) |
   | `secrets/device/esphome-key` | A fixed encryption key for the Home Assistant connection. Optional: Home Assistant sets a key itself | See [Developer settings](/docs/configure/#a-fixed-encryption-key) |

2. Download the wake word models:

   ```sh
   just models
   ```

   This puts the openWakeWord models into `blobs/models/`.

The build stops if the signing key is missing. For the other files, it
prints a warning and continues. Read the warnings: without `authorized_keys`
you can't log in over SSH.

## Wake word models

Pinna ships the openWakeWord models for **Alexa**, **Hey Jarvis**, **Hey
Mycroft** and **Hey Rhasspy** (v0.5.1), and **Okay Nabu** from
[`rhasspy/pyopen-wakeword`](https://github.com/rhasspy/pyopen-wakeword), pinned
to a commit. `just models` downloads them during the build; they aren't part of
the repository. Release images include them with a `NOTICE` next to them that
names their sources and licenses. The openWakeWord models are licensed under
CC BY-NC-SA 4.0, which doesn't allow commercial use. Okay Nabu comes from a
repository under Apache-2.0; the model itself carries no licence of its own.

## Amazon's firmware

The device needs Amazon's Wi-Fi and Bluetooth firmware and the bitstream for
its microphone FPGA. They aren't part of the repository. `just kernel` and
`just image` run `just firmware`, which downloads them into `blobs/firmware/`
and checks their sha256. Pinna doesn't modify them, and its license doesn't cover them.
It also downloads the Wi-Fi firmware of the Echo Dot (3rd gen) into
`blobs/firmware/donut/`, for its planned port.

## Build the images

To build everything that you need to install Pinna, run:

```sh
just all
```

`just all` builds the kernel, the root filesystem and the boot image, and then
lists the two files:

| File | Contents |
| --- | --- |
| `build/pinna-boot.img` | The kernel, the device tree, and a small initramfs that checks and mounts the root filesystem |
| `build/pinna.root.img` | The root filesystem: Debian with Pinna, read-only and signed, so that the device can detect any change to it |

The first build takes longest, because it downloads and compiles the kernel.

### Rebuild one part

You don't need to rebuild everything after each change:

| Command | Builds | Output |
| --- | --- | --- |
| `just kernel` | The kernel, as a Debian package | `build/debs/linux-image-*.deb` |
| `just image` | The root filesystem, with Pinna's programs | `build/pinna.root.img` |
| `just bootimg` | The boot image | `build/pinna-boot.img` |

`just bootimg` uses the kernel from the last `just kernel`. After you change
the kernel, run `just kernel` before `just bootimg`.

## Check the build (optional)

To check that the image boots before you install it, boot it in a virtual
machine. This needs a kernel built for QEMU:

```sh
just kernel-qemu
just vm-test
```

The test boots the root image you built, then checks that the system services,
SSH and the Home Assistant API come up. It takes a few minutes. For the other
tests, see [Contribute](/docs/development/).

## What's next

[Install Pinna](/docs/flash/). Later, to build an update for
a device that already runs Pinna, see [Update Pinna](/docs/update/).
