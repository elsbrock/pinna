# ADR 0019: Recovery, backup and restore

Status: accepted (2026-11-17). It replaces:
- where rescue mode lives (ADR 0012);
- the root-only A/B, its own slot record, and the target of the boot guard
  (ADR 0003);
- the device fact "LK always loads `boot_a`", and that kernel updates are
  separate from root updates (ADR 0001);
- the backup in the browser installer (ADR 0016).

## Context

Measured on the owner's Echo Dot (2nd gen), Kaeru `v2.0.0-gab93750`, on
2026-11-16, or read from Kaeru's source at that commit:

- **Kaeru loads `boot_a` or `boot_b` by the slot record** at `0x360` in
  `misc`. Before commit `ab93750` ("Fix A/B slot boot on Echos", Kaeru issue
  #102) it always loaded `boot_a`. ADR 0001 recorded that bug as a device fact.
- **The preloader counts tries and falls back. Kaeru doesn't count**, so tries
  aren't counted twice. Measured on the Dot, 2026-11-17, starting with B at
  priority 15, 3 tries and not good, and A at priority 14 and good:

  | Boot | Kernel | B's tries after the boot |
  | --- | --- | --- |
  | 1 | `boot_b` | 2 |
  | 2 | `boot_b` | 1 |
  | 3 | `boot_a` | 0 |
  | 4 | `boot_a` | B at priority 0 |

  - On boot 3 the preloader used up B's last try. Kaeru then saw no tries left
    and loaded `boot_a`, so 3 tries give a new kernel **2 attempts**. The
    preloader then set B's priority to 0.
  - Slot B's boot chain (`tee2`, which differs from `tee1`) started.
  - LibreEcho reset the record on every boot for the same reason:
    its tries ran out.
  - `set_active:<slot>` (Kaeru) marks that slot good and blanks the other.
  - Kaeru repairs a record without a good slot, but only in LK, after the
    preloader. Kaeru's comments say a record without a good slot "will
    eventually brick the device".
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
- **Restarts over Wi-Fi took 26 to 29 s,** from the command until the network
  answered (2026-11-17). Two restarts while connected over USB took about two
  minutes each, and shutting down took about 100 s of that.

## Decision

### Three layers

| Layer | Covers | Lives in |
| --- | --- | --- |
| Normal system | updates and their rollback, kernel and root together | `boot_a`+`system_a`, `boot_b`+`system_b` |
| Recovery | everything rollback can't fix: both slots bad, settings, backup and restore | `recovery` |
| Kaeru fastboot | everything else; the installer | Kaeru, untouched |

- **One slot holds a kernel and a root together, as on Android.**
  - An update writes the boot image and the root into the other slot.
  - It sets that slot in Kaeru's record to priority 15 with N + 1 tries (the
    preloader takes one before LK looks), not good. Interfaces count
    attempts, 3 by default as before: `--tries` (`pinnactl update`,
    `pinna-bootctl set-active`) and the API's `tries_left` hold attempts, not
    the record's tries.
  - The running slot drops to priority 14 and stays good.
  - After the health check (ADR 0003), Pinna marks the new slot good.
  - A kernel that doesn't start runs out of attempts, and the preloader goes
    back to the old slot by itself.
  - A manifest without a boot image takes the running kernel along.
- **pinna-init boots the root of the slot LK started,** read from
  `androidboot.slot_suffix` in the device tree. When that root can't be
  verified or mounted, and the other slot is good, it marks the slot
  unbootable in Kaeru's record (priority 0) and restarts, so the preloader
  takes the other slot. A slot on trial that fails for any other reason (no
  system, a crash) is retired the same way. A good slot that fails after its
  root mounted stays in rescue mode: retiring it would throw away a version
  that worked. pinna-init writes the reason to pmsg before it restarts, so
  the next boot's report says why. Pinna's own root record at `0x40000` and
  its try counting are gone.
- **Proof that LK loads the slot's kernel.** A Kaeru before `ab93750` reports
  slot b but always loads `boot_a`. On such a device, a new kernel in `boot_a`
  would be the only kernel it ever loads.
  - Every boot image written into a slot carries `pinna.boot=<slot>` in its
    header's command line: the updater, `flash-gen2.sh` and the browser
    installer write it. LK passes the line on.
  - pinna-init records the kernel it got. After a healthy boot of slot b,
    `mark-good` stores whether LK loaded `boot_b` (`/var/lib/pinna/kernel-slots`).
    A boot of slot a says nothing about this.
  - Until that proof exists, the updater writes `boot_b` (where LK ignores it,
    nothing changes) but not `boot_a`, and says so.
  - When the proof says LK loads `boot_b`, it writes both. When it says LK
    ignores the slot, it writes no kernel at all: a new kernel then needs
    fastboot.
- **One slot is always good.** Pinna never writes a record without a good
  slot (the writer refuses one). An update marks the target unbootable first,
  while the running slot stays good, and only then makes it the trial. The
  preloader refuses to start without a good slot, and nothing short of the
  BootROM tool (amonet) gets the device back from that.
- **Machines without a bootloader slot** (the VMs, which QEMU starts directly)
  say `pinna.bootloader=none` on their kernel command line. Only then does
  pinna-init count tries itself, the way the preloader does, so a device never
  has a boot counted twice.
- **First installs and restores start slot a:** `flash-gen2.sh root`, the
  browser installer and `flash-gen2.sh restore` end with `set_active a`.
  Writing only the boot images leaves the slot alone.
- **Devices that ran the earlier, root-only A/B** move over once, by hand
  (`update.md`):
  1. update the root without restarting;
  2. open fastboot;
  3. `fastboot set_active` the slot that holds the new root;
  4. `flash-gen2.sh boot`, which writes the marked boot image to both boot
     partitions and restarts.

  pinna-init doesn't migrate Pinna's old record: an older root would write it
  again on every boot, and the migration would run again each time.
- **The Echo (2nd gen) gets this only after its slot B passes the test** the
  Echo Dot passed (above): both of its slots are marked good, and its `tee2`
  differs from `tee1`. This is a rule for installing these builds, not a
  check in the code: no release is published, and the one Echo (2nd gen)
  running Pinna is the owner's.

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
- **pinna-init loses its own rescue mode.** When a root can't be mounted, or
  a slot on trial fails, the other good slot comes first: pinna-init marks the
  failed slot unbootable and restarts. Only when no other slot is left, when
  a good slot fails after its root mounted, or for a reason another slot can't
  fix (Action held, a request from the dashboard), does it record the reason
  in the boot record, write `boot-recovery` and restart.

### Ways into recovery

- **Volume Up held at power-on.** This works when nothing else does.
- **`boot-recovery` in `misc`,** written by:
  - pinna-init, when no slot can start;
  - the boot guard (ADR 0003), in place of `bootonce-bootloader`, when the
    last good slot never gets healthy, so the device lands in recovery and
    not in bare fastboot;
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

- **Volume Up:** decided as the way into recovery (2026-11-17). Kaeru's
  source says it boots `recovery`; the first boot of the recovery image on the
  Echo Dot (2nd gen) and the Echo (2nd gen) measures it.
- **A `recovery` without a bootable image:** what does Kaeru do? Expected:
  fastboot.
- **Why the USB network stalls:** its own investigation. The rescue page and
  the dashboard depend on it.
- **Why shutdown took about 100 s while connected over USB.** Over Wi-Fi it
  took seconds.
- **The slot record on the Echo (2nd gen):** both of its slots are marked
  good, `lk_a` equals `lk_b`, and `tee1` differs from `tee2`. Counting and
  fallback were measured on the Dot only.
- **A boot of the old slot after a fallback:** the preloader may have started
  slot B's TEE with slot A's kernel (boot 3 above). It started, but it's worth
  a look.
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
- **Keeping `boot_a` = `boot_b` and leaving bad kernels to recovery:** the
  preloader already counts and falls back (measured), so a bad kernel can roll
  back by itself, without anyone pressing a button.
- **Backup as HTTP downloads from the rescue page:** the stall, a page served
  over HTTPS fetching HTTP, and NCM on Windows.
- **Backup through the inactive slot:** it loses that slot, and needs LK's slot
  handling where `recovery` needs none.
- **Backup on the device itself (`cache`):** 784 MiB is too small, and the
  backup dies with the device.
