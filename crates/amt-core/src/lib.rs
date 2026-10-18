//! Intel AMT over TLS, without a webview. Every socket lives in `net`.

mod client;
mod decode;
mod hardware;
mod info;
mod logs;
mod net;
mod power;
mod settings;
mod status;
pub mod wsman;

pub use client::{Client, Trust};
pub use hardware::{Battery, Bios, Disk, Hardware, Module, Motherboard, Processor, System};
pub use info::{ControlMode, Info};
pub use logs::{AuditRecord, Event, Severity};
pub use net::{Connection, Fingerprint, Redirection, Session};
pub use power::{Power, PowerAction, PowerState};
pub use settings::{Account, Settings};
pub use status::{Ipv4, Ipv6, Port, RemoteAccess, Status};

/// A device and how to log in to it. The ports only change for tests and tunnels.
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub wsman_port: u16,
    pub redirection_port: u16,
    pub user: String,
    pub password: String,
}

impl Target {
    pub fn new(host: impl Into<String>, user: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            wsman_port: net::WSMAN_PORT,
            redirection_port: net::REDIRECTION_PORT,
            user: user.into(),
            password: password.into(),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the device presented certificate {actual}, but {expected} is pinned")]
    PinMismatch { expected: Fingerprint, actual: Fingerprint },
    #[error("authentication failed, check the user name and password")]
    Unauthorized,
    #[error("{0}")]
    Transport(String),
    #[error("it didn't answer in time")]
    Timeout,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("{method} failed with return value {code}")]
    Method { method: String, code: String },
    #[error("WS-Man fault: {0}")]
    Fault(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
}
