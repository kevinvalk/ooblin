mod client;
mod fru;
mod sdr;
pub(crate) mod sel;
mod sol;

pub use client::{Change, Client, IpmiSettings, IpmiStatus, User};
pub use fru::Fru;
pub use sol::Sol;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

// We export how datagrams are framed and sealed because the mock's BMC has to do it alike.
pub use crate::net::ipmi::{Hash, Sealing, Session, message, unsealed};
use crate::{Error, Result, Target};

// The network functions of the IPMI 2.0 specification, which group its commands.
const CHASSIS: u8 = 0x00;
const SENSOR: u8 = 0x04;
const APPLICATION: u8 = 0x06;
const STORAGE: u8 = 0x0A;
const TRANSPORT: u8 = 0x0C;

const THIS_CHANNEL: u8 = 0x0E; // The channel a request came in on, so the one we speak on

/// What a server does when its power comes back after it was cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum PowerRestore {
    StayOff,
    Previous,
    PowerOn,
}

#[derive(Debug)]
pub struct Chassis {
    pub is_on: bool,
    /// Whether its identify light is on, `None` where the BMC does not say.
    pub is_identifying: Option<bool>,
    pub restore: Option<PowerRestore>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct SolSettings {
    /// Whether SOL may be used.
    pub is_enabled: bool,
    /// How fast the server's serial port runs in bits a second, `None` for the BMC's own setting.
    pub bit_rate: Option<u32>,
}

/// A BMC's own network settings: DHCP, or else these IPv4 addresses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "BmcNetwork")]
pub struct Network {
    /// `None` where the BMC's flavor does not set a host name, as over IPMI alone.
    pub hostname: Option<String>,
    pub is_dhcp: bool,
    pub address: String,
    pub subnet_mask: String,
    pub gateway: String,
    /// `None` where the BMC's flavor does not set a DNS server, as over IPMI alone.
    pub dns: Option<String>,
    /// `None` for no VLAN.
    pub vlan: Option<u16>,
}

impl Network {
    /// Fails with `Error::Invalid` on settings a BMC does not take.
    pub fn reject_if_invalid(&self) -> Result<()> {
        let ipv4 = |field: &str, address: &str| match address.parse::<core::net::Ipv4Addr>() {
            Ok(_) => Ok(()),
            Err(_) => Err(Error::Invalid(format!("The {field} isn't an IPv4 address."))),
        };

        if !self.is_dhcp {
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

        let hostname = self.hostname.as_deref().unwrap_or_default();
        if hostname.len() > 63 || !hostname.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            return Err(Error::Invalid("A host name is up to 63 letters, digits, dashes and dots.".into()));
        }

        Ok(())
    }
}

// The bit rates SOL runs at, by their IPMI number.
const BIT_RATES: [(u8, u32); 5] = [(6, 9600), (7, 19200), (8, 38400), (9, 57600), (10, 115_200)];

/// We open a session to `target` for one piece of `work` and close it again right after, because a BMC only has a few
/// sessions to give.
pub async fn with_session<T>(target: &Target, work: impl AsyncFnOnce(&Session) -> Result<T>) -> Result<T> {
    let session = Session::open(target).await?;

    let result = work(&session).await;
    session.close().await;

    result
}

pub async fn chassis(session: &Session) -> Result<Chassis> {
    let status = session.request(CHASSIS, 0x01, &[]).await?;
    let &[power, _, misc, ..] = &status[..] else {
        return Err(Error::Protocol("a short chassis status".into()));
    };

    Ok(Chassis {
        is_on: power & 1 != 0,
        // The light is lit for a while (1) or until it is turned off (2). OpenBMC says 3, which is reserved, until it
        // is first asked to identify.
        is_identifying: (misc & 0x40 != 0).then_some(matches!(misc >> 4 & 0x03, 1 | 2)),
        restore: [PowerRestore::StayOff, PowerRestore::Previous, PowerRestore::PowerOn]
            .get(usize::from(power >> 5 & 0x03))
            .copied(),
    })
}

/// `is_on` lights the identify light until we turn it off again, instead of for a while.
pub async fn identify(session: &Session, is_on: bool) -> Result<()> {
    session.request(CHASSIS, 0x04, &[0, u8::from(is_on)]).await.map(drop)
}

pub async fn set_power_restore(session: &Session, restore: PowerRestore) -> Result<()> {
    session.request(CHASSIS, 0x06, &[restore as u8]).await.map(drop)
}

pub async fn sol_settings(session: &Session) -> Result<SolSettings> {
    let parameter = async |number: u8| {
        let answer = session.request(TRANSPORT, 0x22, &[THIS_CHANNEL, number, 0, 0]).await?;
        answer.get(1).copied().ok_or_else(|| Error::Protocol(format!("SOL parameter {number} came empty")))
    };

    let is_enabled = parameter(1).await? & 1 != 0;
    let bit_rate_number = parameter(5).await? & 0x0F;
    let bit_rate = BIT_RATES.iter().find(|(number, _)| *number == bit_rate_number).map(|(_, bit_rate)| *bit_rate);

    Ok(SolSettings { is_enabled, bit_rate })
}

pub async fn set_sol_bit_rate(session: &Session, bit_rate: u32) -> Result<()> {
    let Some((number, _)) = BIT_RATES.iter().find(|(_, rate)| *rate == bit_rate) else {
        return Err(Error::Invalid(format!("SOL doesn't run at {bit_rate} bits a second.")));
    };

    // We set both the bit rate that lasts (5) and the one in use now (6).
    for parameter in [5, 6] {
        session.request(TRANSPORT, 0x21, &[THIS_CHANNEL, parameter, *number]).await?;
    }

    Ok(())
}
