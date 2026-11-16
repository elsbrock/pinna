//! The pairing agent: phones pair only while the pairing window is open.
//!
//! It registers as NoInputNoOutput, so phones pair without a code ("Just
//! Works"). BlueZ asks it to authorize a pairing or a new phone's
//! connection; a paired phone is trusted and connects without asking.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use zbus::zvariant::OwnedObjectPath;

pub const PATH: &str = "/org/pinna/bluetooth/agent";
pub const CAPABILITY: &str = "NoInputNoOutput";

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

pub struct Agent {
    /// The pairing window is open.
    pub window: Arc<AtomicBool>,
}

impl Agent {
    fn allow(&self, what: &str, device: &OwnedObjectPath) -> Result<(), AgentError> {
        if self.window.load(Ordering::Relaxed) {
            tracing::info!("{what} for {}: accepted", device.as_str());
            Ok(())
        } else {
            tracing::info!(
                "{what} for {}: refused, the pairing window is closed",
                device.as_str()
            );
            Err(AgentError::Rejected("the pairing window is closed".into()))
        }
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}

    fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, AgentError> {
        // A legacy phone without Secure Simple Pairing: the usual fixed code.
        self.allow("PIN code", &device).map(|()| "0000".into())
    }

    fn display_pin_code(
        &self,
        device: OwnedObjectPath,
        _pincode: String,
    ) -> Result<(), AgentError> {
        self.allow("PIN code", &device)
    }

    fn request_passkey(&self, device: OwnedObjectPath) -> Result<u32, AgentError> {
        self.allow("passkey", &device).map(|()| 0)
    }

    fn display_passkey(&self, _device: OwnedObjectPath, _passkey: u32, _entered: u16) {}

    fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        _passkey: u32,
    ) -> Result<(), AgentError> {
        self.allow("pairing", &device)
    }

    fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), AgentError> {
        self.allow("pairing", &device)
    }

    fn authorize_service(&self, device: OwnedObjectPath, _uuid: String) -> Result<(), AgentError> {
        self.allow("connection", &device)
    }

    fn cancel(&self) {}
}
