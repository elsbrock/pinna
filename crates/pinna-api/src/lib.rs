//! Generated ConnectRPC types, server traits and clients for Pinna's local control API
//! (`proto/pinna/v1/device.proto`), standalone mode's setup
//! (`proto/pinna/v1/assistant.proto`) and the calls between the satellites of a
//! group (`proto/pinna/v1/peer.proto`, ADR 0026), and the people the satellite
//! recognizes (`proto/pinna/v1/people.proto`, ADR 0038).

#[allow(clippy::all, dead_code, unreachable_pub)]
mod generated {
    connectrpc::include_generated!();
}

pub use generated::pinna::v1::*;
