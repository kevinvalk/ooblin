use core::net::Ipv6Addr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    APPLICATION, CHASSIS, Fru, Network, PowerRestore, Session, Sol, SolSettings, TRANSPORT, chassis, fru, sdr, sel,
    set_power_restore, set_sol_bit_rate, sol_settings, with_session,
};
use crate::{
    Configuration, Consent, Error, Event, Flavor, Info, Ipv4, Ipv6, Panel, Port, Power, PowerAction, PowerState,
    RemoteAccess, Result, Sensor, Target, mac,
};

pub struct Client {
    target: Target,
}

/// What the BMC says it is, and its own network port.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct IpmiStatus {
    pub info: Info,
    pub power: PowerState,
    /// The vendor's name where we know it, else its IANA number, and blank where it is unset.
    pub manufacturer: String,
    pub product_id: u16,
    pub ipmi_version: String,
    /// The system GUID as SMBIOS and the OS write it.
    pub system_id: String,
    pub ports: Vec<Port>,
}

/// What can be set over IPMI, and the BMC's users to read. A setting is `None` where the BMC does not say.
#[derive(Debug, Default, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct IpmiSettings {
    pub network: Option<Network>,
    pub power_restore: Option<PowerRestore>,
    pub sol: Option<SolSettings>,
    pub fru: Option<Fru>,
    pub users: Vec<User>,
}

/// A user slot that has a name.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct User {
    pub id: u8,
    pub name: String,
    /// "Administrator", "Operator", "User", "Callback", "OEM" or "No access".
    pub privilege: String,
    /// `None` where the BMC does not say.
    pub is_enabled: Option<bool>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "IpmiChange")]
pub enum Change {
    Network(Network),
    PowerRestore(PowerRestore),
    SolBitRate(u32),
    Fru(Fru),
}

struct Device {
    firmware: String, // As `4.07`
    ipmi_version: String,
    manufacturer_iana: u32,
    product_id: u16,
}

impl Client {
    pub fn new(target: &Target) -> Self {
        Self { target: target.clone() }
    }

    /// This is the login too, as the BMC says who it is.
    pub async fn info(&self) -> Result<Info> {
        let (device, sol) = with_session(&self.target, async |session| {
            let device = device(session).await?;
            let sol = sol_settings(session).await.ok();
            Ok((device, sol))
        })
        .await?;

        Ok(info(&device, sol.is_some_and(|sol| sol.is_enabled)))
    }

    pub async fn firmware(&self) -> Result<String> {
        Ok(with_session(&self.target, device).await?.firmware)
    }

    pub async fn status(&self) -> Result<IpmiStatus> {
        with_session(&self.target, async |session| {
            let device = device(session).await?;
            let power = if chassis(session).await?.is_on { PowerState::On } else { PowerState::Off };
            let guid = session.request(APPLICATION, 0x37, &[]).await.unwrap_or_default();
            let sol = sol_settings(session).await.ok();
            let port = lan(session).await;

            Ok(IpmiStatus {
                info: info(&device, sol.is_some_and(|sol| sol.is_enabled)),
                power,
                manufacturer: manufacturer(device.manufacturer_iana),
                product_id: device.product_id,
                ipmi_version: device.ipmi_version,
                system_id: guid.try_into().map(crate::uuid).unwrap_or_default(),
                ports: port.into_iter().collect(),
            })
        })
        .await
    }

    pub async fn power(&self) -> Result<Power> {
        use PowerAction::*;

        // NOTE: IPMI asks the OS to shut down through ACPI but has no way to ask it to restart, so we offer no such
        // action.
        let (state, actions) = match with_session(&self.target, chassis).await?.is_on {
            true => (PowerState::On, vec![SoftOff, Off, Reset, Cycle, Nmi, ResetToBios, ResetToPxe]),
            false => (PowerState::Off, vec![On, OnToBios, OnToPxe]),
        };
        Ok(Power { state, actions })
    }

    pub async fn set_power(&self, action: PowerAction) -> Result<()> {
        use PowerAction::*;

        let (chassis_control, boot_device) = match action {
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

        with_session(&self.target, async |session| {
            // We boot once by setting a boot override first, ipmitool's way (`chassis bootdev`), and changing the power
            // after it. The override has to see that power change within a minute!
            if let Some(boot_device) = boot_device {
                // First we say the BIOS has not seen the override yet, then that it holds for this boot only, from
                // `boot_device`.
                let changes = async {
                    session.request(CHASSIS, 0x08, &[0x04, 0x01, 0x01]).await?;
                    session.request(CHASSIS, 0x08, &[0x05, 0x80, boot_device << 2, 0, 0, 0]).await.map(drop)
                };
                in_progress(|state| async move { session.request(CHASSIS, 0x08, &[0, state]).await }, changes).await?;
            }

            session.request_once(CHASSIS, 0x02, &[chassis_control]).await.map(drop)
        })
        .await
    }

    /// We read the BMC's own FRU (0), as that is what IPMI says of the hardware.
    pub async fn fru(&self) -> Result<Fru> {
        with_session(&self.target, async |session| fru::read(session, 0).await).await
    }

    pub async fn sensors(&self) -> Result<Vec<Sensor>> {
        with_session(&self.target, async |session| sdr::sensors(session, &sdr::records(session).await?).await).await
    }

    /// The events come newest first.
    pub async fn event_log(&self) -> Result<Vec<Event>> {
        with_session(&self.target, async |session| {
            // The sensor records only name the sensors, so we carry on without them where the BMC does not give them.
            let records = sdr::records(session).await;
            let records = records.inspect_err(|error| log::warn!("No sensor records for the event log: {error}"));

            sel::events(session, &records.unwrap_or_default()).await
        })
        .await
    }

    pub async fn panel(&self) -> Result<Panel> {
        let chassis = with_session(&self.target, chassis).await?;

        Ok(Panel { is_identifying: chassis.is_identifying, post_code: None }) // IPMI has no POST codes
    }

    pub async fn identify(&self, is_on: bool) -> Result<()> {
        with_session(&self.target, async |session| super::identify(session, is_on).await).await
    }

    pub async fn settings(&self) -> Result<IpmiSettings> {
        with_session(&self.target, async |session| {
            let channel = lan_channel(session).await;
            let settings = server_settings(session).await?;
            let network = network(session, channel).await;
            let users = users(session, channel.unwrap_or(1)).await.unwrap_or_default();

            Ok(IpmiSettings { network, users, ..settings })
        })
        .await
    }

    /// Only what of `settings` is the server's, for a flavor that reads the BMC's network and users some other way.
    pub async fn server_settings(&self) -> Result<IpmiSettings> {
        with_session(&self.target, server_settings).await
    }

    pub async fn change(&self, change: Change) -> Result<()> {
        with_session(&self.target, async |session| match &change {
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

    /// NOTE: We assume the BMC has web pages on HTTPS too, as most have.
    pub fn web_pages(&self) -> String {
        format!("https://{}/", self.target.authority())
    }
}

fn info(device: &Device, is_sol_enabled: bool) -> Info {
    // NOTE: Over IPMI a BMC has no name of its own, nor a certificate, so those stay blank.
    Info {
        flavor: Flavor::Ipmi,
        name: String::new(),
        version: device.firmware.clone(),
        build: String::new(),
        control_mode: String::new(),
        fingerprint: String::new(),
        remote_access: RemoteAccess {
            is_kvm: None,
            is_sol: is_sol_enabled,
            is_ider: false,
            consent: Consent::None,
            is_consent_fixed: true,
        },
    }
}

async fn server_settings(session: &Session) -> Result<IpmiSettings> {
    Ok(IpmiSettings {
        power_restore: chassis(session).await?.restore,
        sol: sol_settings(session).await.ok(),
        fru: fru::read(session, 0).await.ok(),
        ..Default::default()
    })
}

async fn device(session: &Session) -> Result<Device> {
    let id = session.request(APPLICATION, 0x01, &[]).await?;
    let id = id.get(..11).ok_or_else(|| Error::Protocol("a short Get Device ID".into()))?;

    Ok(Device {
        firmware: format!("{}.{:02x}", id[2] & 0x7F, id[3]), // The major version is binary, the minor BCD
        ipmi_version: format!("{}.{}", id[4] & 0x0F, id[4] >> 4),
        manufacturer_iana: u32::from_le_bytes([id[6], id[7], id[8], 0]) & 0x0F_FFFF,
        product_id: u16::from_le_bytes([id[9], id[10]]),
    })
}

fn manufacturer(iana: u32) -> String {
    // The vendors whose BMCs we would meet, by their IANA number.
    let name = match iana {
        343 => "Intel",
        674 => "Dell",
        10876 | 47488 | 10437 => "Supermicro", // Old boards say 47488 or Peppercon's 10437
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

/// The BMC's own port on its first LAN channel as its LAN parameters say it, blank where they do not.
async fn lan(session: &Session) -> Option<Port> {
    let channel = lan_channel(session).await?;
    let parameter = async |number: u8| lan_parameter(session, channel, number, 0).await.unwrap_or_default();

    let address_source = parameter(4).await.first().copied().unwrap_or(0) & 0x0F;
    let mac_address = parameter(5).await;
    let mac_address = mac_address.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(":");
    let address = dotted(&parameter(3).await);
    let subnet_mask = dotted(&parameter(6).await);
    let gateway = dotted(&parameter(12).await);
    let ipv6 = ipv6(session, channel).await;

    Some(Port {
        is_wireless: false,
        mac: mac(&mac_address),
        is_link_up: true, // We reach it, and IPMI does not say more
        mbps: None,
        ipv4: Ipv4 {
            configuration: if address_source == 2 { Configuration::Dhcp } else { Configuration::Manual },
            address,
            subnet_mask,
            gateway,
            // IPMI has no DNS servers.
            primary_dns: String::new(),
            secondary_dns: String::new(),
        },
        ipv6,
    })
}

/// The static and the dynamic (SLAAC or DHCPv6) addresses that are in use, `None` where IPv6 is off or the BMC has none
/// of it.
async fn ipv6(session: &Session, channel: u8) -> Option<Ipv6> {
    let parameter = async |number: u8, set: u8| lan_parameter(session, channel, number, set).await;

    // Parameter 51 is 0 where IPv6 is off, and 55 says how many static and dynamic addresses the BMC holds.
    if parameter(51, 0).await?.first() == Some(&0) {
        return None;
    }
    let counts = parameter(55, 0).await?;

    let mut addresses = Vec::new();
    for (number, count) in [(56, counts.first()?), (59, counts.get(1)?)] {
        // A BMC holds a handful of addresses, so we do not ask for 255 of them just because it says so.
        for set in 0..(*count).min(16) {
            // An entry is its set, enabled or source, the address, its prefix length and its status, where 0 is in use.
            let Some(entry) = parameter(number, set).await.filter(|entry| entry.get(19) == Some(&0)) else { continue };
            let address = Ipv6Addr::from(<[u8; 16]>::try_from(&entry[2..18]).unwrap());
            if !address.is_unspecified() {
                addresses.push(address.to_string());
            }
        }
    }

    Some(Ipv6 { addresses, gateway: String::new(), primary_dns: String::new(), secondary_dns: String::new() })
}

async fn network(session: &Session, channel: Option<u8>) -> Option<Network> {
    let channel = channel?;
    let parameter = async |number: u8| lan_parameter(session, channel, number, 0).await;

    let is_dhcp = parameter(4).await?.first()? & 0x0F == 2;
    let address = dotted(&parameter(3).await?);
    let subnet_mask = dotted(&parameter(6).await?);
    let gateway = dotted(&parameter(12).await?);
    let vlan = vlan(session, channel).await;

    // IPMI keeps no host name or DNS server, so those stay `None`.
    Some(Network { hostname: None, is_dhcp, address, subnet_mask, gateway, dns: None, vlan })
}

/// `None` where the BMC has no VLAN or does not say.
async fn vlan(session: &Session, channel: u8) -> Option<u16> {
    let vlan = lan_parameter(session, channel, 20, 0).await?;
    let vlan = u16::from_le_bytes([*vlan.first()?, *vlan.get(1)?]);

    (vlan & 0x8000 != 0).then_some(vlan & 0x0FFF) // Bit 15 says whether it is enabled, the 12 bits below are its ID
}

/// We set the BMC's network as ipmitool does (`lan set`): in progress until committed, and its address last, because
/// once that changes the BMC may answer us no more. The VLAN is only set where it changes, as not every BMC has one.
async fn set_network(session: &Session, network: &Network) -> Result<()> {
    network.reject_if_invalid()?;

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
        if network.is_dhcp {
            return set(4, &[2]).await;
        }

        set(4, &[1]).await?;
        set(6, &octets(&network.subnet_mask)).await?;
        set(12, &octets(&network.gateway)).await?;
        set(3, &octets(&network.address)).await
    };

    in_progress(|state| async move { set(0, &[state]).await }, changes).await
}

/// Makes `changes` with parameter 0 at "set in progress", then at committed and at complete whatever came of them, so
/// the parameters do not stay locked to us. Not every BMC has that parameter, so we ignore what it answers.
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

async fn lan_parameter(session: &Session, channel: u8, number: u8, set: u8) -> Option<Vec<u8>> {
    let answer = session.request(TRANSPORT, 0x02, &[channel, number, set, 0]).await.ok()?;

    Some(answer.get(1..)?.to_vec()) // The parameter comes after its revision byte
}

fn dotted(bytes: &[u8]) -> String {
    bytes.iter().map(u8::to_string).collect::<Vec<_>>().join(".")
}

async fn lan_channel(session: &Session) -> Option<u8> {
    for channel in 1..=0x0B {
        let info = session.request(APPLICATION, 0x42, &[channel]).await;

        // The first channel whose medium is a LAN is ours: 802.3 (4), or another kind of LAN (6).
        if info.is_ok_and(|info| matches!(info.get(1).map(|medium| medium & 0x7F), Some(0x04 | 0x06))) {
            return Some(channel);
        }
    }

    None
}

async fn users(session: &Session, channel: u8) -> Result<Vec<User>> {
    let user_access = async |id: u8| session.request(APPLICATION, 0x44, &[channel, id]).await;
    let slot_count = user_access(1).await?.first().copied().unwrap_or(0) & 0x3F;

    let mut users = Vec::new();
    for id in 1..=slot_count {
        // A slot the BMC refuses to tell us about is one it does not have.
        let (Ok(name), Ok(access)) = (session.request(APPLICATION, 0x46, &[id]).await, user_access(id).await) else {
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
        let is_enabled = match access.get(1).map(|status| status >> 6) {
            Some(1) => Some(true),
            Some(2) => Some(false),
            _ => None,
        };

        users.push(User { id, name, privilege: privilege.to_owned(), is_enabled });
    }

    Ok(users)
}
