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

# root filesystem image (erofs) via mkosi
image: wmt rust-arm
    scripts/assemble-rootfs-extra.sh
    # mkosi bootstraps its Debian tools tree with the host's keyring; NixOS has none in /usr/share.
    install -Dm0644 -t mkosi/mkosi.sandbox/usr/share/keyrings "$PINNA_DEBIAN_KEYRINGS"/*.gpg
    install -Dm0644 mkosi/apt-sources.sources mkosi/mkosi.sandbox/etc/apt/sources.list.d/mkosi.sources
    cd mkosi && mkosi --machine-id "$(cat ../secrets/machine-id)" -f build

# boot image for boot_a: kernel + known-good DTB, no initramfs
bootimg:
    cargo run -q -p pinna-bootimg -- pack \
        --zimage {{build}}/kernel/arch/arm/boot/zImage \
        --dtb blobs/boot/libreecho-biscuit.dtb \
        --out {{build}}/pinna-boot.img
    cargo run -q -p pinna-bootimg -- info {{build}}/pinna-boot.img

# everything needed for a flash: kernel, boot image, root filesystem
all: kernel image bootimg
    ls -l {{build}}/pinna-boot.img {{out}}/pinna.root-arm.raw

# fetch openWakeWord models (CC BY-NC-SA, never committed) into blobs/
models:
    scripts/fetch-models.sh

# wake word: tract vs onnxruntime parity (needs `just models`)
wakeword-parity:
    cargo test --release -p pinna-wakeword --test parity -- --nocapture
