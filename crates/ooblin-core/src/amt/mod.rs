//! Intel AMT: WS-Man on 16993, and the SOL, KVM and IDER redirection sessions on 16995.

#![allow(non_snake_case, reason = "our XML types name their fields the way CIM does, `PowerState` say")]

mod alarms;
mod certificates;
mod client;
mod hardware;
mod ider;
mod info;
mod kvm;
mod logs;
mod power;
mod redirection;
mod settings;
mod sol;
mod status;
pub mod wsman;

pub use crate::net::{Connection, Redirection, Session, decode_fields, encode_fields};
pub use alarms::Alarm;
pub use certificates::{Certificate, Tls};
pub use client::Client;
pub use ider::Ider;
pub use kvm::{Kvm, Screens};
pub use logs::AuditRecord;
pub use power::Recovery;
pub use settings::{Availability, DnsUpdate, General, Ipv6Configuration, Ipv6Settings, Network, PowerPolicy, Settings};
pub use sol::Sol;
