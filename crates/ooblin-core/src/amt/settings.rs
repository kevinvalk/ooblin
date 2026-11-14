// Addresses only: `core` has no sockets, see tests/net_boundary.rs.
use core::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use instant_xml::ToXml;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::wsman::{Instance, Key, Reference, method};
use super::{Alarm, Certificate, Client, Recovery, Tls};
use crate::{Configuration, Error, Ipv4, RemoteAccess, Result};

/// How the device is set up.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Settings {
    pub remote_access: RemoteAccess,
    pub general: General,
    /// How far the device's clock is ahead of ours, in seconds.
    pub clock_offset: Option<i64>,
    pub power_policies: Vec<PowerPolicy>,
    pub alarms: Vec<Alarm>,
    pub network: Network,
    pub recovery: Recovery,
    /// For the network, then for software on the device itself (through LMS).
    pub tls: Vec<Tls>,
    pub certificates: Vec<Certificate>,
}

/// The wired port's: its IPv4 addresses, and when AMT is on the network at all.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Network {
    pub ipv4: Ipv4,
    pub ipv6: Ipv6Settings,
    pub available: Availability,
}

/// The addresses to use besides the automatic ones (`Manual`), empty for none.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Ipv6Settings {
    pub configuration: Ipv6Configuration,
    pub address: String,
    pub gateway: String,
    pub primary_dns: String,
    pub secondary_dns: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Ipv6Configuration {
    Off,
    Automatic,
    Manual,
}

/// When AMT is on the network: with the device on or asleep (or off), on mains power or a battery.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Availability {
    pub on: bool,
    pub asleep: bool,
    pub on_battery: bool,
    pub asleep_on_battery: bool,
}

impl Availability {
    /// `LinkPolicy`'s values, in the order of our fields.
    const POLICIES: [u8; 4] = [1, 14, 16, 224];
}

/// When AMT itself is powered, the device's maker offering one or more.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct PowerPolicy {
    pub id: String,
    /// As AMT has it but for its kind ("Mobile: …", "Desktop: …"), which the device's maker set.
    pub description: String,
    pub active: bool,
}

/// AMT's name on the network, and whether it answers pings.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct General {
    /// The host name, with its domain if it has one.
    pub name: String,
    /// Whether the OS's name is AMT's too.
    pub shared_with_os: bool,
    pub respond_to_ping: bool,
    /// How long AMT stays powered after the device sleeps or it woke for us, as far as the power policy has it sleep.
    pub awake_minutes: u16,
    pub dns_update: DnsUpdate,
}

/// Who registers AMT's name in DNS: nobody, the DHCP server when AMT asks it, or AMT itself (dynamic DNS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum DnsUpdate {
    Off,
    Dhcp,
    Amt,
}

/// A change to the settings, see `Client::change`.
#[derive(Debug, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Change {
    RemoteAccess(RemoteAccess),
    General(General),
    PowerPolicy(String),
    Ipv4(Ipv4),
    Ipv6(Ipv6Settings),
    Availability(Availability),
    Recovery(Recovery),
    Clock,
    AddAlarm(Alarm),
    DeleteAlarm(String),
    /// The certificate in the file at `path`, see `Client::add_certificate`.
    AddCertificate {
        path: PathBuf,
        trusted_root: bool,
    },
    DeleteCertificate(String),
}

impl Client {
    pub async fn settings(&mut self) -> Result<Settings> {
        let general = self.get("AMT_GeneralSettings").await?;
        let schemes = self.enumerate("AMT_SystemPowerScheme").await?;
        let element_settings = self.enumerate("CIM_ElementSettingData").await?;

        // The active power scheme is the current setting that points at one, its GUID being the reference's last line.
        let active = element_settings.iter().find_map(|setting| {
            let reference = setting.get("SettingData").filter(|reference| reference.contains("AMT_SystemPowerScheme"));
            reference.filter(|_| setting.get("IsCurrent") == Some("1")).and_then(|reference| reference.lines().last())
        });
        let clock = self.query(&GetLowAccuracyTimeSynch).await?.parse::<i64>("Ta0");
        let port = self.wired_port().await?;
        let policies: Vec<u8> = port.all("LinkPolicy").filter_map(|policy| policy.parse().ok()).collect();
        let [on, asleep, on_battery, asleep_on_battery] = Availability::POLICIES.map(|it| policies.contains(&it));
        Ok(Settings {
            remote_access: self.remote_access().await?,
            general: General {
                name: name(&general),
                shared_with_os: general.parse("SharedFQDN").unwrap_or_default(),
                respond_to_ping: general.parse("PingResponseEnabled").unwrap_or_default(),
                awake_minutes: general.parse("IdleWakeTimeout").unwrap_or_default(),
                // Like MeshCommander, AMT's own wins.
                dns_update: match (general.get("DDNSUpdateEnabled"), general.get("DDNSUpdateByDHCPServerEnabled")) {
                    (Some("true"), _) => DnsUpdate::Amt,
                    (_, Some("true")) => DnsUpdate::Dhcp,
                    _ => DnsUpdate::Off,
                },
            },
            clock_offset: clock.map(|clock| clock - now()),
            power_policies: schemes
                .iter()
                .map(|scheme| {
                    let description = scheme.text("Description");
                    PowerPolicy {
                        id: scheme.text("InstanceID"),
                        description: description.split_once(": ").map_or(&*description, |(_, it)| it).to_owned(),
                        active: scheme.get("SchemeGUID") == active,
                    }
                })
                .collect(),
            alarms: self.alarms().await?,
            network: Network {
                ipv4: Ipv4::read(&port),
                ipv6: self.ipv6(&element_settings).await?,
                available: Availability { on, asleep, on_battery, asleep_on_battery },
            },
            recovery: self.recovery().await?,
            tls: self.tls().await?,
            certificates: self.certificates().await?,
        })
    }

    pub async fn change(&mut self, change: Change) -> Result<()> {
        match change {
            Change::RemoteAccess(access) => self.set_remote_access(&access).await,
            Change::General(general) => self.set_general(&general).await,
            Change::PowerPolicy(id) => self.set_power_policy(&id).await,
            Change::Ipv4(ipv4) => self.set_ipv4(&ipv4).await,
            Change::Ipv6(ipv6) => self.set_ipv6(&ipv6).await,
            Change::Availability(available) => self.set_availability(&available).await,
            Change::Recovery(recovery) => self.set_recovery(&recovery).await,
            Change::Clock => self.set_clock().await,
            Change::AddAlarm(alarm) => self.add_alarm(&alarm).await,
            Change::DeleteAlarm(name) => self.delete_alarm(&name).await,
            Change::AddCertificate { path, trusted_root } => {
                // Not a bare `?`: a file that won't read isn't the device being unreachable.
                let file = std::fs::read(path)
                    .map_err(|error| Error::Invalid(format!("That certificate file won't read: {error}.")))?;
                self.add_certificate(&file, trusted_root).await
            }
            Change::DeleteCertificate(id) => self.delete_certificate(&id).await,
        }
    }

    /// The wired port's settings, AMT's first.
    async fn wired_port(&mut self) -> Result<Instance> {
        let ports = self.enumerate("AMT_EthernetPortSettings").await?;
        ports.into_iter().next().ok_or_else(|| Error::Protocol("the device has no network port".into()))
    }

    /// Only addresses: AMT takes whatever it's given, and one it can't use takes it off the network.
    async fn set_ipv4(&mut self, wanted: &Ipv4) -> Result<()> {
        let addresses =
            [&wanted.address, &wanted.subnet_mask, &wanted.gateway, &wanted.primary_dns, &wanted.secondary_dns];
        addresses.iter().try_for_each(|address| valid::<Ipv4Addr>(address, "IPv4"))?;
        if wanted.configuration == Configuration::Manual && (wanted.address.is_empty() || wanted.subnet_mask.is_empty())
        {
            return Err(Error::Invalid("A manual configuration needs an address and a subnet mask.".into()));
        }
        let mut port = self.wired_port().await?;
        let configuration = wanted.configuration;
        port.set("DHCPEnabled", configuration == Configuration::Dhcp);
        if configuration != Configuration::Dhcp {
            // AMT only shares a static address it takes from the OS.
            port.set("IpSyncEnabled", configuration == Configuration::Os);
            port.set("SharedStaticIp", configuration == Configuration::Os);
        }
        for (name, value) in ADDRESSES.into_iter().zip(addresses) {
            port.set_all(name, (!value.is_empty()).then_some(value));
        }
        self.put_port(port).await
    }

    /// The wired port's, on when its link to the port is current (1, else 2), with any address that isn't `::` set by
    /// hand.
    async fn ipv6(&mut self, element_settings: &[Instance]) -> Result<Ipv6Settings> {
        let settings = self.enumerate("IPS_IPv6PortSettings").await?;
        let on = ipv6_link(element_settings).is_some_and(|link| link.get("IsCurrent") == Some("1"));
        let address = |name| settings.first().map(|settings| settings.text(name)).filter(|it| it != "::");
        let addresses = IPV6_ADDRESSES.map(|name| address(name).unwrap_or_default());
        let configuration = match (on, addresses.iter().any(|address| !address.is_empty())) {
            (false, _) => Ipv6Configuration::Off,
            (true, false) => Ipv6Configuration::Automatic,
            (true, true) => Ipv6Configuration::Manual,
        };
        let [address, gateway, primary_dns, secondary_dns] = addresses;
        Ok(Ipv6Settings { configuration, address, gateway, primary_dns, secondary_dns })
    }

    /// Like MeshCommander: the addresses on the port's IPv6 settings, `::` for none, and IPv6 on or off on the link
    /// between them. Only addresses, see `set_ipv4`.
    async fn set_ipv6(&mut self, wanted: &Ipv6Settings) -> Result<()> {
        let configuration = wanted.configuration;
        let addresses = [&wanted.address, &wanted.gateway, &wanted.primary_dns, &wanted.secondary_dns];
        addresses.iter().try_for_each(|address| valid::<Ipv6Addr>(address, "IPv6"))?;
        if configuration != Ipv6Configuration::Off {
            let mut settings = self.enumerate("IPS_IPv6PortSettings").await?.into_iter().next();
            let settings = settings.as_mut().ok_or_else(|| Error::Protocol("the device has no IPv6".into()))?;
            for (name, address) in IPV6_ADDRESSES.into_iter().zip(addresses) {
                let manual = configuration == Ipv6Configuration::Manual && !address.is_empty();
                settings.set(name, if manual { address.to_lowercase() } else { "::".into() });
            }
            let id = settings.text("InstanceID");
            self.put_on("IPS_IPv6PortSettings", &[("InstanceID", Key::Value(&id))], &*settings).await?;
        }

        let element_settings = self.enumerate("CIM_ElementSettingData").await?;
        let link = ipv6_link(&element_settings).ok_or_else(|| Error::Protocol("the device has no IPv6".into()))?;
        let current = if configuration == Ipv6Configuration::Off { 2 } else { 1 };
        if link.parse("IsCurrent") == Some(current) {
            return Ok(());
        }
        let ports = self.enumerate("CIM_EthernetPort").await?;
        let port = ports.first().ok_or_else(|| Error::Protocol("the device has no network port".into()))?;
        let keys = ["CreationClassName", "DeviceID", "SystemCreationClassName", "SystemName"]
            .map(|name| (name, port.get(name).unwrap_or_default()));
        let port = Reference { class: "CIM_EthernetPort", selectors: &keys };
        let id = link.text("SettingData").lines().last().unwrap_or_default().to_owned();
        let settings = Reference { class: "IPS_IPv6PortSettings", selectors: &[("InstanceID", &id)] };
        let changed = ElementSettingData {
            IsCurrent: current,
            IsDefault: &link.text("IsDefault"),
            IsNext: &link.text("IsNext"),
            ManagedElement: port,
            SettingData: settings,
        };
        let keys = [("ManagedElement", Key::Reference(port)), ("SettingData", Key::Reference(settings))];
        self.put_on("CIM_ElementSettingData", &keys, &changed).await
    }

    async fn set_availability(&mut self, wanted: &Availability) -> Result<()> {
        let mut port = self.wired_port().await?;
        let states = [wanted.on, wanted.asleep, wanted.on_battery, wanted.asleep_on_battery];
        let policies = Availability::POLICIES.into_iter().zip(states).filter_map(|(it, on)| on.then_some(it));
        port.set_all("LinkPolicy", policies);
        self.put_port(port).await
    }

    /// Like Intel's console: with DHCP or the OS's address AMT wants no addresses at all.
    async fn put_port(&mut self, mut port: Instance) -> Result<()> {
        if port.get("DHCPEnabled") == Some("true") || port.get("IpSyncEnabled") == Some("true") {
            for name in ADDRESSES {
                port.set_all(name, None::<&str>);
            }
        }
        let id = port.text("InstanceID");
        self.put_on("AMT_EthernetPortSettings", &[("InstanceID", Key::Value(&id))], &port).await
    }

    async fn set_general(&mut self, wanted: &General) -> Result<()> {
        let mut general = self.get("AMT_GeneralSettings").await?;
        // AMT keeps them apart, like MeshCommander we split at the first dot.
        let (host, domain) = wanted.name.split_once('.').unwrap_or((&wanted.name, ""));
        general.set("HostName", host);
        general.set("DomainName", domain);
        general.set("SharedFQDN", wanted.shared_with_os);
        general.set("PingResponseEnabled", wanted.respond_to_ping);
        general.set("IdleWakeTimeout", wanted.awake_minutes);
        general.set("DDNSUpdateEnabled", wanted.dns_update == DnsUpdate::Amt);
        general.set("DDNSUpdateByDHCPServerEnabled", wanted.dns_update == DnsUpdate::Dhcp);
        self.put("AMT_GeneralSettings", &general).await
    }

    /// Makes the power policy with this `id` the active one.
    async fn set_power_policy(&mut self, id: &str) -> Result<()> {
        self.invoke_on(id, &SetPowerScheme).await?;
        Ok(())
    }

    /// Sets the device's clock to ours, like MeshCommander: AMT wants the time it said last and when we heard it.
    async fn set_clock(&mut self) -> Result<()> {
        let clock = self.query(&GetLowAccuracyTimeSynch).await?.parse("Ta0");
        let clock = clock.ok_or_else(|| Error::Protocol("the device didn't say its time".into()))?;
        self.invoke(&SetHighAccuracyTimeSynch { Ta0: clock, Tm1: now(), Tm2: now() }).await?;
        Ok(())
    }
}

/// Whether `address` is empty or an address `kind` names.
fn valid<T: FromStr>(address: &str, kind: &str) -> Result<()> {
    match address.is_empty() || address.parse::<T>().is_ok() {
        true => Ok(()),
        false => Err(Error::Invalid(format!("{address} isn't an {kind} address."))),
    }
}

/// The wired port's link to its IPv6 settings.
fn ipv6_link(element_settings: &[Instance]) -> Option<&Instance> {
    element_settings.iter().find(|setting| setting.get("SettingData").is_some_and(|it| it.ends_with("IPv6 Settings 0")))
}

/// What `IPS_IPv6PortSettings` has to set by hand, in the order of `Ipv6Settings`.
const IPV6_ADDRESSES: [&str; 4] = ["IPv6Address", "DefaultRouter", "PrimaryDNS", "SecondaryDNS"];

/// A port's IPv4 addresses in `AMT_EthernetPortSettings`.
const ADDRESSES: [&str; 5] = ["IPAddress", "SubnetMask", "DefaultGateway", "PrimaryDNS", "SecondaryDNS"];

/// The host name in `AMT_GeneralSettings`, with its domain if it has one.
pub(crate) fn name(general: &Instance) -> String {
    let (host, domain) = (general.text("HostName"), general.text("DomainName"));
    if domain.is_empty() { host } else { format!("{host}.{domain}") }
}

/// Unix seconds.
fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

const ELEMENT_SETTING: &str = "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ElementSettingData";

/// A link between an element and its settings, as MeshCommander writes it back.
#[derive(ToXml)]
#[xml(rename = "CIM_ElementSettingData", ns(ELEMENT_SETTING))]
struct ElementSettingData<'a> {
    IsCurrent: u8,
    IsDefault: &'a str,
    IsNext: &'a str,
    ManagedElement: Reference<'a>,
    SettingData: Reference<'a>,
}

const POWER_SCHEME: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_SystemPowerScheme";

/// A method of the scheme to activate, as MeshCommander calls it.
#[derive(ToXml)]
#[xml(rename = "SetPowerScheme_INPUT", ns(POWER_SCHEME))]
pub(crate) struct SetPowerScheme;

method!(SetPowerScheme, POWER_SCHEME);

const TIME_SYNCHRONIZATION: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_TimeSynchronizationService";

#[derive(ToXml)]
#[xml(rename = "GetLowAccuracyTimeSynch_INPUT", ns(TIME_SYNCHRONIZATION))]
struct GetLowAccuracyTimeSynch;

method!(GetLowAccuracyTimeSynch, TIME_SYNCHRONIZATION);

#[derive(ToXml)]
#[xml(rename = "SetHighAccuracyTimeSynch_INPUT", ns(TIME_SYNCHRONIZATION))]
struct SetHighAccuracyTimeSynch {
    Ta0: i64,
    Tm1: i64,
    Tm2: i64,
}

method!(SetHighAccuracyTimeSynch, TIME_SYNCHRONIZATION);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amt::wsman::{self, body_meaning, meaning};

    #[test]
    fn sets_the_clock_like_go_wsman_messages() {
        // From go-wsman-messages' `timesynchronization/service_test.go`.
        let expected = r#"<h:SetHighAccuracyTimeSynch_INPUT xmlns:h="http://intel.com/wbem/wscim/1/amt-schema/1/AMT_TimeSynchronizationService"><h:Ta0>1644240911</h:Ta0><h:Tm1>1644240943</h:Tm1><h:Tm2>1644240943</h:Tm2></h:SetHighAccuracyTimeSynch_INPUT>"#;
        let input = SetHighAccuracyTimeSynch { Ta0: 1644240911, Tm1: 1644240943, Tm2: 1644240943 };
        assert_eq!(body_meaning(&wsman::invoke(&input, None, 0)), meaning(expected));
    }
}
