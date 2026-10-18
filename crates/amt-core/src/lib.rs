//! Intel AMT over TLS, without a webview. Every socket lives in `net`.

mod client;
mod info;
mod net;
mod wsman;

pub use client::{Client, Trust};
pub use info::{ControlMode, Info};
pub use net::Fingerprint;
pub use wsman::Instance;

use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Target {
    pub host: String,
    #[serde(default = "wsman_port")]
    pub port: u16,
    pub user: String,
    pub password: String,
}

fn wsman_port() -> u16 {
    net::WSMAN_PORT
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
    #[error("HTTP {0}")]
    Http(u16),
    #[error("WS-Man fault: {0}")]
    Fault(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
}
