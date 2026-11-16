// NOTE: Each flavor has its own protocols (`amt`, `ipmi` and `redfish`, which a vendor's `supermicro` adds its own to),
// `Client` speaks to any of them and they all hand over the types of this file. Every socket lives in `net`.
pub mod amt;
mod cdrom;
mod client;
pub mod ipmi;
mod net;
pub mod redfish;
pub mod supermicro;

pub use client::{Client, Kvm, Media, Sol};
pub use net::{Found, RMCP_PORT, discover, fingerprint, nearby};

use core::net::Ipv6Addr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A device and how we log in to it. `host` may name a port (`host:port`, `[v6]:port`), as a BMC's web port can sit
/// behind a tunnel or be our mock. The ports of AMT and IPMI are fields of their own, which only tests change.
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub wsman_port: u16,
    pub redirection_port: u16,
    pub ipmi_port: u16,
    pub user: String,
    pub password: String,
}

impl Target {
    pub fn new(host: impl Into<String>, user: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            wsman_port: net::WSMAN_PORT,
            redirection_port: net::REDIRECTION_PORT,
            ipmi_port: RMCP_PORT,
            user: user.into(),
            password: password.into(),
        }
    }

    /// The bare `host` for IPMI, because a host that names its web port (`host:port`, `[v6]:port`) says nothing about
    /// the port of IPMI.
    pub fn ipmi_host(&self) -> &str {
        let host = match self.host.rsplit_once(':') {
            Some((host, port)) if port.parse::<u16>().is_ok() && (host.ends_with(']') || !host.contains(':')) => host,
            _ => &self.host,
        };

        host.trim_matches(['[', ']'])
    }

    /// `host` as a URL wants it, so an IPv6 address goes in brackets.
    fn authority(&self) -> String {
        match self.host.parse::<Ipv6Addr>() {
            Ok(_) => format!("[{}]", self.host),
            Err(_) => self.host.clone(),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A MAC address as we write them, `12:07:14:5F:37:F1`.
fn mac(mac: &str) -> String {
    mac.to_uppercase().replace('-', ":")
}

fn uuid(mut guid: [u8; 16]) -> String {
    // We write a system GUID as SMBIOS and the OS do, so its first three fields are little-endian.
    guid[0..4].reverse();
    guid[4..6].reverse();
    guid[6..8].reverse();

    let hex = format!("{:032X}", u128::from_be_bytes(guid));
    [&hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]].join("-")
}

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
    /// A Redfish request the BMC refused, with what its error body says went wrong, in its own words (DSP0266).
    #[error("the BMC refused that: {message}")]
    Redfish { status: u16, message: String },
    #[error("{method} failed with return value {code}")]
    Method { method: String, code: String },
    #[error("WS-Man fault: {0}")]
    Fault(String),
    /// AMT's `UnsupportedFeature` fault (a class or a setting), or a call that only another flavor has.
    #[error("this device doesn't support that")]
    Unsupported,
    #[error("unexpected response: {0}")]
    Protocol(String),
    #[error("that's turned off in the device's remote access settings")]
    Off,
    /// See `amt::Client::start_consent`.
    #[error("someone at the device has to give consent first")]
    Consent,
    #[error("that's not the code the device shows")]
    WrongCode,
    /// The device ended a session on purpose, so we do not open it again by ourselves: see `supermicro::Kvm::frame`
    /// and `ipmi::Sol::read`.
    #[error("the BMC ended the session: {0}")]
    Ended(&'static str),
    /// An IPMI command's completion code, or a session setup's status, that is not 0: see `net::ipmi`.
    #[error("the BMC refused that (IPMI code {0:#04X})")]
    Refused(u8),
    /// We show this as it is, so it has to be a sentence with its capital and period.
    #[error("{0}")]
    Invalid(String),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            // We say this ourselves, because TLS says it in more words and with a link.
            std::io::ErrorKind::UnexpectedEof => Self::Transport("the device closed the connection".into()),
            _ => Self::Transport(error.to_string()),
        }
    }
}

/// What a device is, which says how we speak to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Flavor {
    #[default]
    Amt,
    Supermicro,
    /// A BMC we know only over IPMI: any vendor's, or a Supermicro without Redfish.
    Ipmi,
    /// A BMC we know only over Redfish: any vendor's, with IPMI turned off say.
    Redfish,
}

/// What a login says about the device. Text it does not have is empty: AMT's own facts (`build`, `control_mode`) on a
/// BMC, the `fingerprint` over IPMI.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Info {
    pub flavor: Flavor,
    /// The host name, with its domain if it has one.
    pub name: String,
    pub version: String,
    pub build: String,
    /// "Admin" or "Client", only admin control mode redirects without asking someone at the device for consent.
    pub control_mode: String,
    /// The pinned certificate's, see `fingerprint`.
    pub fingerprint: String,
    /// What it lets us redirect, KVM being `None` where it has none (AMT's standard manageability).
    pub remote_access: RemoteAccess,
}

/// What may be redirected, each only while the listener is on too, and when someone at the device has to read out a
/// code first.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct RemoteAccess {
    /// `None` on a device without KVM.
    pub is_kvm: Option<bool>,
    pub is_sol: bool,
    pub is_ider: bool,
    pub consent: Consent,
    /// Only read: whether `consent` may change, which in client control mode it cannot.
    pub is_consent_fixed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Consent {
    None,
    Kvm,
    All,
}

/// The power state, and what the device offers to do about it right now.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Power {
    pub state: PowerState,
    pub actions: Vec<PowerAction>,
}

/// The power states the devices report, folded into what a person cares about and named the way they would say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum PowerState {
    On,
    Asleep,
    Hibernating,
    Off,
    Unknown,
}

/// The power changes a device can make, each flavor offering what it can. The `Soft` ones, `Sleep` and `Hibernate` ask
/// the OS, the `To` ones power on or reset into the BIOS setup, a network boot or the disk image in the virtual drive
/// (AMT's IDER, a BMC's virtual CD), once, or with the BIOS's screen on the serial console (AMT's `UseSOL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum PowerAction {
    On,
    SoftOff,
    Off,
    SoftReset,
    Reset,
    Cycle,
    Sleep,
    Hibernate,
    Nmi,
    OnToBios,
    OnToPxe,
    OnToImage,
    OnToSerial,
    ResetToBios,
    ResetToPxe,
    ResetToImage,
    ResetToSerial,
}

/// Everything the device knows about its hardware, gathered by the BIOS at boot. Only what is installed, no empty
/// memory slots or drive bays. Text the device left out is empty, numbers it left out are `None`.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Hardware {
    pub system: System,
    pub motherboard: Motherboard,
    pub bios: Bios,
    pub processors: Vec<Processor>,
    pub memory: Vec<Module>,
    pub disks: Vec<Disk>,
    pub batteries: Vec<Battery>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct System {
    pub model: String,
    pub manufacturer: String,
    pub version: String,
    pub serial_number: String,
    pub id: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Motherboard {
    pub product: String,
    pub manufacturer: String,
    pub version: String,
    pub serial_number: String,
    pub asset_tag: String,
    pub is_replaceable: bool,
}

/// `released` is a date, SMBIOS keeps no time.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Bios {
    pub vendor: String,
    pub version: String,
    pub released: String,
    pub features: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Processor {
    pub model: String,
    pub manufacturer: String,
    pub family: String,
    pub socket: String,
    pub mhz: Option<u32>,
    pub status: String,
    pub upgrade_method: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Module {
    pub slot: String,
    pub bytes: Option<u64>,
    pub mhz: Option<u32>,
    pub kind: String,
    pub form_factor: String,
    pub manufacturer: String,
    pub part_number: String,
    pub serial_number: String,
    pub asset_tag: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Disk {
    pub model: String,
    pub serial_number: String,
    pub bytes: Option<u64>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Battery {
    pub name: String,
    pub charge_percent: Option<u8>,
    pub design_capacity_mwh: Option<u32>,
}

/// A device's management port: a BMC's own, or AMT's wired one and maybe a wireless one.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Port {
    pub is_wireless: bool,
    pub mac: String,
    pub is_link_up: bool,
    pub mbps: Option<u64>,
    pub ipv4: Ipv4,
    /// `None` when IPv6 is off.
    pub ipv6: Option<Ipv6>,
}

/// The addresses the management port uses, from DHCP, from what was set (`Manual`) or on AMT shared with the OS.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Ipv4 {
    pub configuration: Configuration,
    pub address: String,
    pub subnet_mask: String,
    pub gateway: String,
    pub primary_dns: String,
    pub secondary_dns: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Configuration {
    Dhcp,
    /// A static address shared with the OS, which AMT takes from it.
    Os,
    Manual,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Ipv6 {
    pub addresses: Vec<String>,
    pub gateway: String,
    pub primary_dns: String,
    pub secondary_dns: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Event {
    /// Unix seconds.
    pub occurred_at: u32,
    pub source: String,
    pub description: String,
    pub severity: Severity,
}

/// A sensor's reading in its `unit` ("°C", "RPM", "V", "A", "W", "%" or the unit's name), `None` where it is absent or
/// unreadable, or a discrete sensor's asserted `state` in words; `low` and `high` are its critical thresholds.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Sensor {
    pub name: String,
    pub kind: SensorKind,
    pub reading: Option<f64>,
    pub unit: String,
    pub state: String,
    pub severity: Severity,
    pub low: Option<f64>,
    pub high: Option<f64>,
}

/// What a sensor measures, which says where it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum SensorKind {
    Temperature,
    Fan,
    Voltage,
    Current,
    Power,
    Other,
}

/// What can be seen of a server's front panel from here: whether its identify light (UID) is lit, and the last POST
/// code its BIOS wrote, each `None` where the BMC does not say.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Panel {
    pub is_identifying: Option<bool>,
    pub post_code: Option<u8>,
}

/// How bad an event or a reading is, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// What the webview sends: keys by their DOM `code` (where they sit, not what they type), the mouse with the DOM
/// `buttons` it holds and a notch of `scroll` up (negative) or down (positive), text to type, and letting go of every
/// key and mouse button still held, when the screen loses focus say.
#[derive(Debug, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(tag = "kind")]
pub enum Input {
    Key { code: String, is_down: bool },
    Pointer { x: u16, y: u16, buttons: u8, scroll: i8 },
    Text { text: String },
    Release,
}

// We give a session's goodbye this long, after which dropping the session hangs up all the same.
pub(crate) const GOODBYE: Duration = Duration::from_secs(2);
