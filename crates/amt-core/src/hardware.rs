use serde::Serialize;
use ts_rs::TS;

use crate::{Client, Result};

/// Everything the device knows about its hardware, gathered by the BIOS at boot. Only what's installed: empty memory
/// slots and drive bays never make it into WS-Man. Text the device left out is empty, numbers it left out are `None`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Hardware {
    pub system: System,
    pub motherboard: Motherboard,
    pub bios: Bios,
    pub processors: Vec<Processor>,
    pub memory: Vec<Module>,
    pub disks: Vec<Disk>,
    pub batteries: Vec<Battery>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct System {
    pub model: String,
    pub manufacturer: String,
    pub version: String,
    pub serial_number: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Motherboard {
    pub product: String,
    pub manufacturer: String,
    pub version: String,
    pub serial_number: String,
    pub asset_tag: String,
    pub replaceable: bool,
}

/// `released` is a date, SMBIOS keeps no time.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Bios {
    pub vendor: String,
    pub version: String,
    pub released: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Processor {
    pub model: String,
    pub manufacturer: String,
    pub family: String,
    pub socket: String,
    pub mhz: Option<u32>,
    pub max_mhz: Option<u32>,
    pub status: String,
    pub upgrade_method: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Module {
    pub slot: String,
    pub bytes: Option<u64>,
    pub mhz: Option<u32>,
    pub kind: String,
    pub form_factor: String,
    pub manufacturer: String,
    pub part_number: String,
    pub serial_number: String,
    pub asset_tag: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub model: String,
    pub serial_number: String,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Battery {
    pub name: String,
    pub charge_percent: Option<u8>,
    pub design_capacity_mwh: Option<u32>,
}

impl Client {
    pub async fn hardware(&mut self) -> Result<Hardware> {
        let chassis = self.get("CIM_Chassis").await?;
        let board = self.get("CIM_Card").await?;
        let bios = self.get("CIM_BIOSElement").await?;
        let features = self.enumerate("CIM_BIOSFeature").await?;
        let package = self.enumerate("CIM_ComputerSystemPackage").await?;
        let processors = self.enumerate("CIM_Processor").await?;
        // CIM_Processor only has clock speeds, the rest is on the CIM_Chip tagged with its device ID, and that chip's
        // socket on its location. Enumerating CIM_Chip includes its subclass, the memory modules, we leave those out.
        let chips: Vec<_> =
            self.enumerate("CIM_Chip").await?.into_iter().filter(|chip| chip.class == "CIM_Chip").collect();
        let locations = self.enumerate("CIM_PhysicalElementLocation").await?;
        let memory = self.enumerate("CIM_PhysicalMemory").await?;
        // Disks have their size on CIM_MediaAccessDevice and their name on CIM_PhysicalPackage, in the same order.
        let disks = self.enumerate("CIM_MediaAccessDevice").await?;
        let packages = self.enumerate("CIM_PhysicalPackage").await?;
        let packages = packages.iter().filter(|package| package.class == "CIM_PhysicalPackage");
        let batteries = self.enumerate("CIM_Battery").await?;

        let processors = processors.iter().map(|processor| {
            let tag = processor.get("DeviceID");
            let chip = |name| chips.iter().find(|chip| chip.get("Tag") == tag).map(|chip| chip.text(name));
            // The association's references end in the chip's tag and the location's position.
            let socket = locations.iter().find_map(|location| {
                let chip = location.get("Element")?.lines().last();
                location.get("PhysicalLocation")?.lines().last().filter(|_| chip == tag)
            });
            Processor {
                model: chip("Version").unwrap_or_default(),
                manufacturer: chip("Manufacturer").unwrap_or_default(),
                family: family(processor.get("Family")),
                socket: socket.unwrap_or_default().to_owned(),
                mhz: processor.parse("CurrentClockSpeed"),
                max_mhz: processor.parse("MaxClockSpeed"),
                status: named(CPU_STATUS, processor.get("CPUStatus")),
                upgrade_method: named(UPGRADE_METHOD, processor.get("UpgradeMethod")),
            }
        });
        let memory = memory.iter().enumerate().map(|(i, module)| Module {
            slot: module.get("BankLabel").map_or(format!("Module {}", i + 1), str::to_owned),
            bytes: module.parse("Capacity"),
            mhz: module.parse("ConfiguredMemoryClockSpeed"),
            kind: named(MEMORY_TYPE, module.get("MemoryType")),
            form_factor: named(FORM_FACTOR, module.get("FormFactor")),
            manufacturer: module.text("Manufacturer"),
            part_number: module.text("PartNumber"),
            serial_number: module.text("SerialNumber"),
            // AMT keeps tags unique with a ` (#2)`, the device's own page leaves that out.
            asset_tag: module.text("Tag").split(" (#").next().unwrap_or_default().to_owned(),
        });
        let disks = disks.iter().zip(packages).map(|(disk, package)| Disk {
            model: package.text("Model"),
            serial_number: package.text("SerialNumber"),
            // MaxMediaSize is in kilobytes.
            bytes: disk.parse::<u64>("MaxMediaSize").map(|kilobytes| kilobytes * 1000),
        });
        let batteries = batteries.iter().map(|battery| Battery {
            name: battery.text("DeviceID"),
            charge_percent: battery.parse("EstimatedChargeRemaining"),
            design_capacity_mwh: battery.parse("DesignCapacity"),
        });

        let features = features.iter().flat_map(|feature| feature.all("Characteristics"));
        Ok(Hardware {
            system: System {
                model: chassis.text("Model"),
                manufacturer: chassis.text("Manufacturer"),
                version: chassis.text("Version"),
                serial_number: chassis.text("SerialNumber"),
                id: package.first().map(|package| guid(&package.text("PlatformGUID"))).unwrap_or_default(),
            },
            motherboard: Motherboard {
                product: board.text("Model"),
                manufacturer: board.text("Manufacturer"),
                version: board.text("Version"),
                serial_number: board.text("SerialNumber"),
                asset_tag: board.text("Tag"),
                replaceable: board.parse("CanBeFRUed").unwrap_or_default(),
            },
            bios: Bios {
                vendor: bios.text("Manufacturer"),
                version: bios.text("Version"),
                released: bios.text("ReleaseDate").split('T').next().unwrap_or_default().to_owned(),
                features: features.map(|code| named(BIOS_FEATURES, Some(code))).collect(),
            },
            processors: processors.collect(),
            memory: memory.collect(),
            disks: disks.collect(),
            batteries: batteries.collect(),
        })
    }
}

/// DMTF value maps, as MeshCommander shows them (Apache-2.0) and extended to what AMT 19 reports.
const CPU_STATUS: &[&str] =
    &["Unknown", "Enabled", "Disabled by user", "Disabled by BIOS (POST error)", "Idle", "Other"];
const MEMORY_TYPE: &[&str] = &[
    "Unknown",
    "Other",
    "DRAM",
    "Synchronous DRAM",
    "Cache DRAM",
    "EDO",
    "EDRAM",
    "VRAM",
    "SRAM",
    "RAM",
    "ROM",
    "Flash",
    "EEPROM",
    "FEPROM",
    "EPROM",
    "CDRAM",
    "3DRAM",
    "SDRAM",
    "SGRAM",
    "RDRAM",
    "DDR",
    "DDR2",
    "BRAM",
    "FB-DIMM",
    "DDR3",
    "FBD2",
    "DDR4",
    "LPDDR",
    "LPDDR2",
    "LPDDR3",
    "LPDDR4",
    "Logical non-volatile device",
    "HBM",
    "HBM2",
    "DDR5",
    "LPDDR5",
];
const FORM_FACTOR: &[&str] = &[
    "",
    "Other",
    "Unknown",
    "SIMM",
    "SIP",
    "Chip",
    "DIP",
    "ZIP",
    "Proprietary card",
    "DIMM",
    "TSOP",
    "Row of chips",
    "RIMM",
    "SODIMM",
    "SRIMM",
    "FB-DIMM",
];
const BIOS_FEATURES: &[&str] = &[
    "",
    "Other",
    "Unknown",
    "Undefined",
    "ISA",
    "MCA",
    "EISA",
    "PCI",
    "PCMCIA",
    "Plug and Play",
    "APM",
    "Upgradeable",
    "Shadowing is allowed",
    "VL-VESA",
    "ESCD",
    "Boot from LS-120",
    "ACPI",
    "Boot from I2O",
    "USB legacy",
    "AGP",
    "PC Card",
    "IR",
    "1394",
    "I2C",
    "Smart battery",
    "Boot from ATAPI ZIP",
    "Boot from 1394",
    "Boot from CD",
    "Selectable boot",
    "ROM socketed",
    "Boot from PCMCIA",
    "EDD spec",
];

/// SMBIOS processor upgrade names (DSP0134 7.5.5).
const UPGRADE_METHOD: &[&str] = &[
    "",
    "Other",
    "Unknown",
    "Daughter board",
    "ZIF socket",
    "Replaceable piggy back",
    "None",
    "LIF socket",
    "Slot 1",
    "Slot 2",
    "370-pin socket",
    "Slot A",
    "Slot M",
    "Socket 423",
    "Socket A (Socket 462)",
    "Socket 478",
    "Socket 754",
    "Socket 940",
    "Socket 939",
    "Socket mPGA604",
    "Socket LGA771",
    "Socket LGA775",
    "Socket S1",
    "Socket AM2",
    "Socket F (1207)",
    "Socket LGA1366",
    "Socket G34",
    "Socket AM3",
    "Socket C32",
    "Socket LGA1156",
    "Socket LGA1567",
    "Socket PGA988A",
    "Socket BGA1288",
    "Socket rPGA988B",
    "Socket BGA1023",
    "Socket BGA1224",
    "Socket BGA1155",
    "Socket LGA1356",
    "Socket LGA2011",
    "Socket FS1",
    "Socket FS2",
    "Socket FM1",
    "Socket FM2",
    "Socket LGA2011-3",
    "Socket LGA1356-3",
    "Socket LGA1150",
    "Socket BGA1168",
    "Socket BGA1234",
    "Socket BGA1364",
    "Socket AM4",
    "Socket LGA1151",
    "Socket BGA1356",
    "Socket BGA1440",
    "Socket BGA1515",
    "Socket LGA3647-1",
    "Socket SP3",
    "Socket SP3r2",
    "Socket LGA2066",
    "Socket BGA1392",
    "Socket BGA1510",
    "Socket BGA1528",
    "Socket LGA4189",
    "Socket LGA1200",
    "Socket LGA4677",
    "Socket LGA1700",
    "Socket BGA1744",
    "Socket BGA1781",
    "Socket BGA1211",
    "Socket BGA2422",
    "Socket LGA1211",
    "Socket LGA2422",
    "Socket LGA5773",
    "Socket BGA5773",
    "Socket AM5",
    "Socket SP5",
    "Socket SP6",
    "Socket BGA883",
    "Socket BGA1190",
    "Socket BGA4129",
    "Socket LGA4710",
    "Socket LGA7529",
    "Socket BGA1964",
    "Socket BGA1792",
    "Socket BGA2049",
    "Socket BGA2551",
    "Socket LGA1851",
    "Socket BGA2114",
    "Socket BGA2833",
];

/// SMBIOS processor family names (DSP0134 7.5.2), the Intel ones AMT can run on.
fn family(code: Option<&str>) -> String {
    let name = match code.and_then(|code| code.parse::<u16>().ok()) {
        Some(0x0F) => "Celeron",
        Some(0x2B) => "Atom",
        Some(0xB2) => "Pentium 4",
        Some(0xB3) => "Xeon",
        Some(0xBB) => "Pentium D",
        Some(0xBF) => "Core 2 Duo",
        Some(0xC2) => "Core 2 Quad",
        Some(0xC6) => "Core i7",
        Some(0xCD) => "Core i5",
        Some(0xCE) => "Core i3",
        Some(0xCF) => "Core i9",
        Some(0xD0) => "Xeon D",
        Some(0x300) => "Core 3",
        Some(0x301) => "Core 5",
        Some(0x302) => "Core 7",
        Some(0x303) => "Core 9",
        Some(0x304) => "Core Ultra 3",
        Some(0x305) => "Core Ultra 5",
        Some(0x306) => "Core Ultra 7",
        Some(0x307) => "Core Ultra 9",
        _ => return code.unwrap_or_default().to_owned(),
    };
    name.to_owned()
}

/// SMBIOS keeps a GUID's first three fields little-endian, so the hex AMT gives us reads back to front there.
pub(crate) fn guid(hex: &str) -> String {
    if hex.len() != 32 {
        return hex.to_owned();
    }
    let swapped = |range: std::ops::Range<usize>| -> String {
        hex[range].as_bytes().chunks(2).rev().flat_map(|pair| pair.iter().map(|&byte| byte as char)).collect()
    };
    format!("{}-{}-{}-{}-{}", swapped(0..8), swapped(8..12), swapped(12..16), &hex[16..20], &hex[20..]).to_lowercase()
}

fn named(table: &[&str], code: Option<&str>) -> String {
    let name = code.and_then(|code| code.parse::<usize>().ok()).and_then(|code| table.get(code));
    name.map_or(code.unwrap_or_default().to_owned(), |name| (*name).to_owned())
}
