# ADR 0012: Rescue mode, boot stages on the ring, and how the last boot ended

Status: proposed (2026-11-15). Amends ADR 0003: when nothing can be booted,
pinna-init enters rescue mode instead of rebooting in a loop.

## Context

- Bringing the 6.12 kernel up on the Echo Dot took an evening because a failing
  boot showed nothing. The Dot has no UART. USB exists only once userspace
  binds the gadget. pinna-init rebooted every ~20 s when it found nothing to
  boot, which erased the evidence. The ring showed whatever was last written to
  it, and reading meaning into those colours led to wrong theories. Once a
  diagnostic initramfs gave a USB shell and the logs, each bug took minutes.
- What can carry information, and when:
  - the ring (IS31FL3236, sysfs `frame`), as soon as its I2C driver has probed;
  - the USB gadget (ACM serial, NCM network), once a UDC exists;
  - pstore (ramoops in the DTB: console, dmesg on an oops or panic, pmsg),
    which survives a warm reset only;
  - `misc` (layout in ADR 0003), which survives anything;
  - the persistent journal, once `/var` is mounted.
- The kernel command line is fixed: `loglevel=4` keeps pinna-init's level 5
  lines out of console-ramoops, and `panic=10` reboots 10 s after a panic.
- `mtk_wdt` reports no boot status. `WATCHDOG_HANDLE_BOOT_ENABLED` keeps the
  hardware watchdog fed until userspace opens `/dev/watchdog` (systemd does,
  `RuntimeWatchdogSec=20s`).
- `boot_a` is not A/B, so pinna-init can be newer or older than the root it
  boots, also after a rollback.
- At power-on, LK enters fastboot on Volume Down and ignores Action. The
  Microphone off button is the PMIC key that also latches the hardware mic
  switch.
- One healthy 6.12 boot: storage and the root ready ~1.5 s after the kernel
  starts, USB gadget at 4 s, Wi-Fi driver at 11 s, pinnad at 12 s, health
  verdict at 23 s, Wi-Fi associated at 65 s.
- Ring colours in use (`ring-and-buttons.md`): blue (starting, listening), red
  (microphones off; three flashes for a failed request), amber (timers),
  violet, cyan, yellow, light blue. White and green are free.

## Decision

### Rescue mode

pinna-init enters rescue mode instead of booting when:

| Trigger | Reason |
| --- | --- |
| neither `system_a` nor `system_b` after 15 s | `no-storage` |
| neither slot holds a bootable image | `no-root` |
| the root is mounted, but recording the slot, switching root or exec fails | `no-system` |
| pinna-init itself fails (a panic: a bug) | `crashed` |
| the boot record counts 3 boots of a good slot without a health verdict | `failed-boots` |
| Action is held when pinna-init starts (the key it reports, `KEY_HELP`) | `button` |
| a one-shot request in the boot record (`pinna-bootctl reboot-rescue`, the dashboard) | `requested` |

Everything rescue needs is in the boot image; nothing comes from a root:

- **USB gadget** with the USB ids and class of normal operation, named
  "Pinna rescue", its serial number from idme and the NCM MAC addresses
  derived from it as the normal gadget derives them. It waits for the UDC as
  long as it takes. busybox leaves the initramfs before the switch to the root but stays
  open, so rescue after a failed switch still has it.
  - ACM serial port: a root shell (static busybox, ~1.4 MB; the boot
    partition has 16 MiB and uses ~4 MB). The kernel console gets one too.
  - NCM network: `10.42.0.1/24` with a DHCP server.
- **A page on `http://10.42.0.1`**, served on usb0 only. It shows:
  - why the device is in rescue mode, and what usually fixes that reason,
    starting with how to open fastboot;
  - pinna-init's log;
  - a link that downloads one text file with that log, dmesg, the slot
    records, the previous boot's pstore records, the partitions and the
    release key the boot image trusts.

  Its actions:
  - restart (phase 1): boots the same way, which ends in rescue again unless
    something changed;
  - boot slot a or b (as a trial, ADR 0003);
  - enter fastboot;
  - reset settings (ADR 0010): a flag in the boot record, which the root
    carries out;
  - continue booting, offered only for `button` and `requested` when a slot
    is bootable.

  Every request must name `10.42.0.1` as its Host, against DNS rebinding.
  Actions are POST requests. One that carries an `Origin` other than
  `http://10.42.0.1` is refused: a browser sends one with every cross-site
  POST, so a website open in the computer's browser can't trigger them. A
  POST without an `Origin` (curl on the computer) is accepted.
- **No shell on the network and no SSH.** SSH keys live on `/var`, which may
  be what is broken.
- **The watchdog stays with the kernel.** Rescue never opens `/dev/watchdog`.
- **Leaving rescue:**
  - one of the page's actions;
  - `reboot` or `poweroff` in the shell (busybox signals PID 1);
  - pressing Action (a new press, after the power-on hold is released;
    phase 3, with `button`);
  - for `failed-boots`, `button` and `requested` only: no computer configures
    the gadget within 10 minutes. The device then boots normally with the
    counter at 2, so one more failed boot returns it to rescue.

  `no-storage`, `no-root`, `no-system` and `crashed` stay until someone acts.
  A reboot can't fix them, and without USB the ring is the only output.

### Boot record

One sector at `misc` + 0x41000, written like the slot record (flush, readback).
It is separate from the slot record because the v1 slot reader rejects any
non-zero reserved byte.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | magic `PINNABRC` |
| 8 | 1 | version (1) |
| 9 | 1 | `failed_boots`: boots of a good slot since the last health verdict |
| 10 | 1 | flags: bit 0 rescue requested, bit 1 reset settings (both one-shot) |
| 11 | 5 | reserved, zero |
| 16 | 4 | crc32 (IEEE) of bytes 0..16 |

- A missing or invalid record reads as all zero. A reader that finds an
  unknown version ignores the record and does not write it.
- pinna-init increments `failed_boots` when it boots a good slot whose root
  carries the marker below. Trial boots don't count: their tries already roll
  back (ADR 0003).
- `pinna-bootctl mark-good` sets `failed_boots` to 0. It creates no record
  where none exists.
- **Root marker:** `/usr/lib/pinna/boot-protocol`, containing `1`, says that
  this root resets the counter and runs the stage helper. pinna-init reads it
  from `/sysroot` before it switches root and records it as
  `/run/pinna/boot-protocol`; without that, the stage helper exits at once and
  leaves the ring to that root's pinnad. A root without the marker is
  neither counted nor timed, so a rollback to an older root can't send a
  healthy device into rescue or onto a failure code. An older pinna-init never
  reads the record.

### pstore

- pinna-init writes every log line to `/dev/pmsg0` as well as to `/dev/kmsg`.
- **One archive per boot.** `pstore-archive` (a drop-in on
  `systemd-pstore.service`) stores each boot's records under
  `/var/lib/systemd/pstore/<boot id>/`, so a reset doesn't overwrite the last
  one's as systemd's fixed names did. The id is written at each boot; the
  directory of the previous boot is at `/run/pinna/previous-pstore`, which
  pinnad and the support bundle read.
- **Reboots leave a reason.** Whatever triggers a reboot writes the reason to
  `/run/pinna/reboot-reason`: the health check failing (the unit leaves
  `health check …`), the dashboard's restart (`user`) and slot switch
(`switch-slot`), `pinnactl update
  --reboot` (`update`), `pinna-bootctl reboot-bootloader` (`bootloader`) or
  `reboot-rescue`. A systemd-shutdown hook in
  `/usr/lib/systemd/system-shutdown/` then writes
  `pinna: reboot (<reason>)` to `/dev/pmsg0`, with `not recorded` when no
  reason was written. Rescue writes `pinna: rescue (<reason>)`.
- **On the next boot**, the previous boot's end is worked out from pstore
  (systemd-pstore archives it to `/var/lib/systemd/pstore`):

  | pstore holds | The previous boot |
  | --- | --- |
  | an oops or panic in the kernel log (`dmesg-ramoops` or the console record) | crashed |
  | a `pinna: rescue` line in `pmsg-ramoops` | was in rescue mode, with that reason |
  | a `pinna: reboot` line in `pmsg-ramoops` | restarted, with that reason |
  | records, but no such line | was reset without shutting down (watchdog or hard reset) |
  | nothing | lost power, or this is a cold start |

  pinnad keeps this verdict, together with `failed_boots` at boot, for the
  dashboard ("The last start failed: …") and the support bundle. The rescue
  page reads the same records from `/sys/fs/pstore`.

### Boot stages on the ring

- **Progress:** a blue arc grows clockwise from the Action button, one segment
  per stage. Its leading end gets brighter and dimmer while that stage is
  pending. Boot progress ends at stage 7, when the boot is marked healthy:
  the ring is full, then fades to idle. This replaces today's "blue light
  that runs around the ring, briefly".
- **Failure:** a stage that misses its deadline makes the ring flash white N
  times, pause 2 s and repeat, where N is the stage number. Flashes can be
  counted from any side of the device. The code stays while the device is
  idle, until the next boot. The states in `ring-and-buttons.md` take
  priority over it.

| N | Stage | Shown by | Deadline (time since boot) |
| --- | --- | --- | --- |
| 1 | Storage found | pinna-init | 15 s |
| 2 | Root verified and mounted | pinna-init | none: a failure enters rescue (`no-root`) |
| 3 | System started | pinna-init | none |
| 4 | USB ready (gadget bound) | stage helper | 30 s; skipped on boards without a UDC |
| 5 | Wi-Fi driver up (`wlan0`) | stage helper | 150 s (`pinna-wifi` gives up at 120 s); skipped on boards without it |
| 6 | Pinna running (pinnad answers) | stage helper | 180 s |
| 7 | Healthy (`pinna-boot-ok` passed, slot marked good) | pinnad | none on the ring: a failed check reboots and counts |

- **Not boot stages:** the Home Assistant connection, which may become
  optional, and Wi-Fi association, which the health verdict doesn't include
  (ADR 0003). Nothing in the boot progress, the health check or rescue mode
  depends on either. Wi-Fi association becomes a stage only if it becomes part
  of the health verdict.

- **Who drives the ring:** pinna-init, then the stage helper, then pinnad.
  pinna-init's last frame stays in the LED controller until the helper starts.
  The stage helper is an early unit in the root (`DefaultDependencies=no`,
  started in sysinit), so it is updated with the root. It polls conditions
  rather than relying on hooks in other units and writes what it reached to
  `/run/pinna/boot-stages` (`stage N`, plus `failed N` once a stage missed its
  deadline). Pinnad writes `/run/pinna/ring` when its control API answers —
  that is stage 6 — and the helper stops at that marker; pinnad continues the
  arc for any stage still pending, using the same deadlines, and takes the
  ring through stage 7. The stages' conditions and deadlines are shared code
  in `pinna-bootctl`, so both sides agree.
- **Stages without their hardware:** a board with no UDC directory (the VM
  images) skips the USB stage, and a device tree without `mediatek,mt8163` —
  what `pinna-wifi.service` is already conditioned on — skips the Wi-Fi one.
  Skipped stages count as done, so the arc still ends at 7.
- **Stage 7:** `pinna-bootctl mark-good` writes `/run/pinna/boot-ok`; pinnad
  fills the ring, fades it out, and, when a stage recorded a failure, flashes
  that code while the device is idle (the states in `ring-and-buttons.md` take
  priority over it, as above).
- **Mute during boot:** until pinnad runs, the stage ring wins over a
  hardware mute. After that, pinnad's priority table applies.
- **Rescue mode:** green, slowly brighter and dimmer; solid green once a
  computer has configured the USB gadget. Until USB is up (no UDC yet),
  rescue shows the white code of the stage that failed instead: 1 for
  `no-storage`, 2 for `no-root`, 3 for `no-system`, the stage in progress for
  `crashed`, 4 for the other reasons.
- **Kernel panic:** only if a spike shows it works, a panic notifier in the
  IS31 driver writes one all-white frame. That needs I2C transfers in panic
  context, which the MT8163 I2C driver may not support. Without it, the next
  boot reports "crashed" from pstore.
- **The table is a user-facing contract.** The stages, the codes and the
  rescue colours are published on the website (troubleshooting, ring and
  buttons) and asked for in the issue template. A changed number or meaning
  amends this ADR. New stages are appended.

### Order of work

1. pinna-init: rescue for `no-storage`, `no-root`, `no-system` and
   `crashed`, with the restart action; the pmsg copy of its log; stages 1–3.
   Built.
2. The root: the stage helper (stages 4–6), the `boot-protocol` marker, the
   reboot-reason hook, pinnad's stage 7 and the previous boot's verdict, and
   one pstore archive per boot. Built.
3. The boot record: the counter, `button` and leaving with Action,
   `requested`, the 10-minute way out, the page's other actions.
4. A spike on the panic frame.

Before the contract is published, check on the device that LK ignores Action
at power-on.

Already checked on one healthy 6.12 boot:

- The LED driver probes at 0.42 s and pinna-init starts at 1.20 s, so stages
  1–2 can be shown.
- The Action key's input device appears only 2 ms before pinna-init starts.
  pinna-init waits up to 1 s for it before it decides on `button`.

## Consequences

- A broken boot no longer reboots in a loop and erases its own evidence. It
  stops in rescue mode, with a shell, the logs and a way back over the USB
  cable.
- The boot image grows by ~1.4 MB of busybox plus pinna-init's rescue code. The
  rescue code isn't updated by `pinnactl update`, so it stays small and
  depends on nothing in a root.
- A healthy boot writes two sectors of `misc` (count, then reset).
- Three replugs in a row, each before the health verdict (~25 s), enter
  rescue. The device leaves it after 10 minutes without a computer, or when
  Action is pressed.
- Volume Down stays the bootloader's fastboot key.
- Not covered:
  - A hang between stage 3 and the stage helper: the arc stops at 3 and
    doesn't flash.
  - Failures before pinna-init (LK, the kernel) show no stages. A kernel crash
    leaves `dmesg-ramoops` after a warm reset.
- Later, not decided: a Wi-Fi setup hotspot as a last resort when neither USB
  nor Wi-Fi works (ADR 0009 keeps no access point mode), and pacing the arc by
  the previous boot's timings.
- Separate work: the support bundle and "Download diagnostics", the kernel
  config parity and hardware smoke gates, and the script that tries a new
  kernel unattended.
