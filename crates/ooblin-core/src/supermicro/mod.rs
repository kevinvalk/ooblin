#![allow(non_snake_case, reason = "We name the fields of our JSON types the way Redfish does, `PowerState` say.")]

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

// NOTE: A Supermicro BMC speaks Redfish for what the standard says, has its own web pages for what Redfish does not
// say, IPMI for what every BMC does alike and ATEN's iKVM for its screen.
pub struct Client {
    redfish: redfish::Client,
    ipmi: ipmi::Client,

    /// What changes only with the BMC's firmware or the server's hardware, either of which ends our connection.
    facts: OnceCell<Facts>,

    /// The firmware's version as IPMI says it, which changes no more than `facts` do. We keep it apart because a login
    /// reads it and not them. `None` where IPMI did not say, so we do not wait for it again with every `status`.
    firmware: OnceCell<Option<String>>,
}

/// How the BMC is set up: its network, what it serves on which port, how it runs the fans and our mouse, where it gets
/// the time and sends its log, and the server's power restore policy, serial console and FRU, `None` where the BMC
/// does not say. An empty mode is one this BMC does not have.
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

/// A service by Redfish's name (`HTTPS`, `KVMIP`), on or off and on which port. IPMI and HTTPS, which we speak, do not
/// change here.
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Service {
    pub name: String,
    pub is_enabled: bool,
    pub port: Option<u16>,
}

// The services the BMC's own port page sets, by Redfish's name and the page's.
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
    pub is_enabled: bool,
    pub primary: String,
    pub secondary: String,
}

/// Where the BMC sends its log, `port` 0 for the usual (514).
#[derive(Debug, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Syslog {
    pub is_enabled: bool,
    pub server: String,
    pub port: u16,
}

/// A change to the BMC's settings, see `Client::change`. The serial console's bit rate is one of 9600, 19200, 38400,
/// 57600 or 115200.
#[derive(Debug, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum BmcChange {
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
    /// We connect lazily, as `amt::Client::new` does.
    pub fn new(target: &Target, pin: Option<String>) -> Result<Self> {
        // Supermicro numbers the members of each collection, so we know where they are without asking.
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

    /// We refuse a Redfish service that is not a Supermicro BMC.
    pub async fn info(&mut self) -> Result<Info> {
        Ok(self.manager().await?.0)
    }

    /// The X10 takes half a second a request, its pages and Redfish alike, so we ask all at once, and what does not
    /// change only once. Only a login asks alone first (`manager`): a password changed since is tried by each of these.
    pub async fn status(&mut self) -> Result<BmcStatus> {
        let system = self.redfish.get::<ComputerSystem>("/redfish/v1/Systems/1");
        let facts = self.facts.get_or_try_init(|| self.read_facts());

        // NOTE: We do not use `try_join!` because it would drop `dashboard` between its web login and logout.
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

    /// The identify light comes over IPMI, which every BMC has unless it is turned off, the POST code from Redfish.
    pub async fn panel(&mut self) -> Result<Panel> {
        let snooping = self.redfish.optional::<Snooping>("/redfish/v1/Managers/1/Snooping");
        let (snooping, panel) = tokio::join!(snooping, self.ipmi.panel());
        let panel = panel.inspect_err(|error| log::warn!("The BMC's IPMI didn't answer: {error}")).ok();

        Ok(Panel {
            is_identifying: panel.and_then(|panel| panel.is_identifying),
            post_code: u8::from_str_radix(&snooping?.PostCode, 16).ok(),
        })
    }

    /// The identify light stays lit until we turn it off.
    pub async fn identify(&mut self, is_on: bool) -> Result<()> {
        self.ipmi.identify(is_on).await
    }

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

    pub async fn hardware(&mut self) -> Result<Hardware> {
        // We ask for the system alone first, so a wrong password is tried once where this is the first we ask.
        let system: ComputerSystem = self.redfish.get("/redfish/v1/Systems/1").await?;

        let (chassis, processors, memory, storages) = tokio::try_join!(
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
                is_replaceable: false,
            },
            processors: processors.into_iter().map(Cpu::processor).collect(),
            memory: memory.into_iter().map(Dimm::module).collect(),
            disks: storages.into_iter().flat_map(Storage::disks).collect(),
            batteries: vec![],
            bios: system.bios(),
            system: system.system(),
        })
    }

    /// Newest first. The X10 says how bad an event is only in its raw IPMI data, which we rate as `ipmi` does its SEL.
    pub async fn event_log(&mut self) -> Result<Vec<Event>> {
        let log: Collection<Entry> = self.redfish.get("/redfish/v1/Systems/1/LogServices/Log1/Entries").await?;
        let byte = |hex: &str| u8::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or_default();

        let events = log.Members.into_iter().rev().map(|entry| {
            let raw = &entry.Oem.Supermicro.RawEventData;
            let state = sel::state(byte(&raw.SensorType), byte(&raw.EventDirAndType), byte(&raw.EventData1));

            Event {
                occurred_at: entry.Created.parse::<jiff::Timestamp>().map_or(0, |time| time.as_second() as u32),
                source: if entry.SensorType.is_empty() { entry.OemSensorType } else { entry.SensorType },
                description: entry.Message,
                severity: state.map_or(Severity::Info, |(_, severity)| severity),
            }
        });

        Ok(events.collect())
    }

    /// We read the sensors over IPMI, which takes a fifth of the time the X10's Redfish does and also says what the
    /// sensors that only have states say.
    pub async fn sensors(&mut self) -> Result<Vec<Sensor>> {
        self.ipmi.sensors().await
    }

    /// We ask all at once, see `status`.
    pub async fn bmc_settings(&mut self) -> Result<BmcSettings> {
        // A BMC with IPMI turned off has none of these, so we go without them.
        let server_settings = async {
            self.ipmi
                .server_settings()
                .await
                .inspect_err(|error| log::warn!("The BMC's IPMI didn't answer: {error}"))
                .unwrap_or_default()
        };

        let (manager, interface, protocols, ntp, syslog, fan_mode, mouse_mode, server_settings) = tokio::join!(
            self.redfish.get::<Manager>("/redfish/v1/Managers/1"),
            self.redfish.get::<EthernetInterface>("/redfish/v1/Managers/1/EthernetInterfaces/1"),
            self.redfish.get::<Value>("/redfish/v1/Managers/1/NetworkProtocol"),
            self.redfish.optional::<NtpResource>("/redfish/v1/Managers/1/NTP"),
            self.redfish.optional::<SyslogResource>("/redfish/v1/Managers/1/Syslog"),
            self.redfish.optional("/redfish/v1/Managers/1/FanMode"),
            self.redfish.optional("/redfish/v1/Managers/1/MouseMode"),
            server_settings,
        );
        let (interface, protocols, ntp, syslog) = (interface?, protocols?, ntp?, syslog?);

        let services =
            ["HTTP", "HTTPS", "SSH", "IPMI", "KVMIP", "VirtualMedia", "SNMP"].into_iter().filter_map(|name| {
                let protocol = Protocol::deserialize(protocols.get(name)?).ok()?;
                Some(Service { name: name.to_owned(), is_enabled: protocol.ProtocolEnabled, port: protocol.Port })
            });

        let ipv4 = interface.IPv4Addresses.first().cloned().unwrap_or_default();
        let dns = interface.NameServers.iter().find(|server| server.parse::<Ipv4Addr>().is_ok()).cloned();
        let network = Network {
            hostname: Some(interface.HostName),
            is_dhcp: ipv4.AddressOrigin == "DHCP",
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
            ntp: Ntp { is_enabled: ntp.NTPEnable, primary: ntp.PrimaryNTPServer, secondary: ntp.SecondaryNTPServer },
            syslog: Syslog {
                is_enabled: syslog.EnableSyslog,
                server: syslog.SyslogServerIP,
                port: syslog.SyslogPortNumber,
            },
            power_restore: server_settings.power_restore,
            sol: server_settings.sol,
            fru: server_settings.fru,
        })
    }

    /// We change Redfish's own settings over Redfish, the server's power restore policy, serial console and FRU over
    /// IPMI, and the BMC's services and network as its own pages do, in a web session each.
    pub async fn change(&mut self, change: BmcChange) -> Result<()> {
        let mode = |mode: String| json!({ "Mode": mode });

        match change {
            BmcChange::FanMode(fan_mode) => {
                self.redfish.connection.patch("/redfish/v1/Managers/1/FanMode", &mode(fan_mode)).await
            }
            BmcChange::MouseMode(mouse_mode) => {
                self.redfish.connection.patch("/redfish/v1/Managers/1/MouseMode", &mode(mouse_mode)).await
            }
            BmcChange::Ntp(ntp) => {
                let ntp = json!({
                    "NTPEnable": ntp.is_enabled,
                    "PrimaryNTPServer": ntp.primary,
                    "SecondaryNTPServer": ntp.secondary,
                });
                self.redfish.connection.patch("/redfish/v1/Managers/1/NTP", &ntp).await
            }
            BmcChange::Syslog(syslog) => {
                let syslog = json!({
                    "EnableSyslog": syslog.is_enabled,
                    "SyslogServerIP": syslog.server,
                    "SyslogPortNumber": syslog.port,
                });
                self.redfish.connection.patch("/redfish/v1/Managers/1/Syslog", &syslog).await
            }
            BmcChange::PowerRestore(power_restore) => self.ipmi.change(ipmi::Change::PowerRestore(power_restore)).await,
            BmcChange::SolBitRate(bit_rate) => self.ipmi.change(ipmi::Change::SolBitRate(bit_rate)).await,
            BmcChange::Fru(fru) => self.ipmi.change(ipmi::Change::Fru(fru)).await,
            BmcChange::Services(services) => self.set_services(&services).await,
            BmcChange::Network(network) => self.set_network(&network).await,
        }
    }

    /// The URL of the BMC's own web pages, where everything we do not do can be done.
    pub fn web_pages(&self) -> String {
        self.redfish.web_pages()
    }

    /// What a login says, and the BMC's own port for `status`.
    async fn manager(&self) -> Result<(Info, EthernetInterface)> {
        // We ask for the manager alone first, so a login tries a wrong password once: the X10 locks the account after
        // five. Another vendor's service has no manager `1`, or one that is not Supermicro's.
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
            // We take the firmware's version as IPMI says it (`4.07`), as an IPMI login does, where Redfish says `4.7`.
            version: firmware.clone().unwrap_or(manager.FirmwareVersion),
            build: String::new(),
            control_mode: String::new(),
            fingerprint: self.redfish.connection.fingerprint().expect("a successful call has pinned a certificate"),
            remote_access,
        };

        Ok((info, interface))
    }

    /// We turn services on or off and move their ports as the BMC's own port page does: in one change of all it has,
    /// HTTPS (which we speak) and those Redfish does not show (WS-Man, HTTP's redirect to HTTPS) as they are. IPMI is
    /// not on that page.
    async fn set_services(&self, services: &[Service]) -> Result<()> {
        Web::with(&self.redfish.connection, async |web| {
            let [ports] = web.read(["GETPORTSINFO.XML"]).await?;
            let mut fields = attributes(&ports, "PORT_INFO")?;

            for service in services {
                let Some((_, page)) = SERVICES.iter().find(|(name, _)| *name == service.name) else { continue };

                set(&mut fields, &format!("{page}_SERVICE"), u8::from(service.is_enabled).to_string());
                if let Some(port) = service.port {
                    set(&mut fields, &format!("{page}_PORT"), port.to_string());
                }
            }

            // Turning the KVM off turns virtual media off with it, as the page does.
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

            // The BMC names the ports something else already had, and then it took none of our change.
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

    /// We set the BMC's network as its own network page does: in one change of all it has, what we do not show (IPv6,
    /// its RMCP port, which LAN it uses, its link) as it is. Its address goes last on its own page too: once it
    /// changes, the BMC may answer us no more.
    async fn set_network(&self, network: &Network) -> Result<()> {
        network.reject_if_invalid()?;

        Web::with(&self.redfish.connection, async |web| {
            let [configuration, ipv6] = web.read(["CONFIG_INFO.XML", "CONFIG_IPV6.XML"]).await?;
            let on = |is_on: bool| if is_on { "on" } else { "off" }.to_owned();
            let dhcpv6_mode = if attribute(&ipv6, "DHCPV6", "MODE")? == "1" { "stateful" } else { "stateless" };
            let rmcp_port = attribute(&configuration, "LAN", "RMCP_PORT")?;
            let rmcp_port = u16::from_str_radix(&rmcp_port, 16)
                .map_err(|_| Error::Protocol(format!("the BMC's pages say RMCP port {rmcp_port}")))?;
            let vlan = network.vlan.unwrap_or(0);

            let fields = [
                ("bmcip", network.address.clone()),
                ("bmcmask", network.subnet_mask.clone()),
                ("gatewayip", network.gateway.clone()),
                ("en_dhcp", on(network.is_dhcp)),
                ("en_vlan", on(network.vlan.is_some())),
                ("vlanID", vlan.to_string()),
                ("rmcpport", rmcp_port.to_string()),
                // The page takes all zeros for no DNS server.
                ("dns_server", network.dns.clone().filter(|dns| !dns.is_empty()).unwrap_or("000.000.000.000".into())),
                ("bmcipv6_dns_server", attribute(&ipv6, "DNS_SERVER", "IP")?),
                ("bmcipv6_addr", String::new()),
                ("bmcipv6_opt", "add".into()),
                ("bmcipv6_autoconf", on(attribute(&ipv6, "AUTOCONF", "VALUE")? == "1")),
                ("dhcpv6_mode", dhcpv6_mode.into()),
                ("lan_interface", attribute(&configuration, "LAN_IF", "INTERFACE")?),
                ("link_conf", attribute(&configuration, "LINK_INFO", "MII_LINK_CONF")?),
                ("hostname", network.hostname.clone().unwrap_or_default()),
            ];
            let fields: Vec<_> = fields.into_iter().map(|(name, value)| (name.to_owned(), value)).collect();

            web.change("config_lan", &fields).await.map(drop)
        })
        .await
    }

    async fn read_facts(&self) -> Result<Facts> {
        let root = self.redfish.get::<Root>("/redfish/v1/");
        let lans = self.redfish.members::<EthernetInterface>("/redfish/v1/Systems/1/EthernetInterfaces");
        let (root, lans, dates) = tokio::join!(root, lans, self.dashboard());

        // The BMC's own pages say what Redfish does not, and we go without it where they do not say it either.
        let (firmware_built, bios_released) = dates.unwrap_or_else(|error| {
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

    /// The day the firmware was built and the BIOS released, from the BMC's dashboard's CGI. This takes a web login of
    /// its own, only for this.
    async fn dashboard(&self) -> Result<(String, String)> {
        let [generic, smbios] =
            Web::with(&self.redfish.connection, async |web| web.read(["GENERIC_INFO.XML", "SMBIOS_INFO.XML"]).await)
                .await?;

        // The pages write a date as `05/29/2026`.
        let date = |date: Result<String>| match date.unwrap_or_default().split('/').collect::<Vec<_>>()[..] {
            [month, day, year] => format!("{year}-{month}-{day}"),
            _ => String::new(),
        };

        Ok((date(attribute(&generic, "GENERIC", "IPMIFW_BLDTIME")), date(attribute(&smbios, "BIOS", "REL_DATE"))))
    }
}

/// The attributes of the element named `element` in the XML of one of the BMC's pages, in the order the page has them.
fn attributes(xml: &str, element: &str) -> Result<Vec<(String, String)>> {
    let document = roxmltree::Document::parse(xml.trim_start()).map_err(|error| Error::Protocol(error.to_string()))?;
    let node = document.descendants().find(|node| node.has_tag_name(element));
    let node = node.ok_or_else(|| Error::Protocol(format!("the BMC's pages say no {element}")))?;

    Ok(node.attributes().map(|attribute| (attribute.name().to_owned(), attribute.value().to_owned())).collect())
}

/// An attribute the page does not have is an error, because we write some back as they were and a blank is not that.
fn attribute(xml: &str, element: &str, name: &str) -> Result<String> {
    let attribute = attributes(xml, element)?.into_iter().find(|(attribute, _)| attribute == name);
    let (_, value) = attribute.ok_or_else(|| Error::Protocol(format!("the BMC's pages say no {name} of {element}")))?;

    Ok(value.trim().to_owned())
}

fn set(fields: &mut Vec<(String, String)>, name: &str, value: String) {
    match fields.iter_mut().find(|(field, _)| field == name) {
        Some((_, current)) => *current = value,
        None => fields.push((name.to_owned(), value)),
    }
}

#[derive(Clone)]
struct Facts {
    firmware_built: String,
    redfish: String,
    bios_released: String,
    lans: Vec<Lan>,
}

// NOTE: These are Redfish's resources with only the fields we read of them.

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

/// What a BMC lets us redirect. We take its KVM and virtual media as its services say, because that is what turning
/// them off changes.
fn remote_access(manager: &Manager, protocols: &Value) -> RemoteAccess {
    let is_enabled = |service: &str| protocols.pointer(&format!("/{service}/ProtocolEnabled")).and_then(Value::as_bool);

    RemoteAccess {
        is_kvm: Some(is_enabled("KVMIP").unwrap_or(manager.GraphicalConsole.ServiceEnabled)),
        is_sol: manager.SerialConsole.ServiceEnabled,
        is_ider: is_enabled("VirtualMedia") == Some(true),
        consent: Consent::None,
        is_consent_fixed: true,
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
struct Oem<Extension> {
    Supermicro: Extension,
}
