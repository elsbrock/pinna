---
title: Build Pinna
description: Build the boot image and the signed root filesystem image that you install on the Echo Dot.
---

This page shows how to build the two files you install on an Echo Dot (2nd
gen): the boot image and the root filesystem image. Pinna has no prebuilt
images yet, so you build your own.

## Before you begin

- Use an x86_64 Linux computer with [Nix](https://nixos.org/download/) and
  flakes enabled. The build needs network access to download the toolchain,
  the Debian packages and the kernel source.
- Collect the files from your device that are listed in
  [What you need](/docs/requirements/). You put them into the repository in
  this guide.

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

   The first time, it downloads the tools and takes a few minutes.

Run all of the following commands inside the development shell.

## Create a signing key

Pinna installs and boots only root filesystem images that are signed with the
release key. Create your own key once:

```sh
just keygen
```

The command writes two files:

- `secrets/release.key`: the private key. Git ignores it. Keep it safe and
  make a backup: you need the same key to build updates for devices that run
  your images.
- `keys/release.pub`: the public key. It is built into the boot image and into
  `pinnactl`, so a device accepts only images signed with your private key.

If `secrets/release.key` already exists, `just keygen` keeps it and only
writes the public key again.

If you lose the private key, devices that run your images can't accept updates
any more. To recover, build with a new key and install both the boot image and
the root filesystem again from fastboot.

## Add your device files

1. Copy the files from your device into the repository, as described in
   [What you need](/docs/requirements/):

   | Path | Contents |
   | --- | --- |
   | `blobs/firmware/` | Wi-Fi and Bluetooth firmware from your backup |
   | `secrets/machine-id` | A machine ID for the image |
   | `secrets/authorized_keys` | Your SSH public key, for logging in to the device |
   | `secrets/wifi/wpa_supplicant-wlan0.conf` | Your Wi-Fi network (optional) |

2. Download the wake word models:

   ```sh
   just models
   ```

   This puts the openWakeWord models into `blobs/models/`.

The build stops if the signing key is missing. For the other files, it
prints a warning and continues. Read the warnings: without
Wi-Fi firmware the device has no Wi-Fi, and without `authorized_keys` you
can't log in over SSH.

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

[Install Pinna on your Echo Dot](/docs/flash/). Later, to build an update for
a device that already runs Pinna, see [Update Pinna](/docs/update/).
