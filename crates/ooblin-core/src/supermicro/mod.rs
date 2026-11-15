//! Supermicro BMCs: Redfish for what they say and do (`redfish`, member `1` of each collection as Supermicro numbers
//! them, and what theirs has of its own), their own web pages for what Redfish doesn't say, IPMI for what every BMC
//! does alike (`ipmi`), and ATEN's iKVM for their screen.

#![allow(non_snake_case, reason = "our JSON types name their fields the way Redfish does, `PowerState` say")]

mod ast2100;
mod kvm;
mod media;

pub use crate::net::ikvm::Ikvm;
pub use crate::net::web::Web;
pub use crate::redfish::{BmcStatus, Connection, Lan, probe};
pub use kvm::Kvm;
pub use media::Media;

use core::net::Ipv4Addr;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use ts_rs::TS;

use crate::ipmi::{self, Fru, Network, PowerRestore, SolSettings, sel};
use crate::mac;
use crate::redfish::{self, Collection, ComputerSystem, Cpu, Dimm, EthernetInterface, Paths, Root, Storage, port};
use crate::{
    Consent, Error, Event, Flavor, Hardware, Info, Motherboard, Panel, Power, PowerAction, RemoteAccess, Result,
    Sensor, Severity, Target,
};

pub struct Client {
    /// Its Redfish, for what the standard says.
    redfish: redfish::Client,
    /// Its IPMI, for what every BMC does alike.
    ipmi: ipmi::Client,
    /// What changes only with the BMC's firmware or the server's hardware, either of which ends our connection.
    facts: OnceCell<Facts>,
    /// Its firmware's version as IPMI says it, which changes no more than `facts` do, kept apart because a login reads
    /// it and not them. `None` where IPMI didn't say, which we don't wait for again with every `status`.
    firmware: OnceCell<Option<String>>,
}

/// How the BMC is set up: its network, what it serves on which port, how it runs the fans and our mouse, where it gets
/// the time and sends its log, and the server's power restore policy, serial console and FRU, `None` where the BMC
/// doesn't say. An empty mode is one this BMC doesn't have.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct BmcSettings {
    pub network: Network,
    pub services: Vec<Service>,
    /// What it lets us redirect now, as `Info` said on login.
    pub remote_access: RemoteAccess,
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
        let paths = Paths {
            system: "/redfish/v1/Systems/1".into(),
            manager: "/redfish/v1/Managers/1".into(),
            chassis: "/redfish/v1/Chassis/1".into(),
        };
        Ok(Self {
            redfish: redfish::Client::at(target, pin, paths)?,
            ipmi: ipmi::Client::new(target),
            facts: OnceCell::new(),
            firmware: OnceCell::new(),
        })
    }

    /// Refuses a Redfish service that isn't a Supermicro BMC.
    pub async fn info(&mut self) -> Result<Info> {
        Ok(self.manager().await?.0)
    }

    /// The X10 takes half a second a request, its pages and Redfish alike, so we ask all at once, and what doesn't
    /// change only once. Only a login asks alone first (`manager`): a password changed since is tried by each of these.
    pub async fn status(&mut self) -> Result<BmcStatus> {
        let system = self.redfish.get::<ComputerSystem>("/redfish/v1/Systems/1");
        let facts = self.facts.get_or_try_init(|| self.read_facts());
        // Not `try_join!`, which would drop `dashboard` between its web login and logout.
        let (manager, system, facts) = tokio::join!(self.manager(), system, facts);
        let ((info, interface), system, facts) = (manager?, system?, facts?.clone());
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

    /// The identify light over IPMI, which every BMC has unless it's turned off, the POST code from Redfish.
    pub async fn panel(&mut self) -> Result<Panel> {
        let snooping = self.redfish.optional::<Snooping>("/redfish/v1/Managers/1/Snooping");
        let (snooping, panel) = tokio::join!(snooping, self.ipmi.panel());
        let identify = panel.inspect_err(|error| log::warn!("The BMC's IPMI didn't answer: {error}")).ok();
        Ok(Panel {
            identify: identify.and_then(|panel| panel.identify),
            post_code: u8::from_str_radix(&snooping?.PostCode, 16).ok(),
        })
    }

    /// Lights the identify light until turned off, or turns it off.
    pub async fn identify(&mut self, on: bool) -> Result<()> {
        self.ipmi.identify(on).await
    }

    /// The server's serial console, over IPMI.
    pub async fn sol(&mut self) -> Result<ipmi::Sol> {
        self.ipmi.sol().await
    }

    pub async fn power(&mut self) -> Result<Power> {
        self.redfish.power().await
    }

    pub async fn set_power(&mut self, action: PowerAction) -> Result<()> {
        self.redfish.set_power(action).await
    }

    /// Its Redfish alone, for what the standard says and we read another way: its sensors, say.
    pub fn redfish(&self) -> &redfish::Client {
        &self.redfish
    }

    /// The system alone first, so a wrong password is tried once where this is the first we ask.
    pub async fn hardware(&mut self) -> Result<Hardware> {
        let system: ComputerSystem = self.redfish.get("/redfish/v1/Systems/1").await?;
        let (chassis, processors, memory, storage) = tokio::try_join!(
            self.redfish.get::<Chassis>("/redfish/v1/Chassis/1"),
            self.redfish.members::<Cpu>("/redfish/v1/Systems/1/Processors"),
            self.redfish.members::<Dimm>("/redfish/v1/Systems/1/Memory"),
            self.redfish.members::<Storage>("/redfish/v1/Systems/1/SimpleStorage"),
        )?;
        Ok(Hardware {
            motherboard: Motherboard {
                product: String::new(),
                manufacturer: chassis.Manufacturer,
                version: String::new(),
                serial_number: chassis.Oem.Supermicro.BoardSerialNumber,
                asset_tag: chassis.AssetTag,
                replaceable: false,
            },
            processors: processors.into_iter().map(Cpu::processor).collect(),
            memory: memory.into_iter().map(Dimm::module).collect(),
            disks: storage.into_iter().flat_map(Storage::disks).collect(),
            batteries: vec![],
            bios: system.bios(),
            system: system.system(),
        })
    }

    /// Newest first. The X10 says how bad an event is only in its raw IPMI data, which we rate as `ipmi`'s SEL.
    pub async fn event_log(&mut self) -> Result<Vec<Event>> {
        let log: Collection<Entry> = self.redfish.get("/redfish/v1/Systems/1/LogServices/Log1/Entries").await?;
        let byte = |hex: &str| u8::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or_default();
        let events = log.Members.into_iter().rev().map(|entry| {
            let raw = &entry.Oem.Supermicro.RawEventData;
            let state = sel::state(byte(&raw.SensorType), byte(&raw.EventDirAndType), byte(&raw.EventData1));
            Event {
                time: entry.Created.parse::<jiff::Timestamp>().map_or(0, |time| time.as_second() as u32),
                source: if entry.SensorType.is_empty() { entry.OemSensorType } else { entry.SensorType },
                description: entry.Message,
                severity: state.map_or(Severity::Info, |(_, severity)| severity),
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
            self.ipmi
                .server_settings()
                .await
                .inspect_err(|error| log::warn!("The BMC's IPMI didn't answer: {error}"))
                .unwrap_or_default()
        };
        let (manager, interface, protocols, ntp, syslog, fan_mode, mouse_mode, ipmi) = tokio::join!(
            self.redfish.get::<Manager>("/redfish/v1/Managers/1"),
            self.redfish.get::<EthernetInterface>("/redfish/v1/Managers/1/EthernetInterfaces/1"),
            self.redfish.get::<Value>("/redfish/v1/Managers/1/NetworkProtocol"),
            self.redfish.optional::<NtpResource>("/redfish/v1/Managers/1/NTP"),
            self.redfish.optional::<SyslogResource>("/redfish/v1/Managers/1/Syslog"),
            self.redfish.optional("/redfish/v1/Managers/1/FanMode"),
            self.redfish.optional("/redfish/v1/Managers/1/MouseMode"),
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
            remote_access: remote_access(&manager?, &protocols),
            fan_mode: fan_mode?,
            mouse_mode: mouse_mode?,
            ntp: Ntp { enabled: ntp.NTPEnable, primary: ntp.PrimaryNTPServer, secondary: ntp.SecondaryNTPServer },
            syslog: Syslog {
                enabled: syslog.EnableSyslog,
                server: syslog.SyslogServerIP,
                port: syslog.SyslogPortNumber,
            },
            power_restore: ipmi.power_restore,
            sol: ipmi.sol,
            fru: ipmi.fru,
        })
    }

    /// Changes a setting: Redfish's own over Redfish, the server's power restore policy, serial console and FRU over
    /// IPMI, its services and network as its own pages do, in a web session each.
    pub async fn change(&mut self, change: Change) -> Result<()> {
        let mode = |mode: String| json!({ "Mode": mode });
        match change {
            Change::FanMode(fan) => self.redfish.connection.patch("/redfish/v1/Managers/1/FanMode", &mode(fan)).await,
            Change::MouseMode(mouse) => {
                self.redfish.connection.patch("/redfish/v1/Managers/1/MouseMode", &mode(mouse)).await
            }
            Change::Ntp(ntp) => {
                let ntp = json!({
                    "NTPEnable": ntp.enabled,
                    "PrimaryNTPServer": ntp.primary,
                    "SecondaryNTPServer": ntp.secondary,
                });
                self.redfish.connection.patch("/redfish/v1/Managers/1/NTP", &ntp).await
            }
            Change::Syslog(log) => {
                let log =
                    json!({"EnableSyslog": log.enabled, "SyslogServerIP": log.server, "SyslogPortNumber": log.port});
                self.redfish.connection.patch("/redfish/v1/Managers/1/Syslog", &log).await
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
        self.redfish.web_pages()
    }

    /// What a login says, and the BMC's own port for `status`: its firmware's version as IPMI says it (`4.07`), as an
    /// IPMI login does, where Redfish says `4.7`. The manager alone first, so a login tries a wrong password once: the
    /// X10 locks the account after five. Another vendor's has no manager `1`, or one that isn't Supermicro's.
    async fn manager(&self) -> Result<(Info, EthernetInterface)> {
        let manager: Manager = self.redfish.optional("/redfish/v1/Managers/1").await?;
        if manager.Oem.Supermicro.is_none() {
            return Err(Error::Protocol("a Redfish service that isn't a Supermicro BMC".into()));
        }
        let interface = self.redfish.get::<EthernetInterface>("/redfish/v1/Managers/1/EthernetInterfaces/1");
        let protocols = self.redfish.get::<Value>("/redfish/v1/Managers/1/NetworkProtocol");
        let firmware = self.firmware.get_or_init(async || {
            let firmware = self.ipmi.firmware().await;
            firmware.inspect_err(|error| log::warn!("The BMC's IPMI didn't say its firmware's version: {error}")).ok()
        });
        let (interface, protocols, firmware) = tokio::join!(interface, protocols, firmware);
        let (interface, protocols) = (interface?, protocols?);
        let remote_access = remote_access(&manager, &protocols);
        let info = Info {
            flavor: Flavor::Supermicro,
            name: if interface.FQDN.is_empty() { interface.HostName.clone() } else { interface.FQDN.clone() },
            version: firmware.clone().unwrap_or(manager.FirmwareVersion),
            build: String::new(),
            control_mode: String::new(),
            fingerprint: self.redfish.connection.fingerprint().expect("a successful call has pinned a certificate"),
            remote_access,
        };
        Ok((info, interface))
    }

    /// Turns services on or off and moves their ports as the BMC's own port page does: in one change of all it has,
    /// HTTPS (which we speak) and those Redfish doesn't show (WS-Man, HTTP's redirect to HTTPS) as they are. IPMI
    /// isn't on that page. Turning the KVM off turns virtual media off with it, as the page does, and no two may share
    /// a port.
    async fn set_services(&self, services: &[Service]) -> Result<()> {
        Web::with(&self.redfish.connection, async |web| {
            let [ports] = web.read(["GETPORTSINFO.XML"]).await?;
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
        })
        .await
    }

    /// Sets the BMC's network as its own network page does: in one change of all it has, what we don't show (IPv6, its
    /// RMCP port, which LAN it uses, its link) as it is. Its address last on its own page too: once it changes, the BMC
    /// may answer us no more.
    async fn set_network(&self, network: &Network) -> Result<()> {
        network.check()?;
        Web::with(&self.redfish.connection, async |web| {
            let [config, ipv6] = web.read(["CONFIG_INFO.XML", "CONFIG_IPV6.XML"]).await?;
            let on = |on: bool| if on { "on" } else { "off" }.to_owned();
            let dhcpv6 = if attribute(&ipv6, "DHCPV6", "MODE")? == "1" { "stateful" } else { "stateless" };
            let rmcp = attribute(&config, "LAN", "RMCP_PORT")?;
            let rmcp = u16::from_str_radix(&rmcp, 16)
                .map_err(|_| Error::Protocol(format!("the BMC's pages say RMCP port {rmcp}")))?;
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
                ("bmcipv6_dns_server", attribute(&ipv6, "DNS_SERVER", "IP")?),
                ("bmcipv6_addr", String::new()),
                ("bmcipv6_opt", "add".into()),
                ("bmcipv6_autoconf", on(attribute(&ipv6, "AUTOCONF", "VALUE")? == "1")),
                ("dhcpv6_mode", dhcpv6.into()),
                ("lan_interface", attribute(&config, "LAN_IF", "INTERFACE")?),
                ("link_conf", attribute(&config, "LINK_INFO", "MII_LINK_CONF")?),
                ("hostname", network.hostname.clone().unwrap_or_default()),
            ];
            let fields: Vec<_> = fields.into_iter().map(|(name, value)| (name.to_owned(), value)).collect();
            web.change("config_lan", &fields).await.map(drop)
        })
        .await
    }

    /// Redfish's version and the server's network ports, and from the BMC's own pages what Redfish doesn't say, which
    /// we go without where they don't say it.
    async fn read_facts(&self) -> Result<Facts> {
        let root = self.redfish.get::<Root>("/redfish/v1/");
        let lans = self.redfish.members::<EthernetInterface>("/redfish/v1/Systems/1/EthernetInterfaces");
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
        let [generic, smbios] =
            Web::with(&self.redfish.connection, async |web| web.read(["GENERIC_INFO.XML", "SMBIOS_INFO.XML"]).await)
                .await?;
        // A date as `05/29/2026`.
        let date = |date: Result<String>| match date.unwrap_or_default().split('/').collect::<Vec<_>>()[..] {
            [month, day, year] => format!("{year}-{month}-{day}"),
            _ => String::new(),
        };
        Ok((date(attribute(&generic, "GENERIC", "IPMIFW_BLDTIME")), date(attribute(&smbios, "BIOS", "REL_DATE"))))
    }
}

/// The attributes of the element named `element` in the BMC's pages' XML `xml`, in order.
fn attributes(xml: &str, element: &str) -> Result<Vec<(String, String)>> {
    let xml = roxmltree::Document::parse(xml.trim_start()).map_err(|error| Error::Protocol(error.to_string()))?;
    let node = xml.descendants().find(|node| node.has_tag_name(element));
    let node = node.ok_or_else(|| Error::Protocol(format!("the BMC's pages say no {element}")))?;
    Ok(node.attributes().map(|attribute| (attribute.name().to_owned(), attribute.value().to_owned())).collect())
}

/// The attribute `name` of the element named `element` in the BMC's pages' XML `xml`, trimmed. One it doesn't have is
/// an error: we write some back as they were, which a blank isn't.
fn attribute(xml: &str, element: &str, name: &str) -> Result<String> {
    let found = attributes(xml, element)?.into_iter().find(|(attribute, _)| attribute == name);
    let found = found.ok_or_else(|| Error::Protocol(format!("the BMC's pages say no {name} of {element}")))?;
    Ok(found.1.trim().to_owned())
}

/// Sets the field `name` among `fields` to `value`.
fn set(fields: &mut Vec<(String, String)>, name: &str, value: String) {
    match fields.iter_mut().find(|(field, _)| field == name) {
        Some((_, was)) => *was = value,
        None => fields.push((name.to_owned(), value)),
    }
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

/// What a BMC lets us redirect. Its KVM and virtual media as its services say, which is what turning them off changes.
fn remote_access(manager: &Manager, protocols: &Value) -> RemoteAccess {
    let enabled = |service: &str| protocols.pointer(&format!("/{service}/ProtocolEnabled")).and_then(Value::as_bool);
    RemoteAccess {
        kvm: Some(enabled("KVMIP").unwrap_or(manager.GraphicalConsole.ServiceEnabled)),
        sol: manager.SerialConsole.ServiceEnabled,
        ider: enabled("VirtualMedia") == Some(true),
        consent: Consent::None,
        consent_fixed: true,
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Console {
    ServiceEnabled: bool,
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
struct Entry {
    Created: String,
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
    SensorType: String,
    EventDirAndType: String,
    EventData1: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Protocol {
    ProtocolEnabled: bool,
    Port: Option<u16>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Oem<T> {
    Supermicro: T,
}
