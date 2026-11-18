//! The satellite group (ADR 0026): satellites in standalone mode that pass
//! messages, run each other's device tools and share one member list, with
//! every call between them signed with the group's key.
//!
//! `model` holds the member list's rules and `seal` the group's id, key and
//! signatures. Neither does any I/O.

// Used by the group runtime (ADR 0026), added next.
#![allow(dead_code)]

pub mod model;
pub mod seal;
