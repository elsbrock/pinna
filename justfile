set shell := ["bash", "-euo", "pipefail", "-c"]

target_arm := "armv7-unknown-linux-musleabihf"
build := justfile_directory() / "build"
out := justfile_directory() / "mkosi/mkosi.output"

# list tasks
default:
    @just --list

# host checks: fmt, clippy, tests
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# cross-build all Rust binaries for the device (static musl)
rust-arm:
    cargo zigbuild --release --target {{target_arm}} --workspace --bins

# glibc LADSPA plugin for Debian's PipeWire (armhf device + x86_64 VM)
ladspa:
    cargo zigbuild --release -p pinna-ladspa --target armv7-unknown-linux-gnueabihf.2.36
    cargo zigbuild --release -p pinna-ladspa --target x86_64-unknown-linux-gnu.2.36

# hardware gate against a running device (PINNA_HOST, PINNA_SSH, PINNA_API_KEY, THROUGHPUT_URL)
test-device:
    tests/device/test-device.sh

# put fresh binaries on a running device as a sysext overlay and restart pinnad
push: rust-arm
    scripts/push-dev.sh

# x86_64 static binaries for the VM smoke-test image
rust-x86:
    cargo zigbuild --release --target x86_64-unknown-linux-musl --workspace --bins

# ESPHome native API conformance (aioesphomeapi 46.2.0 against the demo example)
esphome-conformance:
    cargo build -p pinna-esphome --example demo
    uv run --with aioesphomeapi==46.2.0 --with pytest --with pytest-asyncio pytest crates/pinna-esphome/tests/aio -q

# kernel: build + package as build/debs/linux-image-*.deb
kernel:
    kernel/build.sh

# static MT8163 WMT helpers (Wi-Fi/BT power-up)
wmt:
    mkdir -p {{build}}/wmt
    for t in wmt_stock_compat wmt_launcher wmt_configure wmt_bt_on; do \
        zig cc -target arm-linux-musleabihf -mcpu=cortex_a53 -static -Os -o {{build}}/wmt/$t tools/wmt/$t.c; \
    done

# release signing key: secrets/release.key (created if missing, never committed) -> keys/release.pub
keygen:
    cargo run -q -p pinna-image -- keygen --secret secrets/release.key --public keys/release.pub

# root filesystem: erofs via mkosi, sealed into a signed slot image build/pinna.root.img
image: wmt rust-arm ladspa
    scripts/assemble-rootfs-extra.sh
    scripts/build-root.sh

# x86_64 image for KVM smoke tests (same userland, x86_64 binaries) -> build/pinna-x86.root.img
image-x86: rust-x86 ladspa
    scripts/assemble-rootfs-extra.sh --arch x86_64
    scripts/build-root.sh --arch x86_64

# initramfs with pinna-init (armv7: device and QEMU) -> build/initramfs.cpio
initramfs:
    cargo zigbuild --release --target {{target_arm}} -p pinna-init
    mkdir -p {{build}}
    cargo run -q -p pinna-bootimg -- initramfs --init target/{{target_arm}}/release/pinna-init --out {{build}}/initramfs.cpio

# initramfs for the x86_64 VM -> build/initramfs-x86.cpio
initramfs-x86:
    cargo zigbuild --release --target x86_64-unknown-linux-musl -p pinna-init
    mkdir -p {{build}}
    cargo run -q -p pinna-bootimg -- initramfs --init target/x86_64-unknown-linux-musl/release/pinna-init --out {{build}}/initramfs-x86.cpio

# boot image for boot_a: kernel + DTB from the kernel build + initramfs (pinna-init)
# The Echo Dot (2nd gen) boots the radar-puffin device tree (the Echo 2nd gen's);
# it decompiles identically to the DTB in the known-good LibreEcho boot_a.
bootimg: initramfs
    cargo run -q -p pinna-bootimg -- pack \
        --zimage {{build}}/kernel/arch/arm/boot/zImage \
        --dtb {{build}}/kernel/arch/arm/boot/dts/libreecho-radar-puffin.dtb \
        --ramdisk {{build}}/initramfs.cpio \
        --out {{build}}/pinna-boot.img
    cargo run -q -p pinna-bootimg -- info {{build}}/pinna-boot.img

# everything needed for a flash: kernel, boot image, root filesystem
all: kernel image bootimg
    ls -l {{build}}/pinna-boot.img {{build}}/pinna.root.img

# release artifacts: build/release/{pinna-<version>.root.img, pinna-boot.img, manifest.json}
release: image bootimg
    #!/usr/bin/env bash
    set -euo pipefail
    v=$(cargo run -q --release -p pinna-image -- info {{build}}/pinna.root.img | sed -n 's/^version: //p')
    rm -rf {{build}}/release
    mkdir -p {{build}}/release
    cp --reflink=auto {{build}}/pinna.root.img {{build}}/release/pinna-$v.root.img
    cp {{build}}/pinna-boot.img {{build}}/release/pinna-boot.img
    cargo run -q --release -p pinna-image -- verify {{build}}/release/pinna-$v.root.img
    cargo run -q --release -p pinna-image -- manifest {{build}}/release/pinna-$v.root.img > {{build}}/release/manifest.json
    ls -l {{build}}/release
    cat {{build}}/release/manifest.json

# fetch openWakeWord models (CC BY-NC-SA, never committed) into blobs/
models:
    scripts/fetch-models.sh

# wake word: tract vs onnxruntime parity (needs `just models`)
wakeword-parity:
    cargo test --release -p pinna-wakeword --test parity -- --nocapture

# end-to-end: pinnad + real wake word on a WAV fixture, aioesphomeapi playing Home Assistant
e2e:
    cargo build --release -p pinnad
    uv run --with aioesphomeapi==46.2.0 --with pytest pytest tests/e2e -q -s

# boot the real image under QEMU and check it (needs: just kernel-qemu image)
vm-test: initramfs
    tests/vm/boot-test.sh

# A/B update + rollback under QEMU: N -> N+1 -> broken N+2 rolls back (needs: just kernel-qemu image)
vm-update-test: initramfs
    tests/vm/update-test.sh

# QEMU virt variant of the kernel for vm-test
kernel-qemu:
    kernel/build.sh qemu

# x86_64 variant of the kernel for vm-test-x86
kernel-x86:
    kernel/build.sh x86

# boot the x86_64 image under KVM and check it (needs: just kernel-x86 image-x86)
vm-test-x86: initramfs-x86
    ARCH=x86_64 tests/vm/boot-test.sh

# kernel config assertions for every variant (no build)
kernel-config:
    for v in device qemu x86; do CHECK_ONLY=1 kernel/build.sh $v; done

# boot image round trips: the known-good LibreEcho boot_a and our own
bootimg-roundtrip:
    if [ -s blobs/boot/boot_a-libreecho.img ]; then cargo run -q -p pinna-bootimg -- roundtrip blobs/boot/boot_a-libreecho.img; fi
    cargo run -q -p pinna-bootimg -- roundtrip {{build}}/pinna-boot.img

# the plan's full verification: unit/lint, boot images, kernel configs,
# ESPHome conformance, wake word parity, end-to-end voice flow, VM boot gates and A/B update/rollback
test: check bootimg-roundtrip kernel-config esphome-conformance wakeword-parity e2e vm-test vm-test-x86 vm-update-test
