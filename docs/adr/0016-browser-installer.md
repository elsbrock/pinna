# ADR 0016: Install from the browser, over WebUSB fastboot

Status: proposed (2026-11-16). Builds on ADR 0015 (prebuilt releases) and ends
in ADR 0009 (first-time setup over USB). Points marked *default* are not
confirmed by the owner yet.

## Context

- Installing Pinna needs a terminal: `fastboot`, `adb`, bash and a release
  folder (ADR 0015). A device that was never unlocked first needs the previous
  firmware's terminal installer for the bootloader unlock.
- Every step is USB fastboot, the unlock included. A stock device is unlocked
  with `fastboot flash brick <payload>`. The payload (about 114 MB) comes from
  the community amonet archive for the model. It is chosen by
  `getvar lk_build_desc` and must go out as one raw download, never split into
  sparse chunks. It is 32 bytes over `max-download-size` (0x6d00000). The
  device stops answering while the payload runs, so the reply says nothing
  about the outcome. Afterwards `misc` holds a sticky `FASTBOOT_PLEASE`, so
  every boot ends in fastboot until Kaeru's `reboot-bootloader` replaces it
  with a one-shot request.
- The archives are not redistributable, and their host (XDA) cannot be read by
  a web page. The pinned builds drift: Amazon's updates bring new LK builds,
  which need newer archives (an Echo (2nd gen) build `2c3569d-20260617_051433`
  needs a newer radar archive than the one pinned so far).
- Chromium-based browsers (Chrome, Edge, Brave, Opera) speak USB to a page
  through WebUSB, from a secure origin, after the user picks the device in the
  browser's own dialog. Firefox and Safari do not. Linux needs a udev rule
  that gives the logged-in user the device. Windows needs the WinUSB driver
  bound to the fastboot interface (Zadig).
- Measured on the owner's Echo Dot (2nd gen):
  - Fastboot is USB `0bb4:0c01` (manufacturer "MediaTek", product "Android",
    the device serial as its serial number), one vendor interface
    `ff/42/03`, both stock (2026-08-19) and with Kaeru.
  - Stock, on a Linux laptop (2026-08-19): `getvar` got no reply, and each
    request reset the device through the MediaTek preloader (`0e8d:2000`)
    back into fastboot. USB autosuspend was suspected, not confirmed. The
    unlock was not done in that session.
  - With Kaeru (2026-11-15): `getvar unlocked` answers with an empty value;
    `product` is `BISCUIT`, `max-download-size` 0x6d00000.
- GitHub release downloads send no `Access-Control-Allow-Origin`, so a page on
  the website cannot fetch release files.
- The project has no stock unit. The previous firmware's own browser installer
  only ever ran a read-only query from a browser; its unlock and write steps
  were never run from one.

## Decision

### One page on the website, in stages

- `/install/` on the website. Its code is TypeScript in
  `website/src/lib/installer/`, without new dependencies: Pinna's own fastboot
  protocol and WebUSB transport, Android sparse splitting for images larger
  than `max-download-size`, ZIP reading with the browser's
  `DecompressionStream`, SHA-256 with WebCrypto. The protocol, the splitting and
  the decisions are tested with `node --test` against a scripted device.
- It ships in this order, each stage only once the one before has run on
  hardware:
  1. **Check** (read-only): connect, identify, show what the device is and
     whether this page can install it, and offer a report to paste into an
     issue. Nothing is written. It finds the driver and permission problems
     on users' machines, the stock USB identity, and the LK builds in the
     wild.
  2. **Install on an unlocked device** (Kaeru: the previous firmware or Pinna).
  3. **Backup and restore** (optional for the user, offered before any write).
  4. **Unlock a stock device.**
- Browsers without WebUSB get a notice and a link to the terminal guide.
- The page is linked from the docs once stage 2 runs and ADRs 0007–0009 have
  shipped, so that a user without a terminal can finish the setup. *(default)*

### Files

- The user downloads the release files and hands them to the page. The page
  identifies each file by its SHA-256 against the release's `SHA256SUMS`, not
  by its name, because browsers rename repeated downloads. Root and boot image
  must come from the same `SHA256SUMS`. *(default: no hosting yet)*
- `SHA256SUMS` proves the download is intact, not who made it. The device
  checks the root image's signature (pinna-init); nothing checks the boot
  image, as with the terminal install.

### Identify, then decide

- The user says which model they have. The page reads `product`, `serialno`,
  `lk_build_desc`, `max-download-size` and the partition sizes, and sends
  `oem kaeru-version`: an answer means Kaeru; a refusal (`unknown command`,
  `not allowed`, …) means stock; anything else stops the page.
- The page stops on: a product that does not match the stated model (an Echo
  (2nd gen) on the old boot chain reports `BISCUIT`), a missing partition
  (`boot_a`, `boot_b`, `system_a`, `system_b`, `userdata`), an image that does
  not fit, a request that gets no reply. After a request without a reply it
  sends nothing more until the user reconnects: on the stock Dot above, each
  request reset the device.

### Install

- What `flash-gen2.sh root` and `boot` do, in the same order: `system_a` gets the
  root image (sparse chunks), the first MiB of `system_b` and the first 4 MiB
  of `userdata` are zeroed, `boot_a` and `boot_b` get the boot image. Then
  `reboot`. (Only a device unlocked a moment ago needs `reboot-bootloader`
  first, for the sticky `FASTBOOT_PLEASE`; that belongs to stage 4.)
- An image bigger than `max-download-size` goes as Android sparse images that
  each carry their blocks raw and skip the rest, as the fastboot tool sends
  it.
- Right before the first write the page reads the device again; another
  serial number, or a result other than "ready", stops it.
- One confirmation before the first write says what is erased, and that the
  device stays in fastboot until the page has finished.
- A disconnect or a closed tab during the writes leaves the device in fastboot,
  with Kaeru untouched. The page then starts again from the check.
- The page ends with a link to `http://10.42.0.1`. A page served over HTTPS
  cannot fetch it, only open it.

### Backup and restore

- Offered, not required. It runs after the unlock (stock LK cannot boot
  anything unsigned) and before Pinna's first write. It saves `boot_a`,
  `boot_b`, `system_a`, `system_b` and `userdata` with a `SHA256SUMS`, and
  restore writes them back over fastboot, like `flash-gen2.sh restore`.
- Fastboot can't read them: Kaeru registers no command that reads a partition
  (`oem help` is an unknown command, measured on the Dot), and the Dot has no
  `fastboot boot` (ADR 0006). Pinna's rescue mode (ADR 0012) can, and it needs
  only the boot image, which Kaeru boots from the separate `recovery`
  partition, whatever the slot, when Volume Up is held at power-on:
  1. The page writes the release's boot image to `recovery` (16 MiB; Pinna
     writes nothing else there).
  2. The user plugs the device in again, holding Volume Up. pinna-init finds
     no Pinna root and enters rescue (`no-root`).
  3. The rescue page on `http://10.42.0.1` offers the five partitions and the
     `SHA256SUMS` as downloads. The installer can only link to it: a page
     served over HTTPS can't fetch it.
  4. Rescue's "enter fastboot" brings the device back; the user reconnects,
     and the install goes on.
- The stock `recovery` is lost, and the confirmation before that write says
  so. After a restore, `recovery` keeps Pinna's boot image, which still opens
  rescue on Volume Up.
- Before stage 3, on the Dot: Kaeru v2.0.0 boots `recovery` on Volume Up,
  pinna-init comes up from it (LK may build another command line for
  recovery), and Chrome lets the user keep large downloads over HTTP from
  `10.42.0.1`. The downloads are added to ADR 0012's rescue page.
- The unlock itself cannot be undone: it rewrites the boot chain. A restore
  brings back the previous system on Kaeru. When stage 4 ships, the goal
  "every installation step can be undone" names this exception, and the
  confirmation before the unlock says it.

### Unlock a stock device

- The user hands the page the amonet archive for their model. The page checks
  the archive's size and SHA-256, and the payload's, against a table pinned in
  the page per product and `lk_build_desc`. A build that is not in the table is
  refused, with a request to report it; there is no override.
- The payload goes out once, as one raw download, followed by `flash:brick`.
  Any reply, or none, is an unknown outcome. The page never sends it again on
  its own: it remembers the serial in the browser's storage, but that is only
  a hint (another browser does not know it). The page decides from what the
  device reports when it reconnects: Kaeru answers, or it does not.
- Stage 4 stays hidden (not linked, behind a URL parameter) until a stock unit
  of that model has been unlocked through the page. The project has none, so
  that first unit is a user's, who knows the risk. *(default)*

## Consequences

- Installing needs a Chromium-based browser and a USB cable, and on Linux one
  udev rule, on Windows the WinUSB driver. No `fastboot`, `adb`, bash or Nix.
- The terminal guide stays, for other browsers and for developers.
- Every stage is tested on hardware the project owns except the unlock. Its
  first run is on a user's device, and the pinned table needs updating when
  Amazon ships a new LK build.
- Moving the files to a host that allows cross-origin downloads later removes
  the download-and-pick step without changing the rest.
