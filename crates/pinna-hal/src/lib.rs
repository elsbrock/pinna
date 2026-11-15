//! Hardware access for Pinna (Echo Dot 2nd gen "biscuit"): LED ring, buttons,
//! mic privacy switch and identity data. Real implementations talk to sysfs
//! and evdev only; mocks allow development on a laptop.

pub mod anim;
pub mod buttons;
pub mod ident;
pub mod led;
pub mod privacy;

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("{0} not found")]
    NotFound(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
