use serde::Serialize;

use crate::wsman::Instance;
use crate::{Client, Result};

/// The hardware as the DMT console reads it, minus what we don't show.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub manufacturer: String,
    pub model: String,
    pub serial: String,
    pub board: String,
    pub bios: String,
    pub bios_date: String,
    pub processor: String,
    pub memory: Vec<Memory>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub bank: String,
    pub bytes: u64,
    pub mhz: u64,
    pub part: String,
}

impl Client {
    pub async fn inventory(&mut self) -> Result<Inventory> {
        let chassis = self.get("CIM_Chassis").await?;
        let board = self.get("CIM_Card").await?;
        let bios = self.get("CIM_BIOSElement").await?;
        // CIM_Processor only has clock speeds, the model name lives on CIM_Chip. Only its Get though: AMT 19 enumerates
        // CIM_Chip as the memory modules, which are a subclass of it.
        let chip = self.get("CIM_Chip").await?;
        let dimms = self.enumerate("CIM_PhysicalMemory").await?;

        let number =
            |instance: &Instance, name| instance.get(name).and_then(|value| value.parse().ok()).unwrap_or_default();
        Ok(Inventory {
            manufacturer: chassis.text("Manufacturer"),
            model: chassis.text("Model"),
            serial: chassis.text("SerialNumber"),
            board: format!("{} {}", board.text("Manufacturer"), board.text("Model")),
            bios: bios.text("Version"),
            bios_date: bios.text("ReleaseDate").split('T').next().unwrap_or_default().to_owned(),
            processor: chip.text("Version"),
            memory: dimms
                .iter()
                .map(|dimm| Memory {
                    bank: dimm.text("BankLabel"),
                    bytes: number(dimm, "Capacity"),
                    mhz: number(dimm, "ConfiguredMemoryClockSpeed"),
                    part: dimm.text("PartNumber"),
                })
                .collect(),
        })
    }
}
