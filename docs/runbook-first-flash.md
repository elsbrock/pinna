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

## 1. Back up, and save what Home Assistant knows (LibreEcho still running)

Step 4 overwrites `system_a` and wipes `userdata`, and recovery restores from
this backup. Do not continue without it.

```sh
scripts/backup-dot.sh ~/echo-backups/echodot2-$(date +%F)
```

This pulls the whole eMMC and its boot areas over adb (~4 GB), compares every
partition except `userdata` with the device, and cuts `partitions/*.img` with
`SHA256SUMS`, the layout `scripts/flash-dot.sh restore` takes. Copy the
directory somewhere safe. `scripts/verify-backup.sh <dir>` repeats the
comparison later, as long as LibreEcho still runs.

**Taking over the LibreEcho device in Home Assistant** (keeps its entities,
automations and scripts; see step 7): `userdata` holds LibreEcho's API key and
node name, so read them now:

```sh
adb shell cat /data/libreecho/config/web-config.json
```

Put `esphome_noise_key` into `secrets/device/esphome-key` and `hostname`
(LibreEcho's node name, `libreecho` if unset) into `[device] name` in
`secrets/device/pinnad.toml`.

## 2. Owner-local inputs and build

`secrets/` (gitignored) holds what makes the image yours:

| File | Needed | Ends up as | Create with |
|---|---|---|---|
| `secrets/release.key` | yes | root image signing key | `just keygen` (also rewrites `keys/release.pub`) |
| `secrets/machine-id` | yes | image machine id | `systemd-id128 new > secrets/machine-id` |
| `secrets/authorized_keys` | for SSH | `/root/.ssh/authorized_keys` | your SSH public key(s) |
| `secrets/wifi/wpa_supplicant-wlan0.conf` | for Wi-Fi | default for `/var/lib/pinna/wifi/` (first boot; edit on the device afterwards) | see below |
| `secrets/device/pinnad.toml` | no | default for `/var/lib/pinna/pinnad.toml` — device identity, e.g. `[device] name = "echodot2"` | by hand |
| `secrets/device/esphome-key` | no | default for `/var/lib/pinna/esphome-key` — the Home Assistant API key (base64, 32 bytes) | `head -c32 /dev/urandom \| base64`, or LibreEcho's (step 1) |

Without an API key the ESPHome API is plaintext and Home Assistant adds the
device without asking for one. Without Wi-Fi credentials the device is
reachable over USB only. A minimal Wi-Fi file:

```
ctrl_interface=DIR=/run/wpa_supplicant GROUP=netdev
country=DE

network={
    ssid="your-network"
    psk="your-passphrase"
    scan_ssid=1
}
```

The `/var` defaults are copied only when missing, so a re-flash (which wipes
`/var`) comes back as the same Home Assistant device on the same network.

```sh
nix develop                     # the supported build environment (x86_64 Linux)
just keygen                     # once: secrets/release.key signs root images
just all                        # kernel, wake word models, root image, boot image
```

`just image` builds the erofs root and seals it into `build/pinna.root.img`;
only images signed with the key in `keys/release.pub` (compiled into the boot
image's pinna-init) boot. After changing the key, flash the new boot image and
a root signed with the new key together (steps 4 and 5): `pinnactl update`
cannot cross a key change. Keep your `keys/release.pub` when pulling: a
different key in the tree builds a pinna-init that rejects your root images.

## 3. Enter fastboot

Unplug, hold **Volume Down**, plug the micro-USB cable in, release when the
ring changes. (On Kaeru units the Action button does nothing.) From a running
Pinna, `pinna-bootctl reboot-bootloader` does the same without touching the
device.

```sh
fastboot devices
scripts/flash-dot.sh check     # product must be BISCUIT; lists the built images
```

## 4. Flash the root filesystem

```sh
scripts/flash-dot.sh root      # system_a <- sealed root, system_b cleared, userdata wiped
```

The script checks the image's signature first. LK accepts 109 MiB per download
(`max-download-size` 0x6d00000); fastboot splits the larger root image into
sparse chunks by itself.

## 5. Flash the boot image

```sh
scripts/flash-dot.sh boot      # boot_a + boot_b <- Pinna, reboot
```

(`scripts/flash-dot.sh try` would boot the image from RAM without writing it,
but Kaeru on the Echo Dot answers `fastboot boot` with `unknown command`.)

## 6. First boot checks

On the UART console (if attached) pinna-init reports first:
`pinna-init: no slot state (first flash): slot a`, then
`pinna-init: slot mmcblk0p13 -> dm-0 (dm-verity, root hash …)` and
`pinna-init: booting slot a, version …`.

Within ~20 s the host should see a new USB device "Pinna (Echo Dot 2nd gen)":

- serial console: `/dev/ttyACM0` (root autologin) — `picocom -b 115200 /dev/ttyACM0`
  (picocom is not in the dev shell)
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

If it does not come up, see Recovery below.

## 7. Home Assistant

Pinna advertises `_esphomelib._tcp` via mDNS. Home Assistant should show
"Echo Dot" as a discovered ESPHome device. Add it, then configure the voice
assistant pipeline for the device. Say "Alexa…".

**Taking over a LibreEcho device entry**: Pinna exposes LibreEcho's entity set
under the same names (Home Assistant's unique ids are
`{mac}/0/{platform}/{name}`), so the existing entities, automations and
scripts keep working, provided the image carries LibreEcho's node name and
API key (step 1). Reload the ESPHome config entry once so Home Assistant
rebuilds the platforms from Pinna's device info.

The API is plaintext until a key is set: put a base64 32-byte key in
`/var/lib/pinna/esphome-key` (or `[esphome] encryption_key` in
`/var/lib/pinna/pinnad.toml`), restart pinnad, and give Home Assistant the
same key. Home
Assistant can then rotate it over the encrypted connection; the new key is
stored in `/var/lib/pinna/esphome-key`.

Things expected to need tuning on real hardware (not testable off-device):
mic gain and wake threshold (`/var/lib/pinna/pinnad.toml` on the device; it
overrides the image's `/etc/pinna/pinnad.toml`; `systemctl restart pinnad`
after editing, `journalctl -u pinnad` shows scores), speaker mixer sequence,
LED direction.

## 8. Updates and rollback

On the build machine:

```sh
just release                   # build/release/{pinna-<version>.root.img, pinna-boot.img, manifest.json}
cd build/release && python3 -m http.server 8000
```

On the device (as root). If you used `just push`, first run
`scripts/push-dev.sh --remove` from the build machine: the dev overlay would
otherwise keep shadowing the new image's binaries.

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
scripts/flash-dot.sh restore <backup dir>/partitions
```

restores LibreEcho exactly from the step 1 backup (boot_a, boot_b, system_a,
userdata, and system_b when the backup has it). It checks the backup's
`SHA256SUMS` and that every required image is there before writing anything.

### Fastboot guard

`/etc/pinna/fastboot-guard` (in every image built from this tree; it only acts
when the Kaeru payload is present in `expdb`) makes a boot arm a
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

