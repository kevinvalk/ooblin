//! IPMI 2.0, which any BMC speaks whoever made it (see `net::ipmi` for its sessions): what the BMC flavors ask over it,
//! and `Client`, a BMC we know over IPMI alone. Commands from the IPMI 2.0 specification, its sensors (`sdr`), event
//! log (`sel`) and FRU (`fru`) too, each with what ipmitool knows that the specification doesn't say.

mod client;
pub mod fru;
pub mod sdr;
pub mod sel;
mod sol;

pub use client::{Change, Client, IpmiSettings, IpmiStatus, User};
pub use fru::Fru;
pub use sol::Sol;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How datagrams are framed and sealed, which a BMC does alike: for the mock's.
pub use crate::net::ipmi::{Hash, Sealing, Session, message, unsealed};
use crate::{Error, Result, Target};

const CHASSIS: u8 = 0x00;
const SENSOR: u8 = 0x04;
const APPLICATION: u8 = 0x06;
const STORAGE: u8 = 0x0A;
const TRANSPORT: u8 = 0x0C;

/// The channel a request came in on, so the one we speak on.
const THIS_CHANNEL: u8 = 0x0E;

/// What a server does when its power comes back after it was cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum PowerRestore {
    StayOff,
    Previous,
    PowerOn,
}

/// A BMC's chassis: whether the server is on, whether its identify light is on, `None` where it doesn't say, and its
/// power restore policy.
#[derive(Debug)]
pub struct Chassis {
    pub on: bool,
    pub identify: Option<bool>,
    pub restore: Option<PowerRestore>,
}

/// Its serial console's settings: whether SOL may be used, and how fast the server's serial port runs (bits a
/// second, `None` for the BMC's own setting).
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct SolSettings {
    pub enabled: bool,
    pub bit_rate: Option<u32>,
}

/// A BMC's own network settings: its host name and DNS server where its flavor sets them (`None` over IPMI alone),
/// DHCP or these IPv4 addresses, and its VLAN, `None` for none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "BmcNetwork")]
pub struct Network {
    pub hostname: Option<String>,
    pub dhcp: bool,
    pub address: String,
    pub subnet_mask: String,
    pub gateway: String,
    pub dns: Option<String>,
    pub vlan: Option<u16>,
}

impl Network {
    /// What a BMC takes: IPv4 addresses where they're set, a VLAN from 1 to 4094, a host name of letters, digits,
    /// dashes and dots.
    pub fn check(&self) -> Result<()> {
        let ipv4 = |what: &str, address: &str| match address.parse::<core::net::Ipv4Addr>() {
            Ok(_) => Ok(()),
            Err(_) => Err(Error::Invalid(format!("The {what} isn't an IPv4 address."))),
        };
        if !self.dhcp {
            ipv4("address", &self.address)?;
            ipv4("subnet mask", &self.subnet_mask)?;
            ipv4("gateway", &self.gateway)?;
        }
        if let Some(dns) = self.dns.as_deref().filter(|dns| !dns.is_empty()) {
            ipv4("DNS server", dns)?;
        }
        if self.vlan.is_some_and(|vlan| !(1..=4094).contains(&vlan)) {
            return Err(Error::Invalid("A VLAN is 1 to 4094.".into()));
        }
        let name = self.hostname.as_deref().unwrap_or_default();
        if name.len() > 63 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            return Err(Error::Invalid("A host name is up to 63 letters, digits, dashes and dots.".into()));
        }
        Ok(())
    }
}

/// The bit rates SOL runs at, by their IPMI number.
const BIT_RATES: [(u8, u32); 5] = [(6, 9600), (7, 19200), (8, 38400), (9, 57600), (10, 115_200)];

/// Opens a session to `target`, does `what` in it and closes it again: a BMC has only a few.
pub async fn with<T>(target: &Target, what: impl AsyncFnOnce(&Session) -> Result<T>) -> Result<T> {
    let session = Session::open(target).await?;
    let result = what(&session).await;
    session.close().await;
    result
}

pub async fn chassis(session: &Session) -> Result<Chassis> {
    let status = session.request(CHASSIS, 0x01, &[]).await?;
    let (power, misc) = (status.first().copied().unwrap_or(0), status.get(2).copied().unwrap_or(0));
    Ok(Chassis {
        on: power & 1 != 0,
        // Lit for a while (1) or until turned off (2). OpenBMC says 3, reserved, until it's first asked to identify.
        identify: (misc & 0x40 != 0).then_some(matches!(misc >> 4 & 0x03, 1 | 2)),
        restore: [PowerRestore::StayOff, PowerRestore::Previous, PowerRestore::PowerOn]
            .get(usize::from(power >> 5 & 0x03))
            .copied(),
    })
}

/// Lights the identify light until turned off, or turns it off.
pub async fn identify(session: &Session, on: bool) -> Result<()> {
    session.request(CHASSIS, 0x04, &[0, u8::from(on)]).await.map(drop)
}

pub async fn set_power_restore(session: &Session, restore: PowerRestore) -> Result<()> {
    session.request(CHASSIS, 0x06, &[restore as u8]).await.map(drop)
}

pub async fn sol_settings(session: &Session) -> Result<SolSettings> {
    let parameter = async |number: u8| {
        let answer = session.request(TRANSPORT, 0x22, &[THIS_CHANNEL, number, 0, 0]).await?;
        answer.get(1).copied().ok_or_else(|| Error::Protocol(format!("SOL parameter {number} came empty")))
    };
    let enabled = parameter(1).await? & 1 != 0;
    let rate = parameter(5).await? & 0x0F;
    let bit_rate = BIT_RATES.iter().find(|(number, _)| *number == rate).map(|(_, rate)| *rate);
    Ok(SolSettings { enabled, bit_rate })
}

/// Sets how fast the server's serial port runs for SOL, one of 9600, 19200, 38400, 57600 or 115200 bits a second.
pub async fn set_sol_bit_rate(session: &Session, bit_rate: u32) -> Result<()> {
    let Some((number, _)) = BIT_RATES.iter().find(|(_, rate)| *rate == bit_rate) else {
        return Err(Error::Invalid(format!("SOL doesn't run at {bit_rate} bits a second.")));
    };
    // Both the setting that lasts and the one in use now.
    for parameter in [5, 6] {
        session.request(TRANSPORT, 0x21, &[THIS_CHANNEL, parameter, *number]).await?;
    }
    Ok(())
}
