# ADR 0019: Recovery, backup and restore

Status: proposed (2026-11-16). When accepted, it replaces where rescue mode
lives (ADR 0012), the target of the boot guard (ADR 0003), the device fact
"LK always loads `boot_a`" (ADR 0001) and the backup in the browser installer
(ADR 0016).

## Context

Measured on the owner's Echo Dot (2nd gen), Kaeru `v2.0.0-gab93750`, on
2026-11-16, or read from Kaeru's source at that commit:

- **Kaeru loads `boot_a` or `boot_b` by the slot record** at `0x360` in
  `misc`. Before commit `ab93750` ("Fix A/B slot boot on Echos", Kaeru issue
  #102) it always loaded `boot_a`. ADR 0001 recorded that bug as a device fact.
- **Kaeru never counts tries and never falls back** to the other slot.
  `set_active:<slot>` marks that slot good and blanks the other one. The
  preloader reads the same record, and Kaeru always keeps one slot marked good
  because "the defaults will eventually brick the device".
- **Kaeru boots `recovery` instead of the slot's boot partition** when Volume
  Up is held at power-on, or when `misc` holds `boot-recovery` at offset 0.
  `fastboot reboot recovery` writes that request, and so can Linux.
  - Measured: both requests boot `recovery`.
  - Kaeru clears the request as it boots, so it is one-shot. The next restart
    boots normally again (measured).
  - `bootonce-bootloader` opens fastboot once; `fastboot_please` opens it on
    every boot until it is erased.
- **LK passes its command line in the device tree**
  (`/proc/device-tree/chosen/bootargs`). The kernel uses its built-in one. In
  a boot from `recovery`, LK leaves out `skip_initramfs` (measured).
- **Layout of `misc`:** the boot request at 0, Kaeru's slot record at `0x360`,
  Pinna's root slot record at `0x40000`. They don't overlap.
- **Kaeru can't read a partition** (it has no such command; `oem help` is an
  unknown command), and the Dot has no `fastboot boot` (ADR 0006). Writing
  16 MiB over fastboot took 1.5 s.
- **Partitions:** `boot_a`, `boot_b` and `recovery` are 16 MiB each,
  `system_a` and `system_b` 768 MiB, `cache` 784 MiB (unused), `userdata`
  1264 MiB. Pinna doesn't use `recovery`; it holds Amazon's recovery.
- **The USB network stalls under sustained load.**
  - Four reads of 16 to 50 MB over SSH; two stopped without an error on either
    side. Afterwards the link refused new connections for a while.
  - Throughput was 1.9 to 5.2 MB/s, limited by SSH.
  - Windows 10 may not bind NCM without a Microsoft OS descriptor.
- **A restart from Pinna takes about two minutes:** shutting down took about
  100 s of that.

## Decision

### Three layers

| Layer | Covers | Lives in |
| --- | --- | --- |
| Normal system | updates and rollback of the root (ADR 0003) | `boot_a` = `boot_b`, `system_a`/`system_b` |
| Recovery | everything that stops the normal system from starting: root, kernel, settings | `recovery` |
| Kaeru fastboot | everything else; the installer | Kaeru, untouched |

- **The kernel is not A/B.** `boot_a` and `boot_b` always hold the same boot
  image, and Pinna never writes Kaeru's slot record. Whatever slot the record
  names boots the same kernel. A kernel that doesn't start is fixed from
  recovery.

### The recovery image

- **`pinna-recovery.img`, a release file of its own:** Pinna's kernel and an
  initramfs that does only recovery. It never mounts or starts a root.
- **Only the installer writes `recovery`.** `flash-gen2.sh` writes it as well.
  Updates never touch it, so a broken update can't break recovery.
- **Over USB it offers:**
  - **A fastboot interface for the installer:** `getvar`, `download`, `flash`,
    `fetch:<partition>` (read), `reboot`, `reboot-bootloader`. It reads every
    write back to check it.
  - **For people:** ADR 0012's rescue page and shell over NCM and ACM: why the
    device is in recovery, the logs, the support bundle, restart, and open
    fastboot.
- **Its own USB identity:** a product ID of its own, the fastboot interface
  (class ff/42/03) next to ACM and NCM, and Microsoft OS descriptors (`WINUSB`
  for fastboot, `WINNCM` for the network). Windows then needs no extra driver
  for recovery. Kaeru's fastboot still needs WinUSB from Zadig.
- **On the ring:** recovery shows that it is recovery, and why.
- **pinna-init loses its own rescue mode.** For every reason that started
  rescue (ADR 0012), it records the reason in the boot record, writes
  `boot-recovery` and restarts.

### Ways into recovery

- **Volume Up held at power-on.** This works when nothing else does.
- **`boot-recovery` in `misc`,** written by:
  - pinna-init, when nothing can start;
  - the boot guard (ADR 0003), in place of `bootonce-bootloader`, so a boot
    that never gets healthy lands in recovery and not in bare fastboot;
  - the dashboard ("Restart in recovery") and Action held at start;
  - the installer, with `fastboot reboot recovery`.
- **A broken recovery can't trap the device:** Kaeru clears the request as it
  boots, so the next power cycle starts the normal system.

### Backup of the device

This is optional. The installer offers it before Pinna's first write to
`boot_a`, `boot_b`, `system_a`, `system_b` or `userdata`.

1. Over Kaeru's fastboot, the page writes `pinna-recovery.img` to `recovery`
   (its only write so far) and sends `reboot recovery`.
2. The user picks the device again and a folder (Chrome's File System Access
   API).
3. The page reads the five partitions with `fetch` and writes
   `<partition>.img`, with a `SHA256SUMS` computed as the data arrives. This is
   the format `flash-gen2.sh restore` reads; about 2.8 GB.
4. The install goes on over recovery's fastboot: it writes the partitions,
   reads them back to compare, and restarts.

What is lost: the original `recovery`, and the confirmation before that write
says so. Everything else Pinna overwrites can be restored, so the goal "every
installation step can be undone" holds except for `recovery` and the unlock
itself (ADR 0016).

The backup goes over WebUSB, not as HTTP downloads from the rescue page:
- the USB network stalls under load;
- a page served over HTTPS can't fetch from the device;
- Windows may not bind NCM.

### Restore

- **In the browser installer, "Restore a backup":** start recovery (from
  Kaeru's fastboot with `reboot recovery`, or with Volume Up), pick the folder,
  check `SHA256SUMS`, write the five partitions, read them back, and restart.
  `recovery` keeps Pinna's recovery.
- **In the terminal:** `flash-gen2.sh restore` keeps working from Kaeru's
  fastboot.

### Backup of the settings

Settings are a small file from the dashboard: Wi-Fi networks, the Home
Assistant key, the name, the wake word and the volume. After a reinstall, the
installer offers to put it back. It is designed with the settings (ADR 0007),
not here.

## Open before building

- **Volume Up:** does holding it at power-on boot `recovery` on the Echo Dot
  (2nd gen) and the Echo (2nd gen)? It needs someone at the device.
- **A `recovery` without a bootable image:** what does Kaeru do? Expected:
  fastboot.
- **Why the USB network stalls:** its own investigation. The rescue page and
  the dashboard depend on it.
- **Why shutdown takes about 100 s.** Entering recovery from a running system
  waits for it.
- **WebUSB speed for `fetch`:** expected well above 10 MB/s.

## Consequences

- **New work:**
  - the `pinna-recovery.img` build target and release file;
  - FunctionFS in the kernel (`CONFIG_USB_CONFIGFS_F_FS`);
  - a fastboot server in the recovery initramfs;
  - stage 3 of the installer, backup and restore (ADR 0016).
- **`flash-gen2.sh` and the install guide:**
  - `flash-gen2.sh` writes `recovery`;
  - in the install guide, `recovery` leaves the list of partitions Pinna never
    writes and joins the "What gets written" table.
- **pinna-init gets smaller.** ADR 0012's page and actions move to the
  recovery image.

## Alternatives not taken

- **Rescue in both `boot_a` and `recovery`:** two copies to keep in step. One
  dedicated image is simpler.
- **An A/B kernel with automatic rollback:** Kaeru doesn't count tries, and a
  slot record without a good slot risks a brick in the preloader.
- **Backup as HTTP downloads from the rescue page:** the stall, a page served
  over HTTPS fetching HTTP, and NCM on Windows.
- **Backup through the inactive slot:** it loses that slot, and needs LK's slot
  handling where `recovery` needs none.
- **Backup on the device itself (`cache`):** 784 MiB is too small, and the
  backup dies with the device.
