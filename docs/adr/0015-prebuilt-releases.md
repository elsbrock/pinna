# ADR 0015: Prebuilt releases, signed with the project key, without owner data

Status: proposed (2026-11-15). Amends ADR 0003 (who holds the signing key, and
which public key a build compiles in) and ADR 0010 (SSH keys are no longer
baked into release images). Points marked *default* are not confirmed by the
owner yet.

## Context

- Installing Pinna means building it first: x86_64 Linux, Nix, a kernel build,
  mkosi and a signing key of one's own. Most owners of an Echo Dot stop there.
  Installing from the browser, a later goal, needs images that someone else
  built.
- Every image is personal today:
  - it is signed with the builder's key, and `just keygen` rewrites
    `keys/release.pub` with it (ADR 0003);
  - it carries the builder's machine id (`secrets/machine-id`) and SSH keys
    (`secrets/authorized_keys`);
  - it may carry Wi-Fi, the node name and the Home Assistant API key as seeds
    for `/var`.
- One image on many Dots would give them one machine id. systemd-networkd
  derives the DHCP client ID from it, so two Dots on one network would ask for
  the same lease.
- Each Dot has a serial number in the bootloader's idme record. On 6.12 it is
  in the device tree (`/sys/firmware/devicetree/base/idme/serial/value`); on
  6.1 in `/proc/idme/serial`. The USB gadget and rescue mode take it as the
  device's identity (ADR 0012).
- Wi-Fi, the name and the API key already move to the dashboard (ADRs
  0007–0009). SSH keys stay baked into the image (ADR 0010).
- The openWakeWord models are CC BY-NC-SA 4.0. The build fetches them; the
  repository never holds them.

## Decision

### The project key

- `keys/release.pub` is the project's key. It is the key it holds today, so
  Dots that run it need no new boot image. `just keygen` no longer rewrites it.
  *(default)*
- Its secret half is a GitHub Actions secret, `PINNA_RELEASE_KEY`, in a
  protected `release` environment. Only a `v*` tag runs the job that can read
  it. *(default)*
- A developer without that key runs `just keygen`. It writes
  `secrets/release.key` and `secrets/release.pub`. The build compiles in
  `secrets/release.pub` when it exists, else `keys/release.pub`. Flashing such
  a build onto a Dot that runs a release is a key change (ADR 0003): root and
  boot image together.
- `pinna-image seal` refuses a key whose public half is not the compiled-in
  key, so no build seals a root that its own boot image rejects.

### Release builds

- On a `v*` tag, `.github/workflows/release.yml` builds the kernel, the
  models, the root and the boot image, with the version from the tag. It
  uploads to a draft GitHub release:
  - `pinna-<version>.root.img`
  - `pinna-boot.img`
  - `manifest.json`
  - `SHA256SUMS`
  - `flash-gen2.sh`

  The owner publishes the draft. *(default: GitHub Releases. The browser
  installer needs a host that allows cross-origin downloads; that is decided
  with it.)*
- Started by hand (`workflow_dispatch`), the workflow builds the same with a
  throwaway key and keeps the files as workflow artifacts. They are for
  testing the workflow, not for devices.
- `PINNA_RELEASE=1` makes the build fail when `secrets/` holds anything of an
  owner's besides the signing key: `authorized_keys`, `machine-id`, `wifi/`,
  `device/`. It also fails without the wake word models.
- `flash-gen2.sh` checks an image against the `SHA256SUMS` next to it, when
  there is one. Flashing a release then needs `fastboot`, bash and coreutils,
  not Rust or Nix. The device still checks the signature (pinna-init).
- The release's `manifest.json` is an update manifest (ADR 0003):
  `pinnactl update https://github.com/elsbrock/pinna/releases/latest/download/manifest.json`
  installs the newest published release.

### No owner data in the image

- **Machine id.** pinna-init derives it from the idme serial number: the
  first 16 bytes of sha256 over a fixed prefix and the serial, as a version 4
  UUID. It writes it to `/run/pinna/machine-id` and bind-mounts that over
  `/etc/machine-id` in the root. Without a serial number (the VMs), the
  image's machine id stays. A failure is logged and never stops the boot.
  `secrets/machine-id` becomes optional; without it, mkosi writes
  `uninitialized`. *(default: the idme serial, the same identity the gadget
  uses, rather than the eMMC CID)*
- **SSH keys.** sshd also reads `/var/lib/pinna/ssh/authorized_keys`. Release
  images carry no key. An owner adds one there from the USB serial console,
  which logs in as root already; a later installer writes it. Builds with
  `secrets/authorized_keys` keep baking it in. *(default)*
- **Wi-Fi, name and API key** are not in release images. They come from the
  dashboard (ADRs 0007–0009). Builds with `secrets/` keep seeding them as
  today.
- **Wake word models.** Release images carry the openWakeWord v0.5.1 `.onnx`
  models, unchanged, with a `NOTICE` that names their source and licence.
  Releases are free of charge, which the non-commercial term requires.
  *(default)* The `.tflite` models of ADR 0013 are not decided here.

## Consequences

- Installing no longer needs Nix, Rust or a kernel build: download, back up,
  flash. Building stays the way to develop Pinna.
- Whoever can push a `v*` tag and pass the `release` environment's review can
  sign images that every Dot with a release accepts. The environment's
  required reviewer is that gate.
- A leaked key can't be revoked on devices: pinna-init has it compiled in.
  Replacing it means a new boot image on every Dot, by fastboot (ADR 0003).
- On a Dot that already runs Pinna, the first boot with the new pinna-init
  changes the machine id:
  - DHCP may hand out a new address;
  - the journal of earlier boots stays under the old id (`journalctl -m`).
  The ESPHome node name and the SSH host key don't depend on it.
- A factory reset (ADR 0010) also removes SSH keys added under `/var`.
- Open: release images keep the time zone the image sets (Europe/Berlin).
- The browser installer builds on these releases. It gets its own record.
