//! The satellite group (ADR 0026): satellites in standalone mode that pass
//! messages, run each other's device tools and share one member list, with
//! every call between them signed with the group's key.
//!
//! `model` holds the member list's rules and `seal` the group's id, key and
//! signatures. `persisted` is the group as the standalone files keep it, and
//! `view` what the dashboard shows of it. None of them does any I/O.
//!
//! The calls between members: `envelope` signs and opens them, `client`
//! makes them, `peer` serves them, `runtime` holds what is known of the
//! other members while pinnad runs, with the changes the dashboard makes,
//! and `exchange` merges the members' lists after every search.

pub mod client;
pub mod envelope;
pub mod exchange;
pub mod model;
pub mod peer;
pub mod persisted;
pub mod runtime;
pub mod seal;
pub mod view;

pub use persisted::{Joined, Persisted};
pub use runtime::{Group, Limits};

#[cfg(test)]
pub(crate) mod tests;
