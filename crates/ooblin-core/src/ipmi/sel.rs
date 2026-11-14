//! A BMC's system event log (SEL), worded: IPMI 2.0 chapters 31 and 32, and 42 for what events mean.

use std::collections::HashSet;

use super::sdr::{Record, meaning};
use super::{STORAGE, Session};
use crate::{Error, Event, Result, Severity};

/// The log, newest first, each sensor named by its record in `records` where it has one.
pub async fn events(session: &Session, records: &[Record]) -> Result<Vec<Event>> {
    let (mut events, mut seen, mut id) = (Vec::new(), HashSet::new(), 0u16);
    while id != 0xFFFF && seen.insert(id) {
        let [low, high] = id.to_le_bytes();
        // Whole entries need no reservation.
        let answer = match session.request(STORAGE, 0x43, &[0, 0, low, high, 0, 0xFF]).await {
            Err(Error::Refused(0xCB)) if id == 0 => break,
            answer => answer?,
        };
        let entry = answer.get(2..18).ok_or_else(|| Error::Protocol("a short SEL entry".into()))?;
        events.push(event(entry, records));
        id = u16::from_le_bytes([answer[0], answer[1]]);
    }
    events.reverse();
    Ok(events)
}

fn event(entry: &[u8], records: &[Record]) -> Event {
    let time = |at: usize| match u32::from_le_bytes(entry[at..at + 4].try_into().unwrap()) {
        // Unspecified, or seconds since the BMC started rather than a date.
        u32::MAX | 0..=0x2000_0000 => 0,
        time => time,
    };
    let kind = entry[2];
    if kind != 0x02 {
        // OEM: 0xC0 to 0xDF with a timestamp and the manufacturer, the rest only data.
        let stamped = (0xC0..0xE0).contains(&kind);
        return Event {
            time: if stamped { time(3) } else { 0 },
            source: "OEM".into(),
            description: format!("OEM record {kind:02X}h, data {}", hex(&entry[if stamped { 7 } else { 3 }..])),
            severity: Severity::Info,
        };
    }
    let (sensor_type, number, direction, data) = (entry[10], entry[11], entry[12], &entry[13..16]);
    let (event, deasserted, offset) = (direction & 0x7F, direction & 0x80 != 0, data[0] & 0x0F);
    let record = records.iter().find(|record| record.number() == number && record.owner() == entry[7]);
    let source = record.map_or_else(|| sensor_type_name(sensor_type), Record::name);
    let Some((text, severity)) = state(sensor_type, direction, data[0]) else {
        let oem = sensor_type >= 0xC0 || (0x70..0x80).contains(&event);
        let description = format!("{} event, data {}", if oem { "OEM" } else { "Unknown" }, hex(&entry[12..]));
        return Event { time: time(3), source, description, severity: Severity::Info };
    };
    let mut description = text.to_string();
    // System firmware says what it was at in data 2.
    if sensor_type == 0x0F && event == 0x6F && data[0] & 0xC0 == 0xC0 {
        let details = if offset == 0 { FIRMWARE_ERRORS } else { FIRMWARE_PROGRESS };
        if let Some(detail) = details.get(usize::from(data[1])) {
            description = format!("{description}: {detail}");
        }
    }
    if deasserted {
        description += " deasserted";
    }
    // A threshold's event carries the reading (data 2) and the threshold crossed (data 3).
    if event == 0x01
        && data[0] & 0xF0 == 0x50
        && let Some(record) = record
        && let (Some(reading), Some(threshold)) = (record.value(data[1]), record.value(data[2]))
    {
        // Odd offsets go high.
        let comparison = match (offset & 1 != 0, deasserted) {
            (true, false) => "≥",
            (false, false) => "≤",
            (true, true) => "<",
            (false, true) => ">",
        };
        let unit = record.unit();
        description = format!("{description}, {} {comparison} {}", amount(reading, &unit), amount(threshold, &unit));
    }
    Event { time: time(3), source, description, severity }
}

/// What an event of a sensor of `sensor_type` says, by its event direction and type and its event data 1, and how bad
/// it is.
pub(crate) fn state(sensor_type: u8, direction: u8, data: u8) -> Option<(&'static str, Severity)> {
    let (text, asserting, deasserting) = meaning(sensor_type, direction & 0x7F, data & 0x0F)?;
    Some((text, if direction & 0x80 != 0 { deasserting } else { asserting }))
}

fn amount(value: f64, unit: &str) -> String {
    format!("{value} {unit}").trim_end().to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" ")
}

fn sensor_type_name(code: u8) -> String {
    match SENSOR_TYPES.get(usize::from(code)) {
        Some(name) if code != 0 => name.to_string(),
        _ if code >= 0xC0 => "OEM".into(),
        _ => format!("Sensor type {code:02X}h"),
    }
}

/// ipmitool's names of the sensor types, by their IPMI code.
const SENSOR_TYPES: [&str; 45] = [
    "reserved",
    "Temperature",
    "Voltage",
    "Current",
    "Fan",
    "Physical Security",
    "Platform Security",
    "Processor",
    "Power Supply",
    "Power Unit",
    "Cooling Device",
    "Other",
    "Memory",
    "Drive Slot / Bay",
    "POST Memory Resize",
    "System Firmware",
    "Event Logging Disabled",
    "Watchdog1",
    "System Event",
    "Critical Interrupt",
    "Button",
    "Module / Board",
    "Microcontroller",
    "Add-in Card",
    "Chassis",
    "Chip Set",
    "Other FRU",
    "Cable / Interconnect",
    "Terminator",
    "System Boot Initiated",
    "Boot Error",
    "OS Boot",
    "OS Critical Stop",
    "Slot / Connector",
    "System ACPI Power State",
    "Watchdog2",
    "Platform Alert",
    "Entity Presence",
    "Monitor ASIC",
    "LAN",
    "Management Subsys Health",
    "Battery",
    "Session Audit",
    "Version Change",
    "FRU State",
];

/// What a system firmware error is, by its code in data 2.
const FIRMWARE_ERRORS: &[&str] = &[
    "Unspecified",
    "No system memory installed",
    "No usable system memory",
    "Unrecoverable IDE device failure",
    "Unrecoverable system-board failure",
    "Unrecoverable diskette failure",
    "Unrecoverable hard-disk controller failure",
    "Unrecoverable PS/2 or USB keyboard failure",
    "Removable boot media not found",
    "Unrecoverable video controller failure",
    "No video device selected",
    "BIOS corruption detected",
    "CPU voltage mismatch",
    "CPU speed mismatch failure",
];

/// What system firmware hung at or got to, likewise.
const FIRMWARE_PROGRESS: &[&str] = &[
    "Unspecified",
    "Memory initialization",
    "Hard-disk initialization",
    "Secondary CPU Initialization",
    "User authentication",
    "User-initiated system setup",
    "USB resource configuration",
    "PCI resource configuration",
    "Option ROM initialization",
    "Video initialization",
    "Cache initialization",
    "SMBus initialization",
    "Keyboard controller initialization",
    "Management controller initialization",
    "Docking station attachment",
    "Enabling docking station",
    "Docking station ejection",
    "Disabling docking station",
    "Calling operating system wake-up vector",
    "System boot initiated",
    "Motherboard initialization",
    "reserved",
    "Floppy initialization",
    "Keyboard test",
    "Pointing device test",
    "Primary CPU initialization",
];
