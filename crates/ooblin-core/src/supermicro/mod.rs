//! Supermicro BMCs: Redfish for what they say and do (member `1` of each collection, as Supermicro numbers them), their
//! own web pages for what Redfish doesn't say, IPMI for what every BMC does alike (`ipmi`), and ATEN's iKVM for their
//! screen.

#![allow(non_snake_case, reason = "our JSON types name their fields the way Redfish does, `PowerState` say")]

mod ast2100;
mod kvm;
mod media;

pub use crate::net::ikvm::Ikvm;
pub use crate::net::redfish::{Connection, probe};
pub use crate::net::web::Web;
pub use kvm::Kvm;
pub use media::Media;

use core::net::{Ipv4Addr, Ipv6Addr};

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use ts_rs::TS;

use crate::ipmi::{self, Fru, Network, PowerRestore, SolSettings};
use crate::mac;
use crate::{
    Bios, Configuration, Consent, Disk, Error, Event, Flavor, Hardware, Info, Ipv4, Ipv6, Module, Motherboard, Panel,
    Port, Power, PowerAction, PowerState, Processor, RemoteAccess, Result, Sensor, Severity, System, Target,
};

pub struct Client {
    connection: Connection,
    /// Its IPMI, for what every BMC does alike.
    ipmi: ipmi::Client,
    /// What changes only with the BMC's firmware or the server's hardware, either of which ends our connection.
    facts: OnceCell<Facts>,
}

/// What the server and its BMC are: the day its firmware (in `info`) was built, Redfish's version, the BIOS's and the
/// day it was released, and the server's own network ports. Dates are `2026-05-29`, empty where the BMC's pages don't
/// say.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct BmcStatus {
    pub info: Info,
    pub power: PowerState,
    pub system_id: String,
    pub firmware_built: String,
    pub redfish: String,
    pub bios: String,
    pub bios_released: String,
    pub lans: Vec<Lan>,
    /// The BMC's own port, the one we speak to, as AMT's are its management ports.
    pub ports: Vec<Port>,
}

/// One of the server's network ports: its name (`OnBoard LAN 1`) and MAC address.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Lan {
    pub name: String,
    pub mac: String,
}

/// How the BMC is set up: its network, what it serves on which port, how it runs the fans and our mouse, where it gets the time and
/// sends its log, and the server's power restore policy, serial console and FRU, `None` where the BMC doesn't say. An
/// empty mode is one this BMC doesn't have.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct BmcSettings {
    pub network: Network,
    pub services: Vec<Service>,
    pub fan_mode: Mode,
    pub mouse_mode: Mode,
    pub ntp: Ntp,
    pub syslog: Syslog,
    pub power_restore: Option<PowerRestore>,
    pub sol: Option<SolSettings>,
    pub fru: Option<Fru>,
}

/// A service by Redfish's name (`HTTPS`, `KVMIP`), on or off and on which port. IPMI and HTTPS, which we speak, don't
/// change here.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Service {
    pub name: String,
    pub enabled: bool,
    pub port: Option<u16>,
}

/// The services the BMC's own port page sets, by Redfish's name and the page's.
const SERVICES: [(&str, &str); 5] =
    [("HTTP", "HTTP"), ("SSH", "SSH"), ("KVMIP", "IKVM"), ("VirtualMedia", "VM"), ("SNMP", "SNMP")];

/// One of the BMC's own modes, in Redfish's words (`FullSpeed`), and those it can be set to.
#[derive(Debug, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(default)]
pub struct Mode {
    #[serde(rename(deserialize = "Mode"))]
    pub mode: String,
    #[serde(rename(deserialize = "Mode@Redfish.AllowableValues"))]
    pub modes: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Ntp {
    pub enabled: bool,
    pub primary: String,
    pub secondary: String,
}

/// Where the BMC sends its log, `port` 0 for the usual (514).
#[derive(Debug, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Syslog {
    pub enabled: bool,
    pub server: String,
    pub port: u16,
}

/// A change to the BMC's settings, see `Client::change`. The serial console's bit rate is one of 9600, 19200, 38400,
/// 57600 or 115200.
#[derive(Debug, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "BmcChange")]
pub enum Change {
    FanMode(String),
    MouseMode(String),
    Ntp(Ntp),
    Syslog(Syslog),
    PowerRestore(PowerRestore),
    SolBitRate(u32),
    Services(Vec<Service>),
    Network(Network),
    Fru(Fru),
}

impl Client {
    /// Connects lazily, as `amt::Client::new` does.
    pub fn new(target: &Target, pin: Option<String>) -> Result<Self> {
        Ok(Self { connection: Connection::new(target, pin)?, ipmi: ipmi::Client::new(target), facts: OnceCell::new() })
    }

    /// Refuses a Redfish service that isn't a Supermicro BMC.
    pub async fn info(&mut self) -> Result<Info> {
        Ok(self.manager().await?.0)
    }

    /// The X10 takes half a second a request, its pages and Redfish alike, so we ask all at once, and what doesn't
    /// change only once.
    pub async fn status(&mut self) -> Result<BmcStatus> {
        let system = self.get::<ComputerSystem>("/redfish/v1/Systems/1");
        let facts = self.facts.get_or_try_init(|| self.read_facts());
        let ((info, interface), system, facts) = tokio::try_join!(self.manager(), system, facts)?;
        let facts = facts.clone();
        Ok(BmcStatus {
            info,
            power: system.state(),
            system_id: system.UUID,
            firmware_built: facts.firmware_built,
            redfish: facts.redfish,
            bios: system.BiosVersion,
            bios_released: facts.bios_released,
            lans: facts.lans,
            ports: vec![port(interface)],
        })
    }

    /// The identify light over IPMI, which every BMC has, the POST code from Redfish.
    pub async fn panel(&mut self) -> Result<Panel> {
        let snooping = self.get::<Snooping>("/redfish/v1/Managers/1/Snooping");
        let (snooping, panel) = tokio::try_join!(snooping, self.ipmi.panel())?;
        Ok(Panel { post_code: u8::from_str_radix(&snooping.PostCode, 16).ok(), ..panel })
    }

    /// Lights the identify light until turned off, or turns it off.
    pub async fn identify(&mut self, on: bool) -> Result<()> {
        self.ipmi.identify(on).await
    }

    /// The server's serial console, over IPMI.
    pub async fn sol(&mut self) -> Result<ipmi::Sol> {
        self.ipmi.sol().await
    }

    /// What it offers is what its Reset allows (boot once only into a target it allows), on or off.
    pub async fn power(&mut self) -> Result<Power> {
        use PowerAction::*;
        let system: ComputerSystem = self.get("/redfish/v1/Systems/1").await?;
        let state = system.state();
        let offered: &[_] = match state {
            PowerState::Off => &[On, OnToBios, OnToPxe, OnToImage],
            _ => &[SoftOff, Off, SoftReset, Reset, Nmi, ResetToBios, ResetToPxe, ResetToImage],
        };
        let (resets, targets) = (&system.Actions.reset.types, &system.Boot.targets);
        let offers = |(reset, target): (&str, Option<&str>)| {
            resets.iter().any(|allowed| allowed == reset)
                && target.is_none_or(|target| targets.iter().any(|t| t == target))
        };
        let actions = offered.iter().copied().filter(|&action| reset(action).is_some_and(offers)).collect();
        Ok(Power { state, actions })
    }

    /// Boot once is a boot override first, then the power change.
    pub async fn set_power(&mut self, action: PowerAction) -> Result<()> {
        let (reset, target) = reset(action).ok_or(Error::Unsupported)?;
        if let Some(target) = target {
            let boot = json!({"Boot": {"BootSourceOverrideEnabled": "Once", "BootSourceOverrideTarget": target}});
            self.connection.patch("/redfish/v1/Systems/1", &boot).await?;
        }
        self.connection.post("/redfish/v1/Systems/1/Actions/ComputerSystem.Reset", &json!({"ResetType": reset})).await
    }

    pub async fn hardware(&mut self) -> Result<Hardware> {
        let system: ComputerSystem = self.get("/redfish/v1/Systems/1").await?;
        let chassis: Chassis = self.get("/redfish/v1/Chassis/1").await?;
        let processors: Vec<Cpu> = self.members("/redfish/v1/Systems/1/Processors").await?;
        let memory: Vec<Dimm> = self.members("/redfish/v1/Systems/1/Memory").await?;
        let storage: Vec<Storage> = self.members("/redfish/v1/Systems/1/SimpleStorage").await?;
        // The X10 says a serial number it doesn't have as spaces.
        Ok(Hardware {
            system: System {
                model: system.Model,
                manufacturer: system.Manufacturer,
                version: String::new(),
                serial_number: system.SerialNumber.trim().to_owned(),
                id: system.UUID,
            },
            motherboard: Motherboard {
                product: String::new(),
                manufacturer: chassis.Manufacturer,
                version: String::new(),
                serial_number: chassis.Oem.Supermicro.BoardSerialNumber.trim().to_owned(),
                asset_tag: chassis.AssetTag,
                replaceable: false,
            },
            bios: Bios {
                vendor: String::new(),
                version: system.BiosVersion,
                released: String::new(),
                features: vec![],
            },
            processors: processors
                .into_iter()
                .map(|cpu| Processor {
                    model: cpu.Model,
                    manufacturer: cpu.Manufacturer,
                    family: String::new(),
                    socket: cpu.Socket,
                    mhz: None,
                    max_mhz: cpu.MaxSpeedMHz,
                    status: cpu.Status.State,
                    upgrade_method: String::new(),
                })
                .collect(),
            memory: memory
                .into_iter()
                .map(|dimm| Module {
                    slot: dimm.DeviceLocator,
                    bytes: dimm.CapacityMiB.map(|mib| mib << 20),
                    mhz: dimm.OperatingSpeedMhz,
                    kind: dimm.MemoryDeviceType,
                    form_factor: String::new(),
                    manufacturer: dimm.Manufacturer,
                    part_number: dimm.PartNumber,
                    serial_number: dimm.SerialNumber,
                    asset_tag: String::new(),
                })
                .collect(),
            disks: storage
                .into_iter()
                .flat_map(|storage| storage.Devices)
                .filter(|device| device.Status.State != "Absent")
                .map(|device| Disk { model: device.Model, serial_number: String::new(), bytes: device.CapacityBytes })
                .collect(),
            batteries: vec![],
        })
    }

    /// Newest first. The X10 says how bad an event is only in its raw IPMI data: a threshold crossed is a warning
    /// while non-critical (offsets 0, 1, 6, 7), else critical.
    pub async fn event_log(&mut self) -> Result<Vec<Event>> {
        let log: Collection<Entry> = self.get("/redfish/v1/Systems/1/LogServices/Log1/Entries").await?;
        let byte = |hex: &str| u8::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or_default();
        let events = log.Members.into_iter().rev().map(|entry| {
            let raw = &entry.Oem.Supermicro.RawEventData;
            let threshold = entry.EntryCode != "Deassert" && byte(&raw.EventDirAndType) & 0x7F == 1;
            Event {
                time: entry.Created.parse::<jiff::Timestamp>().map_or(0, |time| time.as_second() as u32),
                source: if entry.SensorType.is_empty() { entry.OemSensorType } else { entry.SensorType },
                description: entry.Message,
                severity: match byte(&raw.EventData1) & 0x0F {
                    _ if !threshold => Severity::Info,
                    0 | 1 | 6 | 7 => Severity::Warning,
                    _ => Severity::Critical,
                },
            }
        });
        Ok(events.collect())
    }

    /// Over IPMI, which reads them in a fifth of the time the X10's Redfish takes, and says what its sensors that
    /// only have states say.
    pub async fn sensors(&mut self) -> Result<Vec<Sensor>> {
        self.ipmi.sensors().await
    }

    /// All asked at once, see `status`.
    pub async fn bmc_settings(&mut self) -> Result<BmcSettings> {
        // A BMC with IPMI turned off has none of these.
        let ipmi = async {
            match self.ipmi.settings().await {
                Ok(settings) => (settings.power_restore, settings.sol, settings.fru),
                Err(error) => {
                    log::warn!("The BMC's IPMI didn't answer: {error}");
                    (None, None, None)
                }
            }
        };
        let (interface, protocols, ntp, syslog, fan_mode, mouse_mode, (power_restore, sol, fru)) = tokio::join!(
            self.get::<EthernetInterface>("/redfish/v1/Managers/1/EthernetInterfaces/1"),
            self.get::<Value>("/redfish/v1/Managers/1/NetworkProtocol"),
            self.optional::<NtpResource>("/redfish/v1/Managers/1/NTP"),
            self.optional::<SyslogResource>("/redfish/v1/Managers/1/Syslog"),
            self.optional("/redfish/v1/Managers/1/FanMode"),
            self.optional("/redfish/v1/Managers/1/MouseMode"),
            ipmi,
        );
        let (interface, protocols, ntp, syslog) = (interface?, protocols?, ntp?, syslog?);
        let services =
            ["HTTP", "HTTPS", "SSH", "IPMI", "KVMIP", "VirtualMedia", "SNMP"].into_iter().filter_map(|name| {
                let protocol = Protocol::deserialize(protocols.get(name)?).ok()?;
                Some(Service { name: name.to_owned(), enabled: protocol.ProtocolEnabled, port: protocol.Port })
            });
        let ipv4 = interface.IPv4Addresses.first().cloned().unwrap_or_default();
        let dns = interface.NameServers.iter().find(|server| server.parse::<Ipv4Addr>().is_ok()).cloned();
        let network = Network {
            hostname: Some(interface.HostName),
            dhcp: ipv4.AddressOrigin == "DHCP",
            address: ipv4.Address,
            subnet_mask: ipv4.SubnetMask,
            gateway: ipv4.Gateway,
            dns: Some(dns.unwrap_or_default()),
            vlan: interface.VLAN.VLANEnable.then_some(interface.VLAN.VLANId),
        };
        Ok(BmcSettings {
            network,
            services: services.collect(),
            fan_mode: fan_mode?,
            mouse_mode: mouse_mode?,
            ntp: Ntp { enabled: ntp.NTPEnable, primary: ntp.PrimaryNTPServer, secondary: ntp.SecondaryNTPServer },
            syslog: Syslog {
                enabled: syslog.EnableSyslog,
                server: syslog.SyslogServerIP,
                port: syslog.SyslogPortNumber,
            },
            power_restore,
            sol,
            fru,
        })
    }

    /// Changes a setting: Redfish's own over Redfish, the server's power restore policy and serial console over IPMI.
    pub async fn change(&mut self, change: Change) -> Result<()> {
        let mode = |mode: String| json!({ "Mode": mode });
        match change {
            Change::FanMode(fan) => self.connection.patch("/redfish/v1/Managers/1/FanMode", &mode(fan)).await,
            Change::MouseMode(mouse) => self.connection.patch("/redfish/v1/Managers/1/MouseMode", &mode(mouse)).await,
            Change::Ntp(ntp) => {
                let ntp = json!({"NTPEnable": ntp.enabled, "PrimaryNTPServer": ntp.primary, "SecondaryNTPServer": ntp.secondary});
                self.connection.patch("/redfish/v1/Managers/1/NTP", &ntp).await
            }
            Change::Syslog(log) => {
                let log =
                    json!({"EnableSyslog": log.enabled, "SyslogServerIP": log.server, "SyslogPortNumber": log.port});
                self.connection.patch("/redfish/v1/Managers/1/Syslog", &log).await
            }
            Change::PowerRestore(restore) => self.ipmi.change(ipmi::Change::PowerRestore(restore)).await,
            Change::SolBitRate(rate) => self.ipmi.change(ipmi::Change::SolBitRate(rate)).await,
            Change::Fru(fru) => self.ipmi.change(ipmi::Change::Fru(fru)).await,
            Change::Services(services) => self.set_services(&services).await,
            Change::Network(network) => self.set_network(&network).await,
        }
    }

    /// The BMC's own web pages, where everything we don't do can be done.
    pub fn web_pages(&self) -> String {
        format!("{}/", self.connection.url())
    }

    /// What a login says, and the BMC's own port for `status`.
    /// Its firmware's version as IPMI says it (`4.07`), as an IPMI login does: Redfish says `4.7`. The manager alone
    /// first, so a wrong password is tried once: the X10 locks the account after five.
    async fn manager(&self) -> Result<(Info, EthernetInterface)> {
        let manager: Manager = self.get("/redfish/v1/Managers/1").await?;
        if manager.Oem.Supermicro.is_none() {
            return Err(Error::Protocol("a Redfish service that isn't a Supermicro BMC".into()));
        }
        let interface = self.get::<EthernetInterface>("/redfish/v1/Managers/1/EthernetInterfaces/1");
        let protocols = self.get::<Value>("/redfish/v1/Managers/1/NetworkProtocol");
        let (interface, protocols, firmware) = tokio::join!(interface, protocols, self.ipmi.firmware());
        let (interface, protocols) = (interface?, protocols?);
        let info = Info {
            flavor: Flavor::Supermicro,
            name: if interface.FQDN.is_empty() { interface.HostName.clone() } else { interface.FQDN.clone() },
            version: firmware.unwrap_or(manager.FirmwareVersion),
            build: None,
            control_mode: String::new(),
            fingerprint: self.connection.fingerprint().expect("a successful call has pinned a certificate"),
            remote_access: RemoteAccess {
                kvm: Some(manager.GraphicalConsole.ServiceEnabled),
                sol: manager.SerialConsole.ServiceEnabled,
                ider: protocols.pointer("/VirtualMedia/ProtocolEnabled") == Some(&Value::Bool(true)),
                consent: Consent::None,
                consent_fixed: true,
            },
        };
        Ok((info, interface))
    }

    /// Turns services on or off and moves their ports as the BMC's own port page does: in one change of all it has,
    /// HTTPS (which we speak) and those Redfish doesn't show (WS-Man, HTTP's redirect to HTTPS) as they are. IPMI isn't
    /// on that page. Turning the KVM off turns virtual media off with it, as the page does, and no two may share a port.
    async fn set_services(&self, services: &[Service]) -> Result<()> {
        let web = Web::login(&self.connection).await?;
        let changed = async {
            let [ports] = <[String; 1]>::try_from(web.read(&["GETPORTSINFO.XML"]).await?).unwrap();
            let mut fields = attributes(&ports, "PORT_INFO")?;
            for service in services {
                let Some((_, key)) = SERVICES.iter().find(|(name, _)| *name == service.name) else { continue };
                set(&mut fields, &format!("{key}_SERVICE"), u8::from(service.enabled).to_string());
                if let Some(port) = service.port {
                    set(&mut fields, &format!("{key}_PORT"), port.to_string());
                }
            }
            if fields.contains(&("IKVM_SERVICE".into(), "0".into())) {
                set(&mut fields, "VM_SERVICE", "0".into());
            }
            let ports: Vec<_> =
                fields.iter().filter(|(name, _)| name.ends_with("_PORT")).map(|(_, port)| port).collect();
            if ports.iter().any(|port| !port.parse::<u16>().is_ok_and(|port| port > 0)) {
                return Err(Error::Invalid("A port is 1 to 65535.".into()));
            }
            if ports.iter().enumerate().any(|(i, port)| ports[..i].contains(port)) {
                return Err(Error::Invalid("Two services can't share a port.".into()));
            }
            // It names the ports something else had, and took none of it then.
            let status = attributes(&web.change("config_port", &fields).await?, "PORTCONF_STATUS")?;
            let taken =
                status.iter().find(|(name, value)| name.contains("_PORT_") && name.ends_with("STATUS") && value == "1");
            match taken.map(|(name, _)| name.split('_').next().unwrap_or_default()) {
                Some(page) => {
                    let service = SERVICES.iter().find(|(_, key)| *key == page).map_or(page, |(name, _)| *name);
                    Err(Error::Invalid(format!("The BMC says the {service} port is in use.")))
                }
                None => Ok(()),
            }
        };
        let changed = changed.await;
        web.logout().await;
        changed
    }

    /// Sets the BMC's network as its own network page does: in one change of all it has, what we don't show (IPv6, its
    /// RMCP port, which LAN it uses, its link) as it is. Its address last on its own page too: once it changes, the BMC
    /// may answer us no more.
    async fn set_network(&self, network: &Network) -> Result<()> {
        network.check()?;
        let web = Web::login(&self.connection).await?;
        let changed = async {
            let [config, ipv6] =
                <[String; 2]>::try_from(web.read(&["CONFIG_INFO.XML", "CONFIG_IPV6.XML"]).await?).unwrap();
            let value = |xml: &str, element: &str, name: &str| -> Result<String> {
                let attributes = attributes(xml, element)?;
                let found = attributes.into_iter().find(|(attribute, _)| attribute == name);
                Ok(found.map(|(_, value)| value.trim().to_owned()).unwrap_or_default())
            };
            let on = |on: bool| if on { "on" } else { "off" }.to_owned();
            let rmcp = u16::from_str_radix(&value(&config, "LAN", "RMCP_PORT")?, 16).unwrap_or(623);
            let vlan = network.vlan.unwrap_or(0);
            let fields = [
                ("bmcip", network.address.clone()),
                ("bmcmask", network.subnet_mask.clone()),
                ("gatewayip", network.gateway.clone()),
                ("en_dhcp", on(network.dhcp)),
                ("en_vlan", on(network.vlan.is_some())),
                ("vlanID", vlan.to_string()),
                ("rmcpport", rmcp.to_string()),
                // No DNS server is all zeros to its page.
                ("dns_server", network.dns.clone().filter(|dns| !dns.is_empty()).unwrap_or("000.000.000.000".into())),
                ("bmcipv6_dns_server", value(&ipv6, "DNS_SERVER", "IP")?),
                ("bmcipv6_addr", String::new()),
                ("bmcipv6_opt", "add".into()),
                ("bmcipv6_autoconf", on(value(&ipv6, "AUTOCONF", "VALUE")? == "1")),
                ("dhcpv6_mode", if value(&ipv6, "DHCPV6", "MODE")? == "1" { "stateful" } else { "stateless" }.into()),
                ("lan_interface", value(&config, "LAN_IF", "INTERFACE")?),
                ("link_conf", value(&config, "LINK_INFO", "MII_LINK_CONF")?),
                ("hostname", network.hostname.clone().unwrap_or_default()),
            ];
            let fields: Vec<_> = fields.into_iter().map(|(name, value)| (name.to_owned(), value)).collect();
            web.change("config_lan", &fields).await.map(drop)
        };
        let changed = changed.await;
        web.logout().await;
        changed
    }

    /// The resource at `path`, or its default where this BMC doesn't have it: an OEM one, say.
    async fn optional<T: DeserializeOwned + Default>(&self, path: &str) -> Result<T> {
        match self.get(path).await {
            Err(Error::Http(404)) => Ok(T::default()),
            resource => resource,
        }
    }

    /// Redfish's version and the server's network ports, and from the BMC's own pages what Redfish doesn't say, which
    /// we go without where they don't say it.
    async fn read_facts(&self) -> Result<Facts> {
        let root = self.get::<Root>("/redfish/v1/");
        let lans = self.members::<EthernetInterface>("/redfish/v1/Systems/1/EthernetInterfaces");
        let (root, lans, pages) = tokio::join!(root, lans, self.dashboard());
        let (firmware_built, bios_released) = pages.unwrap_or_else(|error| {
            log::warn!("The BMC's pages didn't say its firmware's and BIOS's dates: {error}");
            Default::default()
        });
        Ok(Facts {
            firmware_built,
            redfish: root?.RedfishVersion,
            bios_released,
            lans: lans?.into_iter().map(|lan| Lan { name: lan.Name, mac: mac(&lan.MACAddress) }).collect(),
        })
    }

    /// The day the firmware was built and the BIOS released, from the BMC's dashboard's CGI: a web login of its own,
    /// only for this.
    async fn dashboard(&self) -> Result<(String, String)> {
        let web = Web::login(&self.connection).await?;
        let read = web.read(&["GENERIC_INFO.XML", "SMBIOS_INFO.XML"]).await;
        web.logout().await;
        let [generic, smbios] = <[String; 2]>::try_from(read?).unwrap();
        let attribute = |xml: &str, element: &str, name: &str| {
            let xml = roxmltree::Document::parse(xml.trim_start()).ok()?;
            let element = xml.descendants().find(|node| node.has_tag_name(element))?;
            Some(element.attribute(name)?.trim().to_owned())
        };
        // A date as `05/29/2026`.
        let date = |date: Option<String>| {
            let date = date.unwrap_or_default();
            match date.split('/').collect::<Vec<_>>()[..] {
                [month, day, year] => format!("{year}-{month}-{day}"),
                _ => String::new(),
            }
        };
        Ok((date(attribute(&generic, "GENERIC", "IPMIFW_BLDTIME")), date(attribute(&smbios, "BIOS", "REL_DATE"))))
    }

    /// Each member of the collection at `path`, one request each: the X10's `$expand` is broken.
    async fn members<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let collection: Collection<Link> = self.get(path).await?;
        let mut members = Vec::new();
        for member in collection.Members {
            members.push(self.get(&member.id).await?);
        }
        Ok(members)
    }

    /// The resource at `path`, tidied.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut value = self.connection.get(path).await?;
        tidy(&mut value);
        T::deserialize(value).map_err(|error| Error::Protocol(format!("{path}: {error}")))
    }
}

/// The `ResetType` that does `action`, and the target to boot once into first, for what Redfish can do.
fn reset(action: PowerAction) -> Option<(&'static str, Option<&'static str>)> {
    use PowerAction::*;
    Some(match action {
        On => ("On", None),
        OnToBios => ("On", Some("BiosSetup")),
        OnToPxe => ("On", Some("Pxe")),
        OnToImage => ("On", Some("UsbCd")),
        SoftOff => ("GracefulShutdown", None),
        Off => ("ForceOff", None),
        SoftReset => ("GracefulRestart", None),
        Reset => ("ForceRestart", None),
        ResetToBios => ("ForceRestart", Some("BiosSetup")),
        ResetToPxe => ("ForceRestart", Some("Pxe")),
        ResetToImage => ("ForceRestart", Some("UsbCd")),
        Nmi => ("Nmi", None),
        _ => return None,
    })
}

/// What a BMC leaves out comes as null, blanks or SMBIOS's placeholder: we drop them all, so they read as "" or `None`
/// as AMT's do.
fn tidy(value: &mut Value) {
    match value {
        Value::Object(fields) => fields.retain(|_, value| {
            tidy(value);
            !value.is_null()
        }),
        Value::Array(values) => values.iter_mut().for_each(tidy),
        Value::String(text) => {
            let trimmed = text.trim();
            *text = if trimmed == "To be filled by O.E.M." { "" } else { trimmed }.to_owned();
        }
        _ => {}
    }
}

/// The BMC's own port. It names one list of DNS servers for both IPv4 and IPv6, and the X10 its own link-local address
/// as its IPv6 gateway.
fn port(interface: EthernetInterface) -> Port {
    let mut ipv4_dns = interface.NameServers.iter().filter(|server| server.parse::<Ipv4Addr>().is_ok()).cloned();
    let mut ipv6_dns = interface.NameServers.iter().filter(|server| server.parse::<Ipv6Addr>().is_ok()).cloned();
    let ipv4 = interface.IPv4Addresses.into_iter().next().unwrap_or_default();
    let addresses: Vec<_> = interface.IPv6Addresses.into_iter().map(|address| address.Address.to_lowercase()).collect();
    let gateway = Some(interface.IPv6DefaultGateway.to_lowercase()).filter(|gateway| !addresses.contains(gateway));
    Port {
        wireless: false,
        mac: mac(&interface.MACAddress),
        link_up: interface.Status.State == "Enabled" && interface.InterfaceEnabled,
        mbps: interface.SpeedMbps.filter(|&mbps| mbps > 0),
        ipv4: Ipv4 {
            configuration: if ipv4.AddressOrigin == "DHCP" { Configuration::Dhcp } else { Configuration::Manual },
            address: ipv4.Address,
            subnet_mask: ipv4.SubnetMask,
            gateway: ipv4.Gateway,
            primary_dns: ipv4_dns.next().unwrap_or_default(),
            secondary_dns: ipv4_dns.next().unwrap_or_default(),
        },
        ipv6: (!addresses.is_empty()).then(|| Ipv6 {
            addresses,
            gateway: gateway.unwrap_or_default(),
            primary_dns: ipv6_dns.next().unwrap_or_default(),
            secondary_dns: ipv6_dns.next().unwrap_or_default(),
        }),
    }
}

/// The attributes of the element named `element` in the BMC's pages' XML `xml`, in order.
fn attributes(xml: &str, element: &str) -> Result<Vec<(String, String)>> {
    let xml = roxmltree::Document::parse(xml.trim_start()).map_err(|error| Error::Protocol(error.to_string()))?;
    let node = xml.descendants().find(|node| node.has_tag_name(element));
    let node = node.ok_or_else(|| Error::Protocol(format!("the BMC's pages say no {element}")))?;
    Ok(node.attributes().map(|attribute| (attribute.name().to_owned(), attribute.value().to_owned())).collect())
}

/// Sets the field `name` among `fields` to `value`.
fn set(fields: &mut Vec<(String, String)>, name: &str, value: String) {
    match fields.iter_mut().find(|(field, _)| field == name) {
        Some((_, was)) => *was = value,
        None => fields.push((name.to_owned(), value)),
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Vlan {
    VLANEnable: bool,
    VLANId: u16,
}

/// See `Client::read_facts`.
#[derive(Clone)]
struct Facts {
    firmware_built: String,
    redfish: String,
    bios_released: String,
    lans: Vec<Lan>,
}

// Redfish's resources, only what we read of them.

#[derive(Default, Deserialize)]
#[serde(default)]
struct Root {
    RedfishVersion: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Snooping {
    PostCode: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct NtpResource {
    NTPEnable: bool,
    PrimaryNTPServer: String,
    SecondaryNTPServer: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SyslogResource {
    EnableSyslog: bool,
    SyslogServerIP: String,
    SyslogPortNumber: u16,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Manager {
    FirmwareVersion: String,
    GraphicalConsole: Console,
    SerialConsole: Console,
    Oem: Oem<Option<IgnoredAny>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Console {
    ServiceEnabled: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct EthernetInterface {
    Name: String,
    VLAN: Vlan,
    HostName: String,
    FQDN: String,
    MACAddress: String,
    Status: Condition,
    InterfaceEnabled: bool,
    SpeedMbps: Option<u64>,
    IPv4Addresses: Vec<Address>,
    IPv6Addresses: Vec<Address>,
    IPv6DefaultGateway: String,
    NameServers: Vec<String>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Address {
    Address: String,
    SubnetMask: String,
    AddressOrigin: String,
    Gateway: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ComputerSystem {
    PowerState: String,
    UUID: String,
    Model: String,
    Manufacturer: String,
    SerialNumber: String,
    BiosVersion: String,
    Boot: Boot,
    Actions: Actions,
}

impl ComputerSystem {
    fn state(&self) -> PowerState {
        match &*self.PowerState {
            "On" => PowerState::On,
            "Off" => PowerState::Off,
            _ => PowerState::Unknown,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Boot {
    #[serde(rename = "BootSourceOverrideTarget@Redfish.AllowableValues")]
    targets: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Actions {
    #[serde(rename = "#ComputerSystem.Reset")]
    reset: ResetAction,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ResetAction {
    #[serde(rename = "ResetType@Redfish.AllowableValues")]
    types: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Chassis {
    Manufacturer: String,
    AssetTag: String,
    Oem: Oem<Board>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Board {
    BoardSerialNumber: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Cpu {
    Model: String,
    Manufacturer: String,
    Socket: String,
    MaxSpeedMHz: Option<u32>,
    Status: Condition,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Dimm {
    DeviceLocator: String,
    CapacityMiB: Option<u64>,
    OperatingSpeedMhz: Option<u32>,
    MemoryDeviceType: String,
    Manufacturer: String,
    PartNumber: String,
    SerialNumber: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Storage {
    Devices: Vec<Device>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Device {
    Model: String,
    CapacityBytes: Option<u64>,
    Status: Condition,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Entry {
    Created: String,
    EntryCode: String,
    SensorType: String,
    OemSensorType: String,
    Message: String,
    Oem: Oem<Sel>,
}

/// The SEL record behind a log entry, its bytes as hex (`0x01`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Sel {
    RawEventData: RawEventData,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawEventData {
    EventDirAndType: String,
    EventData1: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Protocol {
    ProtocolEnabled: bool,
    Port: Option<u16>,
}

/// A resource's `Status`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Condition {
    State: String,
    Health: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Oem<T> {
    Supermicro: T,
}

#[derive(Deserialize)]
struct Collection<T> {
    Members: Vec<T>,
}

#[derive(Deserialize)]
struct Link {
    #[serde(rename = "@odata.id")]
    id: String,
}
