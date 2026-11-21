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

# the virtual Echo Dot: the x86_64 image under KVM, driven from its dashboard (ADR 0014; needs: just kernel-x86 image-x86)
dev-vm: initramfs-x86
    scripts/dev-vm.sh

# a Home Assistant for the virtual Echo Dot on this computer: HA, Whisper and Piper in containers (`just dev-ha stop` removes them)
dev-ha *args:
    scripts/dev-ha.sh {{args}}

# fresh binaries into the running virtual Echo Dot, like `just push`, until it restarts
push-vm: rust-x86
    PINNA_TARGET=x86_64-unknown-linux-musl PINNA_SYSEXT_DIR=/run/extensions PINNA_HOST=root@127.0.0.1 \
    PINNA_SSH="ssh -p ${SSH_PORT:-2223} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR" \
        scripts/push-dev.sh

# x86_64 static binaries for the VM smoke-test image
rust-x86:
    cargo zigbuild --release --target x86_64-unknown-linux-musl --workspace --bins

# ESPHome native API conformance (aioesphomeapi 46.2.0 against the demo example)
esphome-conformance:
    cargo build -p pinna-esphome --example demo
    uv run --with aioesphomeapi==46.2.0 --with pytest --with pytest-asyncio pytest crates/pinna-esphome/tests/aio -q

# Sendspin interoperability (ADR 0027): a pinnad on free ports, so beside a dev one, against aiosendspin as pinned by Music Assistant 2.10.7 and 2.11.0b9
sendspin-conformance:
    cargo build --release -p pinnad
    uv run --with 'aiosendspin[server]==10.2.0' --with av==16.1.0 --with pytest --with pytest-asyncio pytest tests/sendspin -q
    uv run --with 'aiosendspin[server]==10.3.0' --with av==19.0.2 --with pytest --with pytest-asyncio pytest tests/sendspin -q

# the release service's tests (ADR 0022): recorded GitHub answers, no accounts
updates-test:
    node --test 'services/updates/test/*.test.ts'

# kernel: build + package as build/debs/linux-image-*.deb
kernel: firmware
    kernel/build.sh

# kernel with kernel/debug.config (ftrace, debugfs, sysrq, core dumps) for dev images
kernel-debug: firmware
    DEBUG=1 kernel/build.sh

# static MT8163 WMT helpers (Wi-Fi/BT power-up)
wmt:
    mkdir -p {{build}}/wmt
    for t in wmt_stock_compat wmt_launcher wmt_configure wmt_bt_on; do \
        zig cc -target arm-linux-musleabihf -mcpu=cortex_a53 -static -Os -o {{build}}/wmt/$t tools/wmt/$t.c; \
    done

# your own signing key: secrets/release.{key,pub}, compiled in instead of keys/release.pub (ADR 0015)
keygen:
    cargo run -q -p pinna-image -- keygen --secret secrets/release.key --public secrets/release.pub

# dashboard: static files for /usr/share/pinna/web (dashboard/dist)
web:
    cd dashboard && npm ci && npm run build

# dashboard checks: design tokens, generated API code, unit tests, svelte-check
web-check:
    cd dashboard && npm ci && npm run check

# root filesystem: erofs via mkosi, sealed into a signed slot image build/pinna.root.img
image: models firmware wmt rust-arm ladspa web
    scripts/assemble-rootfs-extra.sh
    scripts/build-root.sh

# x86_64 image for KVM smoke tests (same userland, x86_64 binaries) -> build/pinna-x86.root.img
image-x86: models rust-x86 ladspa web
    scripts/assemble-rootfs-extra.sh --arch x86_64
    scripts/build-root.sh --arch x86_64

# initramfs with pinna-init and busybox for rescue mode (armv7: device and QEMU) -> build/initramfs.cpio
initramfs:
    cargo zigbuild --release --target {{target_arm}} -p pinna-init
    mkdir -p {{build}}
    cargo run -q -p pinna-bootimg -- initramfs --init target/{{target_arm}}/release/pinna-init \
        --busybox "$(scripts/fetch-busybox.sh armhf)" --out {{build}}/initramfs.cpio

# initramfs for the x86_64 VM -> build/initramfs-x86.cpio
initramfs-x86:
    cargo zigbuild --release --target x86_64-unknown-linux-musl -p pinna-init
    mkdir -p {{build}}
    cargo run -q -p pinna-bootimg -- initramfs --init target/x86_64-unknown-linux-musl/release/pinna-init \
        --busybox "$(scripts/fetch-busybox.sh amd64)" --out {{build}}/initramfs-x86.cpio

# boot image for boot_a: kernel + DTB from the kernel build + initramfs (pinna-init)
# The Echo Dot (2nd gen) boots the radar-puffin device tree (the Echo 2nd gen's);
# it decompiles identically to the DTB in LibreEcho's known-good boot_a, which it is ported from.
bootimg: initramfs
    cargo run -q -p pinna-bootimg -- pack \
        --zimage {{build}}/kernel/arch/arm/boot/zImage \
        --dtb {{build}}/kernel/arch/arm/boot/dts/mediatek/mt8163-radar-puffin.dtb \
        --ramdisk {{build}}/initramfs.cpio \
        --out {{build}}/pinna-boot.img
    cargo run -q -p pinna-bootimg -- info {{build}}/pinna-boot.img

# boot image for the Echo Dot (3rd gen) in LK's 64-bit layout (ADR 0031; not yet booted on one)
bootimg-mt8516: initramfs
    cargo run -q -p pinna-bootimg -- pack \
        --image-gz {{build}}/kernel-mt8516/arch/arm64/boot/Image.gz \
        --dtb {{build}}/kernel-mt8516/arch/arm64/boot/dts/mediatek/mt8516-amazon-donut.dtb \
        --ramdisk {{build}}/initramfs.cpio \
        --out {{build}}/pinna-boot-mt8516.img
    cargo run -q -p pinna-bootimg -- info {{build}}/pinna-boot-mt8516.img

# everything needed for a flash: kernel, wake word models, root filesystem, boot image
all: kernel models image bootimg
    ls -l {{build}}/pinna-boot.img {{build}}/pinna.root.img

# release artifacts: build/release/{pinna-<version>.root.img, pinna-boot.img, manifest.json(.sig)}
# PINNA_SUMMARY, PINNA_NOTES_URL and PINNA_MODELS (space-separated) go into the signed manifest (ADR 0022)
release: image bootimg
    #!/usr/bin/env bash
    set -euo pipefail
    v=$(cargo run -q --release -p pinna-image -- info {{build}}/pinna.root.img | sed -n 's/^version: //p')
    rm -rf {{build}}/release
    mkdir -p {{build}}/release
    cp --reflink=auto {{build}}/pinna.root.img {{build}}/release/pinna-$v.root.img
    cp {{build}}/pinna-boot.img {{build}}/release/pinna-boot.img
    cargo run -q --release -p pinna-image -- verify {{build}}/release/pinna-$v.root.img
    args=()
    [ -z "${PINNA_SUMMARY:-}" ] || args+=(--summary "$PINNA_SUMMARY")
    [ -z "${PINNA_NOTES_URL:-}" ] || args+=(--notes-url "$PINNA_NOTES_URL")
    for m in ${PINNA_MODELS:-}; do args+=(--model "$m"); done
    cargo run -q --release -p pinna-image -- manifest --boot {{build}}/release/pinna-boot.img \
        --out {{build}}/release "${args[@]}" {{build}}/release/pinna-$v.root.img
    install -m0755 scripts/flash-gen2.sh {{build}}/release/flash-gen2.sh
    (cd {{build}}/release && sha256sum -- pinna-*.root.img pinna-boot.img manifest.json manifest.json.sig flash-gen2.sh > SHA256SUMS)
    ls -l {{build}}/release
    cat {{build}}/release/manifest.json

# fetch the wake word models (openWakeWord CC BY-NC-SA, Okay Nabu from an Apache-2.0 repository; never committed) into blobs/
models:
    scripts/fetch-models.sh

# fetch Amazon's Wi-Fi/BT firmware and microphone FPGA bitstream (no license; never committed) into blobs/firmware/
firmware:
    scripts/fetch-firmware.sh

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

# rescue mode under QEMU: disks without a root or without slots stay in rescue (needs: just kernel-qemu)
vm-rescue-test: initramfs
    tests/vm/rescue-test.sh

# the same on x86_64 (needs: just kernel-x86)
vm-rescue-test-x86: initramfs-x86
    ARCH=x86_64 tests/vm/rescue-test.sh

# A/B update + rollback under QEMU: N -> N+1 -> broken N+2 rolls back (needs: just kernel-qemu image)
vm-update-test: initramfs
    tests/vm/update-test.sh

# QEMU virt variant of the kernel for vm-test
kernel-qemu:
    kernel/build.sh qemu

# x86_64 variant of the kernel for vm-test-x86
kernel-x86:
    kernel/build.sh x86

# arm64 QEMU virt variant of the kernel for vm-test-arm64 (ADR 0031)
kernel-qemu-arm64:
    kernel/build.sh qemu-arm64

# the MT8516 Echos' arm64 kernel and the Echo Dot (3rd gen) device tree (ADR 0031)
kernel-mt8516:
    kernel/build.sh mt8516

# boot the x86_64 image under KVM and check it (needs: just kernel-x86 image-x86)
vm-test-x86: initramfs-x86
    ARCH=x86_64 tests/vm/boot-test.sh

# boot the armhf image on the arm64 kernel, as 32-bit EL0 (ADR 0031; needs: just kernel-qemu-arm64 image)
vm-test-arm64: initramfs
    ARCH=arm64 tests/vm/boot-test.sh

# the setup hotspot on x86_64 with simulated radios, driven over ssh (ADR 0028; needs: just kernel-x86 image-x86, SSH_KEY)
vm-hotspot-test: initramfs-x86
    tests/vm/hotspot-test.sh

# kernel config assertions for every variant (no build)
kernel-config:
    for v in device qemu qemu-arm64 mt8516 mt8519 x86; do CHECK_ONLY=1 kernel/build.sh $v; done

# boot image round trips: a saved boot_a from before Pinna and our own
bootimg-roundtrip:
    if [ -s blobs/boot/boot_a-previous.img ]; then cargo run -q -p pinna-bootimg -- roundtrip blobs/boot/boot_a-previous.img; fi
    cargo run -q -p pinna-bootimg -- roundtrip {{build}}/pinna-boot.img

# the plan's full verification: unit/lint, boot images, kernel configs,
# ESPHome conformance, wake word parity, end-to-end voice flow, VM boot gates and A/B update/rollback
test: check web-check bootimg-roundtrip kernel-config esphome-conformance wakeword-parity e2e vm-test vm-test-x86 vm-update-test
