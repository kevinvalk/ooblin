//! Any BMC over Redfish (DSP0266), as far as the standard goes: the first server, BMC and chassis its service lists,
//! power, hardware, the event log, sensors and the identify light. No screen, serial console or disk image, which
//! every vendor does its own way: a vendor's flavor is this and what the vendor adds, as `supermicro` is.

#![allow(non_snake_case, reason = "our JSON types name their fields the way Redfish does, `PowerState` say")]

pub use crate::net::redfish::{Connection, probe};

use core::net::{Ipv4Addr, Ipv6Addr};
use std::collections::HashSet;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use ts_rs::TS;

use crate::mac;
use crate::{
    Bios, Configuration, Consent, Disk, Error, Event, Flavor, Hardware, Info, Ipv4, Ipv6, Module, Motherboard, Panel,
    Port, Power, PowerAction, PowerState, Processor, RemoteAccess, Result, Sensor, SensorKind, Severity, System,
    Target,
};

pub struct Client {
    pub(crate) connection: Connection,
    /// Where its server, BMC and chassis are, which a service says once.
    paths: OnceCell<Paths>,
}

/// The resources we read of a service: a server, the BMC that manages it and the chassis it's in.
pub(crate) struct Paths {
    pub system: String,
    pub manager: String,
    pub chassis: String,
}

/// What the server and its BMC are: Redfish's version, the BIOS's, the server's own network ports, and what a vendor's
/// own pages add where it has them: the day its firmware (in `info`) was built and the BIOS released, as `2026-05-29`.
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

/// The most pages of a log we follow: a service that keeps naming a next one would have us read forever.
const PAGES: usize = 100;

impl Client {
    /// Connects lazily, as `amt::Client::new` does.
    pub fn new(target: &Target, pin: Option<String>) -> Result<Self> {
        Ok(Self { connection: Connection::new(target, pin)?, paths: OnceCell::new() })
    }

    /// Like `new`, for a service whose resources a vendor's flavor knows without asking.
    pub(crate) fn at(target: &Target, pin: Option<String>, paths: Paths) -> Result<Self> {
        Ok(Self { connection: Connection::new(target, pin)?, paths: OnceCell::from(paths) })
    }

    /// The first of each the service lists. Its BMCs alone first, so a login tries a wrong password once: some lock
    /// the account after a few.
    pub(crate) async fn paths(&self) -> Result<&Paths> {
        let first = async |collection: &str| {
            let members = self.get::<Collection<Link>>(collection).await?.Members;
            let first = members.into_iter().next().map(|member| member.id);
            first.ok_or_else(|| Error::Protocol(format!("a Redfish service with nothing in {collection}")))
        };
        let read = async || {
            let manager = first("/redfish/v1/Managers").await?;
            let (system, chassis) = tokio::try_join!(first("/redfish/v1/Systems"), first("/redfish/v1/Chassis"))?;
            Ok(Paths { system, manager, chassis })
        };
        self.paths.get_or_try_init(read).await
    }

    pub async fn info(&self) -> Result<Info> {
        Ok(self.manager().await?.0)
    }

    /// What a login says, and the BMC's own port for `status`.
    async fn manager(&self) -> Result<(Info, EthernetInterface)> {
        let manager: Manager = self.get(&self.paths().await?.manager).await?;
        let interface = self.linked::<EthernetInterface>(&manager.EthernetInterfaces).await?;
        let interface = interface.into_iter().next().unwrap_or_default();
        let info = Info {
            flavor: Flavor::Redfish,
            name: if interface.FQDN.is_empty() { interface.HostName.clone() } else { interface.FQDN.clone() },
            version: manager.FirmwareVersion,
            build: String::new(),
            control_mode: String::new(),
            fingerprint: self.connection.fingerprint().expect("a successful call has pinned a certificate"),
            remote_access: RemoteAccess {
                kvm: None,
                sol: false,
                ider: false,
                consent: Consent::None,
                consent_fixed: true,
            },
        };
        Ok((info, interface))
    }

    pub async fn status(&self) -> Result<BmcStatus> {
        let (info, interface) = self.manager().await?;
        let system = self.get::<ComputerSystem>(&self.paths().await?.system);
        let (system, root) = tokio::try_join!(system, self.get::<Root>("/redfish/v1/"))?;
        let lans = self.linked::<EthernetInterface>(&system.EthernetInterfaces).await?;
        Ok(BmcStatus {
            info,
            power: system.state(),
            system_id: system.UUID,
            firmware_built: String::new(),
            redfish: root.RedfishVersion,
            bios: system.BiosVersion,
            bios_released: String::new(),
            lans: lans.into_iter().map(|lan| Lan { name: lan.Name, mac: mac(&lan.MACAddress) }).collect(),
            ports: vec![port(interface)],
        })
    }

    /// What it offers is what its Reset allows (boot once only into a target it allows), on or off.
    pub async fn power(&self) -> Result<Power> {
        use PowerAction::*;
        let system: ComputerSystem = self.get(&self.paths().await?.system).await?;
        let state = system.state();
        let offered: &[_] = match state {
            PowerState::Off => &[On, OnToBios, OnToPxe, OnToImage],
            _ => &[SoftOff, Off, SoftReset, Reset, Cycle, Nmi, ResetToBios, ResetToPxe, ResetToImage],
        };
        // Newer services say what a Reset allows in a resource of its own.
        let resets = match system.Actions.reset {
            ResetAction { types, .. } if !types.is_empty() => types,
            ResetAction { info: Some(info), .. } => {
                let parameters = self.get::<ActionInfo>(&info).await?.Parameters;
                parameters
                    .into_iter()
                    .find(|parameter| parameter.Name == "ResetType")
                    .unwrap_or_default()
                    .AllowableValues
            }
            _ => vec![],
        };
        let targets = &system.Boot.targets;
        let offers = |(reset, target): (&str, Option<&str>)| {
            resets.iter().any(|allowed| allowed == reset)
                && target.is_none_or(|target| targets.iter().any(|t| t == target))
        };
        let actions = offered.iter().copied().filter(|&action| reset(action).is_some_and(offers)).collect();
        Ok(Power { state, actions })
    }

    /// Boot once is a boot override first, then the power change.
    pub async fn set_power(&self, action: PowerAction) -> Result<()> {
        let (reset, target) = reset(action).ok_or(Error::Unsupported)?;
        let system = &self.paths().await?.system;
        if let Some(target) = target {
            let boot = json!({"Boot": {"BootSourceOverrideEnabled": "Once", "BootSourceOverrideTarget": target}});
            self.connection.patch(system, &boot).await?;
        }
        let reset = json!({"ResetType": reset});
        self.connection.post(&format!("{system}/Actions/ComputerSystem.Reset"), &reset).await
    }

    /// The system alone first, so a wrong password is tried once where this is the first we ask.
    pub async fn hardware(&self) -> Result<Hardware> {
        let paths = self.paths().await?;
        let system: ComputerSystem = self.get(&paths.system).await?;
        let (chassis, processors, memory, disks) = tokio::try_join!(
            self.get::<Chassis>(&paths.chassis),
            self.linked::<Cpu>(&system.Processors),
            self.linked::<Dimm>(&system.Memory),
            self.disks(&system),
        )?;
        Ok(Hardware {
            motherboard: Motherboard {
                product: chassis.Model,
                manufacturer: chassis.Manufacturer,
                version: String::new(),
                serial_number: chassis.SerialNumber,
                asset_tag: chassis.AssetTag,
                replaceable: false,
            },
            // Not its FPGAs and GPUs, which Redfish lists among them, nor sockets and slots with nothing in them.
            processors: processors.into_iter().filter(Cpu::central).map(Cpu::processor).collect(),
            memory: memory.into_iter().filter(|dimm| dimm.Status.State != "Absent").map(Dimm::module).collect(),
            disks,
            batteries: vec![],
            bios: system.bios(),
            system: system.system(),
        })
    }

    /// The server's drives, of its storage controllers where it lists them, else as its simple storage says.
    async fn disks(&self, system: &ComputerSystem) -> Result<Vec<Disk>> {
        if system.Storage.is_none() {
            let storage = self.linked::<Storage>(&system.SimpleStorage).await?;
            return Ok(storage.into_iter().flat_map(Storage::disks).collect());
        }
        let mut disks = Vec::new();
        for storage in self.linked::<Storage>(&system.Storage).await? {
            for drive in storage.Drives {
                disks.extend(self.get::<Device>(&drive.id).await?.disk());
            }
        }
        Ok(disks)
    }

    /// The server's logs, newest first, every page of each.
    pub async fn event_log(&self) -> Result<Vec<Event>> {
        let system: ComputerSystem = self.get(&self.paths().await?.system).await?;
        let mut events = Vec::new();
        for log in self.linked::<LogService>(&system.LogServices).await? {
            let (mut page, mut seen) = (log.Entries.map(|entries| entries.id), HashSet::new());
            while let Some(path) = page.filter(|path| seen.len() < PAGES && seen.insert(path.clone())) {
                let entries: Entries = self.get(&path).await?;
                events.extend(entries.Members.into_iter().map(Entry::event));
                page = entries.next;
            }
        }
        events.sort_by_key(|event| std::cmp::Reverse(event.time));
        Ok(events)
    }

    /// The chassis's sensors, each a resource of its own where it lists them, else as its thermal and power say.
    pub async fn sensors(&self) -> Result<Vec<Sensor>> {
        let chassis: Chassis = self.get(&self.paths().await?.chassis).await?;
        let readings = match (chassis.Sensors, chassis.Thermal, chassis.Power) {
            (Some(sensors), ..) => self.members::<Reading>(&sensors.id).await?,
            (None, thermal, power) => {
                let read = async |resource: Option<Link>| match resource {
                    Some(resource) => self.get::<Readings>(&resource.id).await,
                    None => Ok(Readings::default()),
                };
                let (thermal, power) = tokio::try_join!(read(thermal), read(power))?;
                let readings = [thermal.Temperatures, thermal.Fans, power.Voltages, power.PowerControl];
                readings.into_iter().flatten().collect()
            }
        };
        Ok(readings.into_iter().filter_map(Reading::sensor).collect())
    }

    /// The identify light, which Redfish names two ways. It knows no POST code.
    pub async fn panel(&self) -> Result<Panel> {
        let system: ComputerSystem = self.get(&self.paths().await?.system).await?;
        let lit = system.IndicatorLED.map(|light| light != "Off");
        Ok(Panel { identify: system.LocationIndicatorActive.or(lit), post_code: None })
    }

    /// Lights the identify light until turned off, or turns it off, by the name the server has for it.
    pub async fn identify(&self, on: bool) -> Result<()> {
        let system = &self.paths().await?.system;
        let light = match self.get::<ComputerSystem>(system).await? {
            ComputerSystem { LocationIndicatorActive: Some(_), .. } => json!({"LocationIndicatorActive": on}),
            ComputerSystem { IndicatorLED: Some(_), .. } => {
                json!({"IndicatorLED": if on { "Blinking" } else { "Off" }})
            }
            _ => return Err(Error::Unsupported),
        };
        self.connection.patch(system, &light).await
    }

    /// The BMC's own web pages, where everything we don't do can be done.
    pub fn web_pages(&self) -> String {
        format!("{}/", self.connection.url())
    }

    /// The resource at `path`, or its default where this BMC doesn't have it: an OEM one, say.
    pub(crate) async fn optional<T: DeserializeOwned + Default>(&self, path: &str) -> Result<T> {
        match self.get(path).await {
            Err(Error::Http(404)) => Ok(T::default()),
            resource => resource,
        }
    }

    /// Each member of the collection a resource links, none where it doesn't.
    async fn linked<T: DeserializeOwned>(&self, collection: &Option<Link>) -> Result<Vec<T>> {
        match collection {
            Some(collection) => self.members(&collection.id).await,
            None => Ok(vec![]),
        }
    }

    /// Each member of the collection at `path`, one request each: few services expand a collection, and the X10's
    /// `$expand` is broken.
    pub(crate) async fn members<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let collection: Collection<Link> = self.get(path).await?;
        let mut members = Vec::new();
        for member in collection.Members {
            members.push(self.get(&member.id).await?);
        }
        Ok(members)
    }

    /// The resource at `path`, tidied.
    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut value = self.connection.get(path).await?;
        tidy(&mut value);
        T::deserialize(value).map_err(|error| Error::Protocol(format!("{path}: {error}")))
    }
}

/// The `ResetType` that does `action`, and the target to boot once into first, for what Redfish can do. A disk image
/// is a USB CD-ROM drive, where a vendor's flavor puts one in.
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
        Cycle => ("PowerCycle", None),
        ResetToBios => ("ForceRestart", Some("BiosSetup")),
        ResetToPxe => ("ForceRestart", Some("Pxe")),
        ResetToImage => ("ForceRestart", Some("UsbCd")),
        Nmi => ("Nmi", None),
        _ => return None,
    })
}

/// What a BMC leaves out comes as null, blanks (the X10's serial number) or SMBIOS's placeholder: we drop them all, so
/// they read as "" or `None` as AMT's do.
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
/// as its IPv6 gateway, OpenBMC all zeros for none.
pub(crate) fn port(interface: EthernetInterface) -> Port {
    let mut ipv4_dns = interface.NameServers.iter().filter(|server| server.parse::<Ipv4Addr>().is_ok()).cloned();
    let mut ipv6_dns = interface.NameServers.iter().filter(|server| server.parse::<Ipv6Addr>().is_ok()).cloned();
    let ipv4 = interface.IPv4Addresses.into_iter().next().unwrap_or_default();
    let addresses: Vec<_> = interface.IPv6Addresses.into_iter().map(|address| address.Address.to_lowercase()).collect();
    let gateway = Some(interface.IPv6DefaultGateway.to_lowercase()).filter(|gateway| {
        !addresses.contains(gateway) && gateway.parse::<Ipv6Addr>().is_ok_and(|gateway| !gateway.is_unspecified())
    });
    let enabled = interface.Status.State == "Enabled" && interface.InterfaceEnabled;
    Port {
        wireless: false,
        mac: mac(&interface.MACAddress),
        link_up: interface.LinkStatus.map_or(enabled, |link| link == "LinkUp"),
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

// Redfish's resources, only what we read of them.

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Root {
    pub RedfishVersion: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Manager {
    FirmwareVersion: String,
    EthernetInterfaces: Option<Link>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct EthernetInterface {
    pub Name: String,
    pub VLAN: Vlan,
    pub HostName: String,
    pub FQDN: String,
    pub MACAddress: String,
    pub Status: Condition,
    pub InterfaceEnabled: bool,
    pub LinkStatus: Option<String>,
    pub SpeedMbps: Option<u64>,
    pub IPv4Addresses: Vec<Address>,
    pub IPv6Addresses: Vec<Address>,
    pub IPv6DefaultGateway: String,
    pub NameServers: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Vlan {
    pub VLANEnable: bool,
    pub VLANId: u16,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Address {
    pub Address: String,
    pub SubnetMask: String,
    pub AddressOrigin: String,
    pub Gateway: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct ComputerSystem {
    PowerState: String,
    pub UUID: String,
    Model: String,
    Manufacturer: String,
    SerialNumber: String,
    pub BiosVersion: String,
    LocationIndicatorActive: Option<bool>,
    IndicatorLED: Option<String>,
    Boot: Boot,
    Actions: Actions,
    Processors: Option<Link>,
    Memory: Option<Link>,
    Storage: Option<Link>,
    SimpleStorage: Option<Link>,
    EthernetInterfaces: Option<Link>,
    LogServices: Option<Link>,
}

impl ComputerSystem {
    pub fn state(&self) -> PowerState {
        match &*self.PowerState {
            "On" => PowerState::On,
            "Off" => PowerState::Off,
            _ => PowerState::Unknown,
        }
    }

    pub fn bios(&self) -> Bios {
        Bios { vendor: String::new(), version: self.BiosVersion.clone(), released: String::new(), features: vec![] }
    }

    pub fn system(self) -> System {
        System {
            model: self.Model,
            manufacturer: self.Manufacturer,
            version: String::new(),
            serial_number: self.SerialNumber,
            id: self.UUID,
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
    #[serde(rename = "@Redfish.ActionInfo")]
    info: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ActionInfo {
    Parameters: Vec<Parameter>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Parameter {
    Name: String,
    AllowableValues: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Chassis {
    Manufacturer: String,
    Model: String,
    SerialNumber: String,
    AssetTag: String,
    Sensors: Option<Link>,
    Thermal: Option<Link>,
    Power: Option<Link>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Cpu {
    Model: String,
    Manufacturer: String,
    Socket: String,
    MaxSpeedMHz: Option<u32>,
    OperatingSpeedMHz: Option<u32>,
    ProcessorType: String,
    Status: Condition,
}

impl Cpu {
    fn central(&self) -> bool {
        matches!(&*self.ProcessorType, "" | "CPU") && self.Status.State != "Absent"
    }

    pub fn processor(self) -> Processor {
        Processor {
            model: self.Model,
            manufacturer: self.Manufacturer,
            family: String::new(),
            socket: self.Socket,
            mhz: self.OperatingSpeedMHz,
            max_mhz: self.MaxSpeedMHz,
            status: self.Status.State,
            upgrade_method: String::new(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Dimm {
    Name: String,
    DeviceLocator: String,
    CapacityMiB: Option<u64>,
    OperatingSpeedMhz: Option<u32>,
    MemoryDeviceType: String,
    Manufacturer: String,
    PartNumber: String,
    SerialNumber: String,
    Status: Condition,
}

impl Dimm {
    pub fn module(self) -> Module {
        Module {
            slot: if self.DeviceLocator.is_empty() { self.Name } else { self.DeviceLocator },
            bytes: self.CapacityMiB.map(|mib| mib << 20),
            mhz: self.OperatingSpeedMhz,
            kind: self.MemoryDeviceType,
            form_factor: String::new(),
            manufacturer: self.Manufacturer,
            part_number: self.PartNumber,
            serial_number: self.SerialNumber,
            asset_tag: String::new(),
        }
    }
}

/// A storage controller with its drives, or simple storage with its devices.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Storage {
    Drives: Vec<Link>,
    Devices: Vec<Device>,
}

impl Storage {
    pub fn disks(self) -> impl Iterator<Item = Disk> {
        self.Devices.into_iter().filter_map(Device::disk)
    }
}

/// A drive, or a simple storage's device.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Device {
    Model: String,
    SerialNumber: String,
    CapacityBytes: Option<u64>,
    Status: Condition,
}

impl Device {
    /// None for a bay that's empty.
    fn disk(self) -> Option<Disk> {
        let disk = Disk { model: self.Model, serial_number: self.SerialNumber, bytes: self.CapacityBytes };
        (self.Status.State != "Absent").then_some(disk)
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LogService {
    Entries: Option<Link>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Entries {
    Members: Vec<Entry>,
    #[serde(rename = "Members@odata.nextLink", alias = "@odata.nextLink")]
    next: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Entry {
    Created: String,
    Severity: String,
    SensorType: String,
    MessageId: String,
    Message: String,
}

impl Entry {
    fn event(self) -> Event {
        Event {
            time: self.Created.parse::<jiff::Timestamp>().map_or(0, |time| time.as_second() as u32),
            source: if self.SensorType.is_empty() { self.MessageId } else { self.SensorType },
            description: self.Message,
            severity: severity(&self.Severity),
        }
    }
}

/// How bad Redfish says something is: `OK`, `Warning` or `Critical`.
fn severity(health: &str) -> Severity {
    match health {
        "Critical" => Severity::Critical,
        "Warning" => Severity::Warning,
        _ => Severity::Info,
    }
}

/// What a chassis's thermal and power resources read, before it had sensors of their own.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Readings {
    Temperatures: Vec<Reading>,
    Fans: Vec<Reading>,
    Voltages: Vec<Reading>,
    PowerControl: Vec<Reading>,
}

/// A sensor, or one of a thermal or power resource's readings, which name what they read in the field itself.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Reading {
    Name: String,
    Reading: Option<f64>,
    ReadingUnits: String,
    ReadingType: String,
    ReadingCelsius: Option<f64>,
    ReadingVolts: Option<f64>,
    PowerConsumedWatts: Option<f64>,
    LowerThresholdCritical: Option<f64>,
    UpperThresholdCritical: Option<f64>,
    Thresholds: Thresholds,
    Status: Condition,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Thresholds {
    LowerCritical: Threshold,
    UpperCritical: Threshold,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Threshold {
    Reading: Option<f64>,
}

impl Reading {
    /// None for one that isn't there: an empty fan bay, say.
    fn sensor(self) -> Option<Sensor> {
        use SensorKind::*;
        let (reading, kind, unit) = match (self.ReadingCelsius, self.ReadingVolts, self.PowerConsumedWatts) {
            (Some(celsius), ..) => (Some(celsius), Temperature, "°C"),
            (_, Some(volts), _) => (Some(volts), Voltage, "V"),
            (.., Some(watts)) => (Some(watts), Power, "W"),
            _ => match (&*self.ReadingType, &*self.ReadingUnits) {
                ("Temperature", _) | (_, "Cel") => (self.Reading, Temperature, "°C"),
                ("Rotational", _) | (_, "RPM") => (self.Reading, Fan, "RPM"),
                ("Voltage", _) | (_, "V") => (self.Reading, Voltage, "V"),
                ("Current", _) | (_, "A") => (self.Reading, Current, "A"),
                ("Power", _) | (_, "W") => (self.Reading, Power, "W"),
                (_, unit) => (self.Reading, Other, unit),
            },
        };
        let sensor = Sensor {
            name: self.Name,
            kind,
            reading,
            unit: unit.to_owned(),
            state: if reading.is_none() { self.Status.Health.clone() } else { String::new() },
            severity: severity(&self.Status.Health),
            low: self.Thresholds.LowerCritical.Reading.or(self.LowerThresholdCritical),
            high: self.Thresholds.UpperCritical.Reading.or(self.UpperThresholdCritical),
        };
        (self.Status.State != "Absent").then_some(sensor)
    }
}

/// A resource's `Status`.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Condition {
    pub State: String,
    Health: String,
}

#[derive(Deserialize)]
pub(crate) struct Collection<T> {
    pub Members: Vec<T>,
}

#[derive(Deserialize)]
pub(crate) struct Link {
    #[serde(rename = "@odata.id")]
    pub id: String,
}
