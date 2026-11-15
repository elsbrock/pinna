---
title: Contribute
description: Set up a development environment, run the tests, try changes on a device, and send them upstream.
---

This page explains how to work on Pinna: how to set up the tools, which tests
exist and what each one needs, and how to try a change on a running Echo Dot.

Pinna is a small project in early development. Before you start on something
larger than a fix, open an issue so that the work isn't duplicated.

## Set up the tools

Pinna builds on an x86_64 Linux computer with [Nix](https://nixos.org/download/)
and flakes enabled. The repository's development shell provides every tool:
the Rust toolchain, the ARM cross compilers, mkosi, QEMU, fastboot and
[uv](https://docs.astral.sh/uv/) for the Python-based tests.

1. Clone the repository:

   ```sh
   git clone https://github.com/elsbrock/pinna.git
   cd pinna
   ```

2. Start the development shell:

   ```sh
   nix develop
   ```

3. List the available tasks:

   ```sh
   just
   ```

Building images also needs a signing key and the files described in
[What you need](/docs/requirements/). The tests in this section don't, unless
the table says so.

## Run the checks

Run the host checks on every change. They format-check the code, run Clippy
with warnings as errors, and run the unit tests of every crate:

```sh
just check
```

To run everything, including the virtual machine tests, run:

```sh
just test
```

`just test` takes a while, and some of its parts need files that you build
first. The repository has no CI that runs these tests, so run them yourself
before you send a change that affects the image or the device.

## Tests and what they need

| Task | What it checks | What it needs |
| --- | --- | --- |
| `just check` | Formatting, Clippy and the unit tests of every crate | Nothing else |
| `just esphome-conformance` | Home Assistant's own client library (`aioesphomeapi` 46.2.0) talks to Pinna's ESPHome API server: the entity set, encryption, key rotation and error cases | Network access the first time, to download the Python packages |
| `just wakeword-parity` | Pinna's wake word scores match the reference implementation (onnxruntime) on test recordings | The wake word models (`just models`); skips without them |
| `just e2e` | A complete voice request: `pinnad` with the real wake word and a recording, with a simulated Home Assistant | The wake word models (`just models`); skips without them |
| `just kernel-config` | The kernel configuration for the device, QEMU and x86_64 contains every option Pinna needs | Network access the first time, to download the kernel source |
| `just bootimg-roundtrip` | The boot image unpacks and repacks byte for byte | `build/pinna-boot.img` from `just bootimg` |
| `just vm-test` | The real ARM root image boots in QEMU, and SSH and the Home Assistant API come up | `just kernel-qemu image` first. Takes a few minutes, because QEMU emulates the CPU |
| `just vm-test-x86` | The same checks with the x86_64 image | `just kernel-x86 image-x86` first. Faster with KVM |
| `just vm-update-test` | An update installs into the other slot, the trial boot succeeds, and a broken update rolls back | `just kernel-qemu image` and the signing key. Builds two more images, so it takes longer |
| `just test-device` | Speaker, microphones, LED ring and the Home Assistant API on a real device | A running Echo Dot reachable over SSH |

`just wakeword-parity` and `just e2e` pass without running anything if the
wake word models are missing. Run `just models` before you rely on their
result.

## Try a change on a device

To test new versions of Pinna's programs on a running Echo Dot without
reinstalling it, use `just push`:

```sh
just push
```

`just push` cross-compiles `pinnad`, `pinnactl` and `pinna-bootctl`, copies
them to the device over SSH as a system extension in
`/var/lib/extensions/pinna-dev`, and restarts `pinnad`. The root filesystem
stays read-only and unchanged.

By default it connects to `root@pinna.local`. To use another address, set
`PINNA_HOST`:

```sh
PINNA_HOST=root@192.168.1.40 just push
```

To remove the extension and go back to the installed programs, run:

```sh
scripts/push-dev.sh --remove
```

## Work on one part

Most work happens in one crate. Some useful commands:

```sh
cargo test -p pinna-wakeword                      # tests of one crate
cargo run -p pinna-bootimg -- info build/pinna-boot.img
```

To find your way around the code, see [Repository layout](/docs/crates/). For
the reasons behind the design, read the decision records in
[`docs/adr/`](https://github.com/elsbrock/pinna/tree/main/docs/adr).

## Send a change

1. Open an issue that describes the problem or the feature.
2. Make the change on a branch. Write commit messages in the
   [Conventional Commits](https://www.conventionalcommits.org/) style that the
   history uses, for example `fix(pinnad): …` or `feat(image): …`.
3. Run `just check`, and the tests from the table that cover what you changed.
4. Open a pull request on [GitHub](https://github.com/elsbrock/pinna) and say
   which tests you ran.

## Licences

Pinna's own code is under the MIT licence. The Linux kernel is under the GNU
GPL version 2. The wake word models that `just models` downloads are under
CC BY-NC-SA 4.0, which doesn't allow commercial use. They are never committed
to the repository. If you build an image with them, the image contains them,
so keep that licence in mind before you share it.

## What's next

- [Build Pinna](/docs/build/) to get images you can install.
- Read about the [architecture](/docs/architecture/).
