//! A BMC we know over IPMI alone, any vendor's: each call a session of its own (see `with`), as a BMC has only a few.

use core::net::Ipv6Addr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    APPLICATION, CHASSIS, Fru, Network, PowerRestore, Session, Sol, SolSettings, TRANSPORT, chassis, fru, sdr, sel,
    set_power_restore, set_sol_bit_rate, sol_settings, with,
};
use crate::{
    Configuration, Consent, Error, Event, Flavor, Info, Ipv4, Ipv6, Panel, Port, Power, PowerAction, PowerState,
    RemoteAccess, Result, Sensor, Target, mac,
};

pub struct Client {
    target: Target,
}

/// What the BMC says it is, and its own network port. `manufacturer` by name where we know the vendor, else its IANA
/// number, blank where it's unset; `system_id` is the system GUID as SMBIOS and the OS write it.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct IpmiStatus {
    pub info: Info,
    pub power: PowerState,
    pub manufacturer: String,
    pub product_id: u16,
    pub ipmi_version: String,
    pub system_id: String,
    pub ports: Vec<Port>,
}

/// What can be set over IPMI: the BMC's network, the server's power restore policy, its serial console and its FRU, and
/// the BMC's users to read. `None` where the BMC doesn't say.
#[derive(Debug, Default, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct IpmiSettings {
    pub network: Option<Network>,
    pub power_restore: Option<PowerRestore>,
    pub sol: Option<SolSettings>,
    pub fru: Option<Fru>,
    pub users: Vec<User>,
}

/// A user slot with a name: its privilege ("Administrator", "Operator", "User", "Callback", "No access") and whether
/// it's enabled, where the BMC says.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct User {
    pub id: u8,
    pub name: String,
    pub privilege: String,
    pub enabled: Option<bool>,
}

/// A change to what IPMI sets, see `Client::change`.
#[derive(Debug, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "IpmiChange")]
pub enum Change {
    Network(Network),
    PowerRestore(PowerRestore),
    SolBitRate(u32),
    Fru(Fru),
}

/// What Get Device ID says: the firmware as `4.07`, the IPMI version, the manufacturer's IANA number and the product.
struct Device {
    firmware: String,
    ipmi_version: String,
    manufacturer: u32,
    product_id: u16,
}

impl Client {
    pub fn new(target: &Target) -> Self {
        Self { target: target.clone() }
    }

    /// A login: the BMC says who it is. It has no name of its own over IPMI, nor a certificate.
    pub async fn info(&self) -> Result<Info> {
        let (device, sol) =
            with(&self.target, async |session| Ok((device(session).await?, sol_settings(session).await.ok()))).await?;
        Ok(info(&device, sol.is_some_and(|sol| sol.enabled)))
    }

    /// Its firmware's version as Get Device ID says it, `4.07`.
    pub async fn firmware(&self) -> Result<String> {
        Ok(with(&self.target, device).await?.firmware)
    }

    pub async fn status(&self) -> Result<IpmiStatus> {
        with(&self.target, async |session| {
            let device = device(session).await?;
            let power = if chassis(session).await?.on { PowerState::On } else { PowerState::Off };
            let guid = session.request(APPLICATION, 0x37, &[]).await.unwrap_or_default();
            let sol = sol_settings(session).await.ok();
            Ok(IpmiStatus {
                info: info(&device, sol.is_some_and(|sol| sol.enabled)),
                power,
                manufacturer: manufacturer(device.manufacturer),
                product_id: device.product_id,
                ipmi_version: device.ipmi_version,
                system_id: guid.try_into().map(crate::uuid).unwrap_or_default(),
                ports: lan(session).await.into_iter().collect(),
            })
        })
        .await
    }

    /// IPMI turns power on and off, cycles it, resets and sends an NMI, and asks the OS to shut down through ACPI,
    /// but has no way to ask it to restart.
    pub async fn power(&self) -> Result<Power> {
        use PowerAction::*;
        let (state, actions) = match with(&self.target, chassis).await?.on {
            true => (PowerState::On, vec![SoftOff, Off, Reset, Cycle, Nmi, ResetToBios, ResetToPxe]),
            false => (PowerState::Off, vec![On, OnToBios, OnToPxe]),
        };
        Ok(Power { state, actions })
    }

    /// Boot once is a boot override first, ipmitool's way (`chassis bootdev`), then the power change, which the
    /// override has to see within a minute.
    pub async fn set_power(&self, action: PowerAction) -> Result<()> {
        use PowerAction::*;
        let (control, device) = match action {
            Off => (0, None),
            On => (1, None),
            Cycle => (2, None),
            Reset => (3, None),
            Nmi => (4, None),
            SoftOff => (5, None),
            OnToBios => (1, Some(6)),
            ResetToBios => (3, Some(6)),
            OnToPxe => (1, Some(1)),
            ResetToPxe => (3, Some(1)),
            _ => return Err(Error::Unsupported),
        };
        with(&self.target, async |session| {
            if let Some(device) = device {
                // The BIOS hasn't seen it yet; this boot only, from `device`.
                let changes = async {
                    session.request(CHASSIS, 0x08, &[0x04, 0x01, 0x01]).await?;
                    session.request(CHASSIS, 0x08, &[0x05, 0x80, device << 2, 0, 0, 0]).await.map(drop)
                };
                in_progress(|state| async move { session.request(CHASSIS, 0x08, &[0, state]).await }, changes).await?;
            }
            session.request_once(CHASSIS, 0x02, &[control]).await.map(drop)
        })
        .await
    }

    /// The FRU's information, the BMC's own (0), which is what IPMI says of the hardware.
    pub async fn fru(&self) -> Result<Fru> {
        with(&self.target, async |session| fru::read(session, 0).await).await
    }

    pub async fn sensors(&self) -> Result<Vec<Sensor>> {
        with(&self.target, async |session| sdr::sensors(session, &sdr::records(session).await?).await).await
    }

    /// Newest first, each sensor's event in its own words.
    pub async fn event_log(&self) -> Result<Vec<Event>> {
        with(&self.target, async |session| {
            // The records only name the sensors, which events go without.
            let records = sdr::records(session).await;
            let records = records.inspect_err(|error| log::warn!("No sensor records for the event log: {error}"));
            sel::events(session, &records.unwrap_or_default()).await
        })
        .await
    }

    /// The identify light; IPMI has no POST codes.
    pub async fn panel(&self) -> Result<Panel> {
        Ok(Panel { identify: with(&self.target, chassis).await?.identify, post_code: None })
    }

    pub async fn identify(&self, on: bool) -> Result<()> {
        with(&self.target, async |session| super::identify(session, on).await).await
    }

    pub async fn settings(&self) -> Result<IpmiSettings> {
        with(&self.target, async |session| {
            let channel = lan_channel(session).await;
            let server = server_settings(session).await?;
            Ok(IpmiSettings {
                network: network(session, channel).await,
                users: users(session, channel.unwrap_or(1)).await.unwrap_or_default(),
                ..server
            })
        })
        .await
    }

    /// What of `settings` is the server's, for a flavor that reads the BMC's network and users otherwise.
    pub async fn server_settings(&self) -> Result<IpmiSettings> {
        with(&self.target, server_settings).await
    }

    pub async fn change(&self, change: Change) -> Result<()> {
        with(&self.target, async |session| match &change {
            Change::Network(network) => set_network(session, network).await,
            Change::PowerRestore(restore) => set_power_restore(session, *restore).await,
            Change::SolBitRate(rate) => set_sol_bit_rate(session, *rate).await,
            Change::Fru(fru) => fru::write(session, 0, fru).await,
        })
        .await
    }

    pub async fn sol(&self) -> Result<Sol> {
        Sol::open(&self.target).await
    }

    /// Most BMCs have web pages on HTTPS too.
    pub fn web_pages(&self) -> String {
        format!("https://{}/", self.target.authority())
    }
}

fn info(device: &Device, sol: bool) -> Info {
    Info {
        flavor: Flavor::Ipmi,
        name: String::new(),
        version: device.firmware.clone(),
        build: String::new(),
        control_mode: String::new(),
        fingerprint: String::new(),
        remote_access: RemoteAccess { kvm: None, sol, ider: false, consent: Consent::None, consent_fixed: true },
    }
}

/// The server's power restore policy, serial console and FRU, `None` where the BMC doesn't say.
async fn server_settings(session: &Session) -> Result<IpmiSettings> {
    Ok(IpmiSettings {
        power_restore: chassis(session).await?.restore,
        sol: sol_settings(session).await.ok(),
        fru: fru::read(session, 0).await.ok(),
        ..Default::default()
    })
}

/// Get Device ID: the major firmware version in binary, the minor in BCD.
async fn device(session: &Session) -> Result<Device> {
    let id = session.request(APPLICATION, 0x01, &[]).await?;
    let id = id.get(..11).ok_or_else(|| Error::Protocol("a short Get Device ID".into()))?;
    Ok(Device {
        firmware: format!("{}.{:02x}", id[2] & 0x7F, id[3]),
        ipmi_version: format!("{}.{}", id[4] & 0x0F, id[4] >> 4),
        manufacturer: u32::from_le_bytes([id[6], id[7], id[8], 0]) & 0x0F_FFFF,
        product_id: u16::from_le_bytes([id[9], id[10]]),
    })
}

/// The vendors whose BMCs we'd meet, by their IANA number: old Supermicro boards say 47488 or Peppercon's 10437.
fn manufacturer(iana: u32) -> String {
    let name = match iana {
        343 => "Intel",
        674 => "Dell",
        10876 | 47488 | 10437 => "Supermicro",
        11 | 47196 => "HPE",
        2 | 20301 => "IBM",
        19046 => "Lenovo",
        20974 => "AMI",
        49622 => "ASRock Rack",
        7244 => "Quanta",
        15370 => "Gigabyte",
        49871 => "OpenBMC",
        10368 => "Fujitsu",
        5771 => "Cisco",
        2011 => "Huawei",
        37945 | 48512 => "Inspur",
        6653 => "Tyan",
        0 => "",
        _ => return format!("IANA {iana}"),
    };
    name.to_owned()
}

/// The BMC's own port on its first LAN channel, as its LAN parameters say it, blank where they don't: no DNS, which
/// IPMI has none of, nor a link speed.
async fn lan(session: &Session) -> Option<Port> {
    let channel = lan_channel(session).await?;
    let parameter = async |number: u8| lan_parameter(session, channel, number, 0).await.unwrap_or_default();
    let source = parameter(4).await.first().copied().unwrap_or(0) & 0x0F;
    let hardware = parameter(5).await;
    let hardware = hardware.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(":");
    Some(Port {
        wireless: false,
        mac: mac(&hardware),
        // We reach it, and IPMI doesn't say more.
        link_up: true,
        mbps: None,
        ipv4: Ipv4 {
            configuration: if source == 2 { Configuration::Dhcp } else { Configuration::Manual },
            address: dotted(&parameter(3).await),
            subnet_mask: dotted(&parameter(6).await),
            gateway: dotted(&parameter(12).await),
            primary_dns: String::new(),
            secondary_dns: String::new(),
        },
        ipv6: ipv6(session, channel).await,
    })
}

/// Its IPv6 addresses, static and from SLAAC or DHCPv6, those in use; `None` where IPv6 is off or the BMC has none.
async fn ipv6(session: &Session, channel: u8) -> Option<Ipv6> {
    let parameter = async |number: u8, set: u8| lan_parameter(session, channel, number, set).await;
    // Off (0), or how many static and dynamic addresses it holds.
    if parameter(51, 0).await?.first() == Some(&0) {
        return None;
    }
    let counts = parameter(55, 0).await?;
    let mut addresses = Vec::new();
    for (number, count) in [(56, counts.first()?), (59, counts.get(1)?)] {
        // A BMC holds a handful: we don't ask for 255 because it says so.
        for set in 0..(*count).min(16) {
            // Set, enabled or source, the address, its prefix length and status: 0 is in use.
            let Some(entry) = parameter(number, set).await.filter(|entry| entry.get(19) == Some(&0)) else { continue };
            let address = Ipv6Addr::from(<[u8; 16]>::try_from(&entry[2..18]).unwrap());
            if !address.is_unspecified() {
                addresses.push(address.to_string());
            }
        }
    }
    Some(Ipv6 { addresses, gateway: String::new(), primary_dns: String::new(), secondary_dns: String::new() })
}

/// The BMC's network settings on LAN `channel`, if there is one, as IPMI keeps them: no host name or DNS, which it has
/// none of.
async fn network(session: &Session, channel: Option<u8>) -> Option<Network> {
    let channel = channel?;
    let parameter = async |number: u8| lan_parameter(session, channel, number, 0).await;
    Some(Network {
        hostname: None,
        dhcp: parameter(4).await?.first()? & 0x0F == 2,
        address: dotted(&parameter(3).await?),
        subnet_mask: dotted(&parameter(6).await?),
        gateway: dotted(&parameter(12).await?),
        dns: None,
        vlan: vlan(session, channel).await,
    })
}

/// Its VLAN, `None` where it has none or doesn't say: enabled in bit 15, its ID in the 12 below.
async fn vlan(session: &Session, channel: u8) -> Option<u16> {
    let vlan = lan_parameter(session, channel, 20, 0).await?;
    let vlan = u16::from_le_bytes([*vlan.first()?, *vlan.get(1)?]);
    (vlan & 0x8000 != 0).then_some(vlan & 0x0FFF)
}

/// Sets the BMC's network as ipmitool does (`lan set`), in progress until committed, its address last: once that
/// changes, the BMC may answer us no more. Its VLAN only where it changes, as not every BMC has one.
async fn set_network(session: &Session, network: &Network) -> Result<()> {
    network.check()?;
    let channel = lan_channel(session).await.ok_or_else(|| Error::Protocol("the BMC has no LAN channel".into()))?;
    let set = async |number: u8, data: &[u8]| {
        session.request(TRANSPORT, 0x01, &[&[channel, number][..], data].concat()).await.map(drop)
    };
    let octets =
        |address: &str| address.parse::<core::net::Ipv4Addr>().map(|address| address.octets()).unwrap_or_default();
    let changes = async {
        if network.vlan != vlan(session, channel).await {
            set(20, &network.vlan.map_or(0, |vlan| vlan | 0x8000).to_le_bytes()).await?;
        }
        if network.dhcp {
            return set(4, &[2]).await;
        }
        set(4, &[1]).await?;
        set(6, &octets(&network.subnet_mask)).await?;
        set(12, &octets(&network.gateway)).await?;
        set(3, &octets(&network.address)).await
    };
    in_progress(|state| async move { set(0, &[state]).await }, changes).await
}

/// Makes `changes` with parameter 0 "set in progress", then committed and complete whatever came of them, so it doesn't
/// stay locked to us. Not every BMC has that parameter, so we ignore what it says.
async fn in_progress<T, F: Future<Output = Result<T>>>(
    state: impl Fn(u8) -> F,
    changes: impl Future<Output = Result<()>>,
) -> Result<()> {
    let _ = state(1).await;
    let changed = changes.await;
    let _ = state(2).await;
    let _ = state(0).await;
    changed
}

/// LAN configuration parameter `number`'s set `set` on `channel`, after its revision byte.
async fn lan_parameter(session: &Session, channel: u8, number: u8, set: u8) -> Option<Vec<u8>> {
    Some(session.request(TRANSPORT, 0x02, &[channel, number, set, 0]).await.ok()?.get(1..)?.to_vec())
}

fn dotted(bytes: &[u8]) -> String {
    bytes.iter().map(u8::to_string).collect::<Vec<_>>().join(".")
}

/// The first channel whose medium is a LAN (802.3, or another).
async fn lan_channel(session: &Session) -> Option<u8> {
    for channel in 1..=0x0B {
        let info = session.request(APPLICATION, 0x42, &[channel]).await;
        if info.is_ok_and(|info| matches!(info.get(1).map(|medium| medium & 0x7F), Some(0x04 | 0x06))) {
            return Some(channel);
        }
    }
    None
}

/// The user slots that have a name, on `channel`.
async fn users(session: &Session, channel: u8) -> Result<Vec<User>> {
    let access = async |id: u8| session.request(APPLICATION, 0x44, &[channel, id]).await;
    let most = access(1).await?.first().copied().unwrap_or(0) & 0x3F;
    let mut users = Vec::new();
    for id in 1..=most {
        // A slot it refuses to say is one it doesn't have.
        let (Ok(name), Ok(access)) = (session.request(APPLICATION, 0x46, &[id]).await, access(id).await) else {
            continue;
        };
        let name = String::from_utf8_lossy(&name).trim_end_matches('\0').trim().to_owned();
        if name.is_empty() {
            continue;
        }
        let privilege = match access.get(3).map(|access| access & 0x0F) {
            Some(1) => "Callback",
            Some(2) => "User",
            Some(3) => "Operator",
            Some(4) => "Administrator",
            Some(5) => "OEM",
            _ => "No access",
        };
        let enabled = match access.get(1).map(|users| users >> 6) {
            Some(1) => Some(true),
            Some(2) => Some(false),
            _ => None,
        };
        users.push(User { id, name, privilege: privilege.to_owned(), enabled });
    }
    Ok(users)
}
