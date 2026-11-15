set shell := ["bash", "-euo", "pipefail", "-c"]

target_arm := "armv7-unknown-linux-musleabihf"
build := justfile_directory() / "build"

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
