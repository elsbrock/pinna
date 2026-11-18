//! The satellite group (ADR 0026): satellites in standalone mode that pass
//! messages, run each other's device tools and share one member list, with
//! every call between them signed with the group's key.
//!
//! `model` holds the member list's rules and `seal` the group's id, key and
//! signatures. `persisted` is the group as the standalone files keep it, and
//! `view` what the dashboard shows of it. None of them does any I/O.

pub mod model;
pub mod persisted;
pub mod seal;
pub mod view;

pub use persisted::{Joined, Persisted};
