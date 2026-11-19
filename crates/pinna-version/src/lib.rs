//! The version the binaries report: the release they were built for (ADR 0022).
//!
//! The build sets it in `PINNA_VERSION`, as the release workflow does. A build
//! without it reports the workspace's crate version, which says nothing about
//! a release. Owners see the version of the image that booted, which pinnad
//! reads from its signed footer; this is for the binaries' own messages.

/// `PINNA_VERSION` at build time, else the crate version.
pub const VERSION: &str = match option_env!("PINNA_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};
