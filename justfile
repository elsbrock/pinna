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

# fetch openWakeWord models (CC BY-NC-SA, never committed) into blobs/
models:
    scripts/fetch-models.sh

# wake word: tract vs onnxruntime parity (needs `just models`)
wakeword-parity:
    cargo test --release -p pinna-wakeword --test parity -- --nocapture
