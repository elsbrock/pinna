//! Hardware access for Pinna: the LED ring, buttons, microphone switch and
//! identity data of the boards `pinna-board` describes (ADR 0032), re-exported
//! here as [`board`]. A driver takes its paths and key codes from the
//! [`board::Board`] it runs on. Real implementations talk to sysfs and evdev
//! only; mocks allow development on a laptop.

pub use pinna_board as board;

pub mod anim;
pub mod buttons;
pub mod ident;
pub mod led;
pub mod privacy;
pub mod virt;

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("{0} not found")]
    NotFound(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
