// NOTE: We only take addresses from `core`, which has no sockets, see tests/net_boundary.rs.
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

/// The network settings of the wired port: its addresses, and when AMT is on the network at all.
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
    pub is_on: bool,
    pub is_asleep: bool,
    pub is_on_battery: bool,
    pub is_asleep_on_battery: bool,
}

impl Availability {
    // The values of `LinkPolicy`, in the order of our fields.
    const POLICIES: [u8; 4] = [1, 14, 16, 224];
}

/// When AMT itself is powered. The device's maker offers one or more of these.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct PowerPolicy {
    pub id: String,
    /// As AMT has it but for its kind ("Mobile: …", "Desktop: …"), which the device's maker set.
    pub description: String,
    pub is_active: bool,
}

/// AMT's name on the network, and whether it answers pings.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct General {
    /// The host name, with its domain if it has one.
    pub name: String,
    /// Is the OS's name AMT's too?
    pub is_shared_with_os: bool,
    pub is_respond_to_ping: bool,
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
        is_trusted_root: bool,
    },
    DeleteCertificate(String),
}

impl Client {
    pub async fn settings(&mut self) -> Result<Settings> {
        let general = self.get("AMT_GeneralSettings").await?;
        let schemes = self.enumerate("AMT_SystemPowerScheme").await?;
        let element_settings = self.enumerate("CIM_ElementSettingData").await?;

        // The active power scheme is the current setting that points at one, and its GUID is the last line of the
        // reference.
        let active_scheme_guid = element_settings.iter().find_map(|element_setting| {
            let reference =
                element_setting.get("SettingData").filter(|reference| reference.contains("AMT_SystemPowerScheme"));
            let is_current = element_setting.get("IsCurrent") == Some("1");
            reference.filter(|_| is_current).and_then(|reference| reference.lines().last())
        });

        let time = self.query(&GetLowAccuracyTimeSynch).await?;
        let clock = time.parse::<i64>("Ta0");

        let port = self.wired_port().await?;
        let policies: Vec<u8> = port.all("LinkPolicy").filter_map(|policy| policy.parse().ok()).collect();
        let [is_on, is_asleep, is_on_battery, is_asleep_on_battery] =
            Availability::POLICIES.map(|policy| policies.contains(&policy));

        Ok(Settings {
            remote_access: self.remote_access().await?,
            general: General {
                name: name(&general),
                is_shared_with_os: general.parse("SharedFQDN").unwrap_or_default(),
                is_respond_to_ping: general.parse("PingResponseEnabled").unwrap_or_default(),
                awake_minutes: general.parse("IdleWakeTimeout").unwrap_or_default(),
                // Like MeshCommander, we let AMT's own update win when both are on.
                dns_update: match (general.get("DDNSUpdateEnabled"), general.get("DDNSUpdateByDHCPServerEnabled")) {
                    (Some("true"), _) => DnsUpdate::Amt,
                    (_, Some("true")) => DnsUpdate::Dhcp,
                    _ => DnsUpdate::Off,
                },
            },
            clock_offset: clock.map(|clock| clock.saturating_sub(now())),
            power_policies: schemes
                .iter()
                .map(|scheme| {
                    let description = scheme.text("Description");
                    PowerPolicy {
                        id: scheme.text("InstanceID"),
                        description: description.split_once(": ").map_or(&*description, |(_, rest)| rest).to_owned(),
                        is_active: scheme.get("SchemeGUID") == active_scheme_guid,
                    }
                })
                .collect(),
            alarms: self.alarms().await?,
            network: Network {
                ipv4: Ipv4::read(&port),
                ipv6: self.ipv6(&element_settings).await?,
                available: Availability { is_on, is_asleep, is_on_battery, is_asleep_on_battery },
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
            Change::AddCertificate { path, is_trusted_root } => {
                // We do not use a bare `?` here, because a file that will not read is not the device being unreachable.
                let file = std::fs::read(path)
                    .map_err(|error| Error::Invalid(format!("That certificate file won't read: {error}.")))?;
                self.add_certificate(&file, is_trusted_root).await
            }
            Change::DeleteCertificate(id) => self.delete_certificate(&id).await,
        }
    }

    /// The settings of the wired port, which is the first port AMT lists.
    async fn wired_port(&mut self) -> Result<Instance> {
        let ports = self.enumerate("AMT_EthernetPortSettings").await?;
        ports.into_iter().next().ok_or_else(|| Error::Protocol("the device has no network port".into()))
    }

    async fn set_ipv4(&mut self, wanted: &Ipv4) -> Result<()> {
        let addresses =
            [&wanted.address, &wanted.subnet_mask, &wanted.gateway, &wanted.primary_dns, &wanted.secondary_dns];

        // IMPORTANT: We only let addresses through, because AMT takes whatever it is given and an address it cannot
        // use takes it off the network!
        addresses.iter().try_for_each(|address| reject_if_not_address::<Ipv4Addr>(address, "IPv4"))?;
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
        for (name, address) in ADDRESSES.into_iter().zip(addresses) {
            port.set_all(name, (!address.is_empty()).then_some(address));
        }

        self.put_port(port).await
    }

    /// The IPv6 settings of the wired port. IPv6 is on when the link between the two is current (1, else 2), and any
    /// address that is not `::` was set by hand.
    async fn ipv6(&mut self, element_settings: &[Instance]) -> Result<Ipv6Settings> {
        let settings = self.enumerate("IPS_IPv6PortSettings").await?;
        let is_on = ipv6_link(element_settings).is_some_and(|link| link.get("IsCurrent") == Some("1"));

        let address = |name| settings.first().map(|settings| settings.text(name)).filter(|address| address != "::");
        let addresses = IPV6_ADDRESSES.map(|name| address(name).unwrap_or_default());
        let configuration = match (is_on, addresses.iter().any(|address| !address.is_empty())) {
            (false, _) => Ipv6Configuration::Off,
            (true, false) => Ipv6Configuration::Automatic,
            (true, true) => Ipv6Configuration::Manual,
        };
        let [address, gateway, primary_dns, secondary_dns] = addresses;

        Ok(Ipv6Settings { configuration, address, gateway, primary_dns, secondary_dns })
    }

    /// Like MeshCommander, we put the addresses on the port's IPv6 settings (`::` for none), and switch IPv6 on or off
    /// on the link between the two.
    async fn set_ipv6(&mut self, wanted: &Ipv6Settings) -> Result<()> {
        let configuration = wanted.configuration;
        let addresses = [&wanted.address, &wanted.gateway, &wanted.primary_dns, &wanted.secondary_dns];

        // IMPORTANT: We only let addresses through, because AMT takes whatever it is given and an address it cannot
        // use takes it off the network!
        addresses.iter().try_for_each(|address| reject_if_not_address::<Ipv6Addr>(address, "IPv6"))?;

        if configuration != Ipv6Configuration::Off {
            let mut settings = self.enumerate("IPS_IPv6PortSettings").await?.into_iter().next();
            let settings = settings.as_mut().ok_or_else(|| Error::Protocol("the device has no IPv6".into()))?;
            for (name, address) in IPV6_ADDRESSES.into_iter().zip(addresses) {
                let is_manual = configuration == Ipv6Configuration::Manual && !address.is_empty();
                settings.set(name, if is_manual { address.to_lowercase() } else { "::".into() });
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
        let states = [wanted.is_on, wanted.is_asleep, wanted.is_on_battery, wanted.is_asleep_on_battery];
        let policies =
            Availability::POLICIES.into_iter().zip(states).filter_map(|(policy, is_on)| is_on.then_some(policy));
        port.set_all("LinkPolicy", policies);

        self.put_port(port).await
    }

    async fn put_port(&mut self, mut port: Instance) -> Result<()> {
        // Like Intel's console, we send no addresses at all with DHCP or the OS's address because AMT wants none.
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

        // AMT keeps the host and domain name apart, so like MeshCommander we split at the first dot.
        let (host, domain) = wanted.name.split_once('.').unwrap_or((&wanted.name, ""));
        general.set("HostName", host);
        general.set("DomainName", domain);

        general.set("SharedFQDN", wanted.is_shared_with_os);
        general.set("PingResponseEnabled", wanted.is_respond_to_ping);
        general.set("IdleWakeTimeout", wanted.awake_minutes);
        general.set("DDNSUpdateEnabled", wanted.dns_update == DnsUpdate::Amt);
        general.set("DDNSUpdateByDHCPServerEnabled", wanted.dns_update == DnsUpdate::Dhcp);

        self.put("AMT_GeneralSettings", &general).await
    }

    async fn set_power_policy(&mut self, id: &str) -> Result<()> {
        self.invoke_on(id, &SetPowerScheme).await?;
        Ok(())
    }

    /// Sets the device's clock to ours like MeshCommander does: AMT wants the time it said last and when we heard it.
    async fn set_clock(&mut self) -> Result<()> {
        let time = self.query(&GetLowAccuracyTimeSynch).await?;
        let clock = time.parse("Ta0").ok_or_else(|| Error::Protocol("the device didn't say its time".into()))?;
        let now = now();
        self.invoke(&SetHighAccuracyTimeSynch { Ta0: clock, Tm1: now, Tm2: now }).await?;

        Ok(())
    }
}

/// An empty `address` is fine, as that means none.
fn reject_if_not_address<T: FromStr>(address: &str, kind: &str) -> Result<()> {
    match address.is_empty() || address.parse::<T>().is_ok() {
        true => Ok(()),
        false => Err(Error::Invalid(format!("{address} isn't an {kind} address."))),
    }
}

/// The link between the wired port and its IPv6 settings.
fn ipv6_link(element_settings: &[Instance]) -> Option<&Instance> {
    element_settings.iter().find(|element_setting| {
        element_setting.get("SettingData").is_some_and(|reference| reference.ends_with("IPv6 Settings 0"))
    })
}

// The addresses `IPS_IPv6PortSettings` lets us set by hand, in the order of the fields of `Ipv6Settings`.
const IPV6_ADDRESSES: [&str; 4] = ["IPv6Address", "DefaultRouter", "PrimaryDNS", "SecondaryDNS"];

// The IPv4 addresses of a port in `AMT_EthernetPortSettings`.
const ADDRESSES: [&str; 5] = ["IPAddress", "SubnetMask", "DefaultGateway", "PrimaryDNS", "SecondaryDNS"];

/// The host name in `AMT_GeneralSettings`, with its domain if it has one.
pub(crate) fn name(general: &Instance) -> String {
    let (host, domain) = (general.text("HostName"), general.text("DomainName"));
    if domain.is_empty() { host } else { format!("{host}.{domain}") }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

const ELEMENT_SETTING: &str = "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ElementSettingData";

// A link between an element and its settings, with the fields MeshCommander writes back.
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

// NOTE: This is a method of the scheme to activate itself, which is how MeshCommander calls it.
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
