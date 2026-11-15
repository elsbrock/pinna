//! Chooses the release public key that pinna-init, pinna-bootctl and
//! pinnactl compile in (ADR 0015): `secrets/release.pub` when it exists (a
//! developer's own key, from `just keygen`), else the project key in
//! `keys/release.pub`.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let local = root.join("secrets/release.pub");
    let project = root.join("keys/release.pub");
    let chosen = if local.is_file() { &local } else { &project };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("release.pub");
    std::fs::copy(chosen, &out).unwrap_or_else(|e| panic!("{}: {e}", chosen.display()));
    println!("cargo:rerun-if-changed={}", project.display());
    println!("cargo:rerun-if-changed={}", root.join("secrets").display());
    println!(
        "cargo:rustc-env=PINNA_RELEASE_PUB_SOURCE={}",
        chosen.display()
    );
}
