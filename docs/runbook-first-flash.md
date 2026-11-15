# First flash: Echo Dot (2nd gen) → Pinna

Moves an Echo Dot that currently runs LibreEcho (Kaeru/amonet-unlocked) to
Pinna. Every step says what it writes and how to undo it.

## What gets written

| Partition | Before | After |
|---|---|---|
| `boot_a` (p10) | LibreEcho boot image | Pinna boot image (`build/pinna-boot.img`: kernel, DTB, initramfs with pinna-init) — the one LK always loads |
| `boot_b` (p11) | LibreEcho copy | Pinna boot image (manual fallback copy) |
| `system_a` (p13) | stock Fire OS system (LibreEcho reads Wi-Fi firmware from it) | Pinna root, sealed slot image (`build/pinna.root.img`: erofs + signed footer + dm-verity hash tree) |
| `system_b` | stock copy | first MiB zeroed (no image); later written by `pinnactl update` |
| `userdata` (p16) | LibreEcho `/data` | wiped; Pinna formats it as `/var` on first boot |

Never written: preloader/boot areas, `lk_a`/`lk_b`, `tee1`/`tee2`, `expdb`
(Kaeru payload), `persist` (identity, calibration, MAC). `misc` only at
runtime: the bootloader command field (fastboot guard) and Pinna's A/B slot
state at offset 0x40000 (ADR 0003).

The Wi-Fi firmware that used to come from `system_a` is baked into the Pinna
root (`firmware/` in the repository). The microphone FPGA bitstream is in the
repository too (`kernel/firmware/`) and built into the kernel.

## 0. Owner-local inputs and build

`secrets/` (gitignored) holds what makes the image yours:

| File | Ends up as |
|---|---|
| `secrets/authorized_keys` | `/root/.ssh/authorized_keys` |
| `secrets/wifi/wpa_supplicant-wlan0.conf` | default for `/var/lib/pinna/wifi/` (first boot; edit on the device afterwards) |
| `secrets/device/pinnad.toml` | default for `/var/lib/pinna/pinnad.toml` — device identity, e.g. `[device] name = "echodot2"` |
| `secrets/device/esphome-key` | default for `/var/lib/pinna/esphome-key` — the Home Assistant API key (base64) |
| `secrets/machine-id`, `secrets/release.key` | image machine id; root image signing key (`just keygen`) |

The `/var` defaults are copied only when missing, so a re-flash (which wipes
`/var`) comes back as the same Home Assistant device on the same network.

```sh
nix develop
just keygen                     # once: secrets/release.key signs root images
just kernel models image bootimg
scripts/flash-dot.sh check      # needs the device in fastboot, see step 2
```

`just image` builds the erofs root and seals it into `build/pinna.root.img`;
only images signed with the key in `keys/release.pub` (compiled into the boot
image's pinna-init) boot. After changing the key, flash the new boot image and
a root signed with the new key together (steps 3 and 5): `pinnactl update`
cannot cross a key change.

## 1. Backup (done 2026-11-15)

`~/echo-backups/echodot2-2026-11-15/`: full `mmcblk0.img`, `mmcblk0boot{0,1}.img`,
and per-partition images under `partitions/` with `SHA256SUMS`. `boot_a`/`boot_b`
hash `99f17f9e…` = the LibreEcho image that was running.

Optional: verify against the live device before changing anything (LibreEcho, adb):

```sh
for p in 10 11 13 16; do adb shell sha256sum /dev/mmcblk0p$p; done
```

## 2. Enter fastboot

Unplug, hold **Volume Down**, plug the micro-USB cable in, release when the
ring changes. (On Kaeru units the Action button does nothing.) From a running
Pinna, `pinna-bootctl reboot-bootloader` does the same without touching the
device.

```sh
fastboot devices
scripts/flash-dot.sh check     # product must be BISCUIT
```

## 3. Flash the root filesystem

```sh
scripts/flash-dot.sh root      # system_a <- sealed root, system_b cleared, userdata wiped
```

If LK rejects the ~160 MB image (max-download-size), convert first:
`img2simg build/pinna.root.img build/pinna.root.simg` and flash that with
`ROOT_IMG=build/pinna.root.simg scripts/flash-dot.sh root`. The script checks
the signature of raw images only, so verify the raw one first:
`cargo run -q --release -p pinna-image -- verify build/pinna.root.img`.

## 4. Try the kernel from RAM first

```sh
scripts/flash-dot.sh try       # fastboot boot build/pinna-boot.img
```

Nothing is written. Kaeru on the Echo Dot answers `fastboot boot` with
`unknown command`, so on that device skip to step 5; the recovery path is the
same.

On the UART console (if attached) pinna-init reports first:
`pinna-init: no slot state (first flash): slot a`, then
`pinna-init: slot mmcblk0p13 -> dm-0 (dm-verity, root hash …)` and
`pinna-init: booting slot a, version …`.

Within ~20 s the host should see a new USB device "Pinna (Echo Dot 2nd gen)":

- serial console: `/dev/ttyACM0` (root autologin) — `picocom -b 115200 /dev/ttyACM0`
- network: the host gets 10.42.0.x by DHCP; `ssh root@10.42.0.1` (or `root@pinna.local`)

Then check:

```sh
pinnactl status
systemctl --failed
journalctl -b -u pinna-wifi -u wpa_supplicant@wlan0 -u pinnad
ip addr show wlan0
pinna-bootctl status
pinna-bootctl slot-status    # active a, a good (after pinna-boot-ok), booted a
findmnt /                    # /dev/dm-0 erofs (dm-verity)
pinnactl led-test            # ring animation
pinnactl volume 40
```

## 5. Make it persistent

From Pinna: `pinna-bootctl reboot-bootloader` (or Volume Down + replug), then

```sh
scripts/flash-dot.sh boot      # boot_a + boot_b <- Pinna, reboot
```

## 6. Home Assistant

Pinna advertises `_esphomelib._tcp` via mDNS. Home Assistant should show
"Echo Dot" as a discovered ESPHome device. Add it, then configure the voice
assistant pipeline for the device. Say "Alexa…".

**Taking over a LibreEcho device entry**: Pinna exposes LibreEcho's entity set
under the same names (Home Assistant's unique ids are
`{mac}/0/{platform}/{name}`), so the existing entities, automations and
scripts keep working. Give Pinna the old node name and API key
(`secrets/device/pinnad.toml`, `secrets/device/esphome-key`; LibreEcho kept the
key as `esphome_noise_key` in `/data/libreecho/config/web-config.json`), then
reload the ESPHome config entry once so Home Assistant rebuilds the
platforms from Pinna's device info.

The API is plaintext until a key is set: put a base64 32-byte key in
`[esphome] encryption_key` (`/etc/pinna/pinnad.toml`, i.e. in the image) or in
`/var/lib/pinna/esphome-key`, and give Home Assistant the same key. Home
Assistant can then rotate it over the encrypted connection; the new key is
stored in `/var/lib/pinna/esphome-key`.

Things expected to need tuning on real hardware (not testable off-device):
mic gain and wake threshold (`/etc/pinna/pinnad.toml`, `journalctl -u pinnad`
shows scores), speaker mixer sequence, LED direction.

## 7. Updates and rollback

On the build machine:

```sh
just release                   # build/release/{pinna-<version>.root.img, pinna-boot.img, manifest.json}
cd build/release && python3 -m http.server 8000
```

On the device (as root):

```sh
pinnactl update http://<host>:8000/manifest.json --reboot
```

If the build machine's firewall blocks port 8000, tunnel it over the SSH
session instead: `ssh -R 8000:localhost:8000 root@10.42.0.1 pinnactl update
http://127.0.0.1:8000/manifest.json --reboot` (on the Echo Dot over USB:
download, write and full verification of a 153 MiB root take ~47 s).

This writes the image into the slot that is not booted (refusing while the
booted slot is still on trial), checks size and sha256 while writing, reads
it back (signature, whole dm-verity hash tree) and only then makes it active
for a trial boot with 3 tries (`--tries N`). After the reboot pinna-init boots
the new slot; once `pinna-boot-ok.service` passes (pinnad answers, sshd up),
the slot is marked good. If the health check fails or does not pass within
5 minutes, the device reboots; each boot uses up a try, and with none left
pinna-init goes back to the previous slot. `pinna-bootctl slot-status` shows
the state and the image version in each slot; `pinna-bootctl set-active a`
switches by hand (as a trial).

`boot_a` (kernel, initramfs) is not A/B: a new kernel or pinna-init is
flashed with `scripts/flash-dot.sh boot` as before.

## Recovery

Any time, from fastboot (Volume Down + plug in):

```sh
scripts/flash-dot.sh restore ~/echo-backups/echodot2-2026-11-15/partitions
```

restores LibreEcho exactly (boot_a, boot_b, system_a, userdata).

Restoring LibreEcho also restores `system_b` when the backup has it.

### Fastboot guard

`/etc/pinna/fastboot-guard` (present in dev images) makes a boot arm a
one-shot "enter fastboot" request early, cleared once sshd and pinnad are up
(`pinna-boot-ok.service`). A boot that hangs, followed by a power cycle (or a
failed health check, which reboots), lands in fastboot instead of looping.
Kaeru clears the request itself, so `fastboot reboot` tries Pinna again.

It is armed only when the booted slot is already good or there is no good
fallback. During the trial boot of a freshly installed slot with a good
previous slot it is not armed (`not arming: trial boot of slot b …`): a failed
trial reboots and rolls back to the previous slot instead of stopping in
fastboot.

## Development loop and hardware gate

```sh
PINNA_HOST=root@10.42.0.1 just push          # fresh binaries as a sysext overlay, pinnad restarted
scripts/push-dev.sh --remove                 # back to the image's binaries (before an update)
PINNA_HOST=root@10.42.0.1 PINNA_API_KEY=secrets/device/esphome-key \
  THROUGHPUT_URL=http://<lan-host>/<file> just test-device
```

`just test-device` runs the boot checks on the real device plus the Home
Assistant entity set, a Wi-Fi throughput floor and an acoustic test: with the
PipeWire microphone graph (ADR 0004) the device plays music and its wake word
through its own speaker, and the echo canceller must remove at least 20 dB of
it without the device waking on its own voice. The acoustic test needs a
quiet room; `PINNA_ACOUSTIC=0` skips it.

## Known limitations

- **Wi-Fi**: the vendor driver's receive path (kernel patches 0021–0027) reaches
  1–1.7 MB/s, but on a busy 2.4 GHz channel downloads still stall for seconds
  at times. Speech and TTS are small; long streams may glitch.
- **Echo cancellation** needs a few seconds of playback to converge; the first
  seconds of new audio are only partly removed.

