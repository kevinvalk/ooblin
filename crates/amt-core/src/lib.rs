//! Intel AMT over TLS, without a webview. Every socket lives in `net`.

#![allow(non_snake_case, reason = "our XML types name their fields the way CIM does, `PowerState` say")]

mod alarms;
mod certificates;
mod client;
mod hardware;
mod ider;
mod info;
mod kvm;
mod logs;
mod net;
mod power;
mod redirection;
mod settings;
mod sol;
mod status;
pub mod wsman;

pub use alarms::Alarm;
pub use certificates::{Certificate, Tls};
pub use client::Client;
pub use hardware::{Battery, Bios, Disk, Hardware, Module, Motherboard, Processor, System};
pub use ider::Ider;
pub use info::Info;
pub use kvm::{Input, Kvm, Screens};
pub use logs::{AuditRecord, Event, Severity};
pub use net::{
    Connection, Found, RMCP_PORT, Redirection, Session, decode_fields, discover, encode_fields, fingerprint, nearby,
};
pub use power::{Power, PowerAction, PowerState, Recovery};
pub use redirection::{Consent, RemoteAccess};
pub use settings::{
    Availability, Change, DnsUpdate, General, Ipv6Configuration, Ipv6Settings, Network, PowerPolicy, Settings,
};
pub use sol::Sol;
pub use status::{Configuration, Ipv4, Ipv6, Port, Status};

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
    PinMismatch { expected: String, actual: String },
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
    /// AMT's `UnsupportedFeature` fault: this device doesn't do that, a class or a setting.
    #[error("this device doesn't support that")]
    Unsupported,
    #[error("unexpected response: {0}")]
    Protocol(String),
    #[error("that's turned off in the device's remote access settings")]
    Off,
    /// See `Client::start_consent`.
    #[error("someone at the device has to give consent first")]
    Consent,
    #[error("that's not the code the device shows")]
    WrongCode,
    #[error("{0}")]
    Invalid(String),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            // TLS says so in more words, with a link.
            std::io::ErrorKind::UnexpectedEof => Self::Transport("the device closed the connection".into()),
            _ => Self::Transport(error.to_string()),
        }
    }
}
