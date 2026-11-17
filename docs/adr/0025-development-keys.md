# ADR 0025: Development keys that a satellite trusts

Status: proposed (2026-11-17). Amends ADR 0003 (pinna-init and the updater
accept more than one key) and ADR 0015 (which public keys a build compiles in,
and how contributors run root images of their own). Points marked *default*
are not confirmed by the owner yet.

## Context

- Only CI holds the secret half of the project key, and only for `v*` tags
  (ADR 0015). Contributors don't have it.
- Changing pinnad or the other daemons needs no key. `just push` copies
  pinnad, pinnactl, pinna-bootctl and pinna-bootstages over SSH into a
  systemd-sysext overlay, `/var/lib/extensions/pinna-dev`; the signed root
  stays as it is (`scripts/push-dev.sh`). The dashboard needs no key either:
  during development it runs on the developer's computer, against a
  satellite's API (`PINNA_URL`).
- Changing the root image (packages, units, configuration) needs a root
  signed with a key the satellite accepts. Today a build compiles in
  `secrets/release.pub` when it exists, else `keys/release.pub`, and
  pinna-init and the updater accept only that one key (ADRs 0003, 0015).
  A contributor runs `just keygen` and flashes a boot image with their
  key. From then on, releases don't install on that satellite, and going
  back to releases means flashing again.
- Signatures protect the two ways in that don't need root: updates from the
  network, where the release service isn't trusted (ADR 0022), and the
  control API, whose `InstallUpdate` takes a manifest URL from anyone on
  the LAN without a login.
- They don't protect against root on the satellite. Nothing checks the boot
  image: Kaeru boots whatever the boot partition holds (ADR 0019), so root can
  replace pinna-init with one that accepts any key.
- On a satellite installed from a release, root needs physical access. Release
  images carry no SSH key; the owner adds one from the USB serial console
  (ADR 0015).
- pinna-init reads `misc` before it chooses a slot. From `0x1000` up, `misc`
  was free; Pinna's slot state is at `0x40000` (ADR 0003), Kaeru's slot record
  at `0x360` (ADR 0019).

## Decision

### Which keys a satellite accepts

- pinna-init accepts a slot signed with a key compiled into it or with a
  trusted development key. The updater accepts a manifest and a root image
  signed with a key compiled into it or with a trusted development key.
- Builds always compile in `keys/release.pub`. When `secrets/release.pub`
  exists (`just keygen`), they compile it in as well, no longer in its place.
  A build that compiles in such a key is a **development build**. A release
  build still refuses `secrets/release.pub` (ADR 0015), so releases compile in
  only the project key.
- `pinna-image seal` refuses a key that isn't one of the compiled-in keys.
- A development build of the boot image therefore also accepts releases.

### Trusted keys in misc

- Up to 4 development keys, stored in `misc` at `0x41000` in one 4 KiB block:
  the magic `PINNAKEY`, a format version (1), the number of keys, the 32-byte
  ed25519 public keys, and a crc32 (IEEE) over the record. Integers are
  little-endian. *(default: offset and count)*
- It is written like the slot state (ADR 0003): with flush, page-cache drop
  and readback. A missing or invalid record means no trusted keys.

### Adding and removing a key

- Only root on the satellite adds or removes a key:
  `pinnactl trust add <key>`, `pinnactl trust remove <key>`,
  `pinnactl trust list`. The key is written as in `secrets/release.pub`.
- No RPC, no dashboard control and no Home Assistant entity adds or removes a
  key. The dashboard has no login, and anyone on the LAN could otherwise
  install a system of their own on a device with a microphone. Like
  `pinnad.toml`, this is for developers, not owners.
- `just trust` adds `secrets/release.pub` to a satellite over SSH, the way
  `just push` reaches it. *(default)*
- `pinnactl trust remove` refuses to remove the key that signed the slot that
  boots next.

### Seeing it

- The dashboard's Updates page says when the satellite trusts a development
  key, with the first 8 hex digits of each key. `GetUpdates` reports the same.
- pinna-init logs which key verified the slot it boots: the project key, or a
  development key with its first 8 hex digits.

### Factory reset

- A factory reset (ADR 0010) removes the trusted keys. It is refused while the
  slot that boots next is signed with a development key, with the message
  "This satellite runs a development build. Install a release first."
  *(default)*

## Consequences

- Contributors run root images of their own on a satellite installed from a
  release, without flashing anything, and go back by installing a release.
  Only kernel and pinna-init changes still need USB.
- Root on the satellite can add a key. Root could already replace pinna-init,
  so this gives it nothing new.
- A satellite that trusts a development key also accepts that key's images
  from the LAN, through the control API. If the key leaks, anyone on that LAN
  can install a system of their own on the satellite until the key is removed.
- `misc` holds a third record of Pinna's.
- `just keygen` no longer means a key change for the satellite: development
  builds accept releases, and releases accept development builds once their
  key is trusted.
