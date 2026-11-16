#![allow(non_snake_case, reason = "The fields of our XML types are CIM's element names (`PowerState`).")]

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

pub use crate::net::{Connection, Redirection, RedirectionSession, decode_fields, encode_fields};
pub use alarms::Alarm;
pub use certificates::{Certificate, Tls};
pub use client::Client;
pub use ider::Ider;
pub use kvm::{Kvm, Screens};
pub use logs::AuditRecord;
pub use power::Recovery;
pub use settings::{
    Availability, Change, DnsUpdate, General, Ipv6Configuration, Ipv6Settings, Network, PowerPolicy, Settings,
};
pub use sol::Sol;
pub use status::Status;
