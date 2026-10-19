//! The event log and the audit log. Decoding follows MeshCommander's `amt-0.2.0.js` (Intel Corporation, Apache-2.0),
//! the method inputs are go-wsman-messages' (`pkg/wsman/amt/messagelog`, `auditlog`, Copyright Intel Corporation,
//! Apache-2.0). Times are UTC, as AMT keeps them and MeshCommander shows them.

use base64::prelude::{BASE64_STANDARD, Engine as _};
use instant_xml::ToXml;
use serde::Serialize;
use ts_rs::TS;

use crate::wsman::Method;
use crate::{Client, Error, Result};

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Event {
    /// Unix seconds.
    pub time: u32,
    pub source: String,
    pub description: String,
    pub severity: Severity,
}

/// MeshCommander's three icons for the PET event severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct AuditRecord {
    /// Unix seconds.
    pub time: u32,
    pub application: String,
    pub event: String,
    pub initiator: String,
    pub address: String,
    pub details: String,
}

/// Both logs come newest first: AMT keeps its event log that way round, its audit log the other. It hands them over in
/// a page or two (AMT 19: 235 events, 76 audit records in one).
impl Client {
    pub async fn event_log(&mut self) -> Result<Vec<Event>> {
        let first = match self.query(&PositionToFirstRecord).await {
            // 2 is NoRecordExists.
            Err(Error::Method { code, .. }) if code == "2" => return Ok(Vec::new()),
            first => first?,
        };
        let mut iteration = first.text("IterationIdentifier");
        let mut events = Vec::new();
        loop {
            // 390 is the most AMT returns at once.
            let page = self.query(&GetRecords { IterationIdentifier: &iteration, MaxReadRecords: 390 }).await?;
            let records: Vec<_> = page.all("RecordArray").collect();
            events.extend(records.iter().filter_map(|record| event(&base64(record)?)));
            if page.get("NoMoreRecords") == Some("true") || records.is_empty() {
                return Ok(events);
            }
            iteration = page.text("IterationIdentifier");
        }
    }

    pub async fn audit_log(&mut self) -> Result<Vec<AuditRecord>> {
        let (mut records, mut read) = (Vec::new(), 0);
        loop {
            let page = self.query(&ReadRecords { StartIndex: read + 1 }).await?;
            let page_records: Vec<_> = page.all("EventRecords").collect();
            records.extend(page_records.iter().filter_map(|record| audit_record(&base64(record)?)));
            read += page_records.len();
            let total = page.parse("TotalRecordCount").unwrap_or(0);
            if read >= total || page_records.is_empty() {
                records.reverse();
                return Ok(records);
            }
        }
    }
}

const MESSAGE_LOG: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_MessageLog";
const AUDIT_LOG: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_AuditLog";

#[derive(ToXml)]
#[xml(rename = "PositionToFirstRecord_INPUT", ns(MESSAGE_LOG))]
struct PositionToFirstRecord;

impl Method for PositionToFirstRecord {
    const URI: &'static str = MESSAGE_LOG;
    const NAME: &'static str = "PositionToFirstRecord";
}

#[derive(ToXml)]
#[xml(rename = "GetRecords_INPUT", ns(MESSAGE_LOG))]
struct GetRecords<'a> {
    IterationIdentifier: &'a str,
    MaxReadRecords: u16,
}

impl Method for GetRecords<'_> {
    const URI: &'static str = MESSAGE_LOG;
    const NAME: &'static str = "GetRecords";
}

#[derive(ToXml)]
#[xml(rename = "ReadRecords_INPUT", ns(AUDIT_LOG))]
struct ReadRecords {
    StartIndex: usize,
}

impl Method for ReadRecords {
    const URI: &'static str = AUDIT_LOG;
    const NAME: &'static str = "ReadRecords";
}

fn base64(record: &str) -> Option<Vec<u8>> {
    BASE64_STANDARD.decode(record).ok()
}

/// A 21-byte PET record, little-endian.
fn event(record: &[u8]) -> Option<Event> {
    let record: [u8; 21] = record.get(..21)?.try_into().ok()?;
    let [t0, t1, t2, t3, _, sensor, _, offset, _, severity, _, entity, _, data @ ..] = record;
    let time = u32::from_le_bytes([t0, t1, t2, t3]);
    if time == 0 || time == u32::MAX {
        return None;
    }
    Some(Event {
        time,
        source: ENTITIES.get(usize::from(entity)).copied().unwrap_or("Unknown").to_owned(),
        description: describe(sensor, offset, data),
        severity: match severity {
            16.. => Severity::Critical,
            8.. => Severity::Warning,
            _ => Severity::Info,
        },
    })
}

fn describe(sensor: u8, offset: u8, d: [u8; 8]) -> String {
    let unknown = || format!("Unknown Sensor Type #{sensor}");
    let ocr = d[..2] == [170, 48];
    let erase = d[..2] == [170, 64];
    match sensor {
        15 if d[0] == 235 => "Invalid Data".into(),
        15 if offset == 0 => nth(FIRMWARE_ERRORS, d[1]).into(),
        15 if offset == 3 && ocr => format!("One Click Recovery: {}", nth(OCR_ERRORS, d[2])),
        15 if offset == 3 && erase => match d[2] {
            1 => "Got an error erasing Device SSD".into(),
            2 => "Erasing Device TPM is not supported".into(),
            3 => "Reached Max Counter".into(),
            _ => unknown(),
        },
        15 if offset == 3 => "OEM Specific Firmware Error event".into(),
        15 if offset == 5 && ocr => match d[2] {
            1 => format!("One Click Recovery: CSME Boot Option {}:{} added successfully", d[3], ocr_source(d[3])),
            0..7 => format!("One Click Recovery: {}", nth(OCR_PROGRESS, d[2])),
            _ => format!("One Click Recovery: Unknown progress event {}", d[2]),
        },
        15 if offset == 5 && erase => match (d[2], d[3]) {
            (1, 2) => "Started erasing Device SSD".into(),
            (1, 3) => "Started erasing Device TPM".into(),
            (1, 5) => "Started erasing Device BIOS Reload of Golden Config".into(),
            (2, 2) => "Erasing Device SSD ended successfully".into(),
            (2, 3) => "Erasing Device TPM ended successfully".into(),
            (2, 5) => "Erasing Device BIOS Reload of Golden Config ended successfully".into(),
            (3, _) => "Beginning Platform Erase".into(),
            (4, _) => "Clear Reserved Parameters".into(),
            (5, _) => "All setting decremented".into(),
            _ => unknown(),
        },
        15 if offset == 5 => "OEM Specific Firmware Progress event".into(),
        15 => nth(FIRMWARE_PROGRESS, d[1]).into(),
        18 if d[0] == 170 => format!(
            "Agent watchdog {:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-... changed to {}",
            d[4],
            d[3],
            d[2],
            d[1],
            d[6],
            d[5],
            match d[7] {
                1 => "Not Started",
                2 => "Stopped",
                4 => "Running",
                8 => "Expired",
                16 => "Suspended",
                _ => "Unknown",
            }
        ),
        5 if offset == 0 => "Case intrusion".into(),
        192 if offset == 0 && ocr && d[2] < 4 => [
            "A remote Serial Over LAN session was established.",
            "Remote Serial Over LAN session finished. User control was restored.",
            "A remote IDE-Redirection session was established.",
            "Remote IDE-Redirection session finished. User control was restored.",
        ][usize::from(d[2])]
        .into(),
        36 => {
            let nic = if d[0] == 0xAA { "wired".into() } else { format!("#{}", d[0]) };
            match u32::from_be_bytes([d[1], d[2], d[3], d[4]]) {
                4294967293 => format!("All received packet filter was matched on {nic} interface."),
                4294967292 => format!("All outbound packet filter was matched on {nic} interface."),
                4294967290 => format!("Spoofed packet filter was matched on {nic} interface."),
                handle => format!("Filter {handle} was matched on {nic} interface."),
            }
        }
        192 => match d[2] {
            0 => "Security policy invoked. Some or all network traffic (TX) was stopped.".into(),
            2 => "Security policy invoked. Some or all network traffic (RX) was stopped.".into(),
            _ => "Security policy invoked.".into(),
        },
        193 => match d[..4] {
            [0xAA, 0x30, 0x00, 0x00] => "User request for remote connection.".into(),
            [0xAA, 0x20, 0x03, 0x01] => "EAC error: attempt to get posture while NAC in Intel® AMT is disabled.".into(),
            [0xAA, 0x20, 0x04, 0x00] => "HWA Error: general error".into(),
            _ => unknown(),
        },
        6 => {
            format!("Authentication failed {} times. The system may be under attack.", u16::from_le_bytes([d[1], d[2]]))
        }
        30 => "No bootable media".into(),
        32 => "Operating system lockup or power interrupt".into(),
        35 if d[0] == 64 => "BIOS POST (Power On Self-Test) Watchdog Timeout.".into(),
        35 => "System boot failure".into(),
        37 => "System firmware started (at least one CPU is properly executing).".into(),
        _ => unknown(),
    }
}

fn nth(table: &[&'static str], index: u8) -> &'static str {
    table.get(usize::from(index)).copied().unwrap_or("Unknown")
}

fn ocr_source(source: u8) -> &'static str {
    match source {
        2 => "HTTPS",
        4 => "Local PBA",
        8 => "WinRE",
        _ => "",
    }
}

/// Big-endian: app and event ID, the initiator, time, network address and extended data, the last three length-prefixed.
fn audit_record(record: &[u8]) -> Option<AuditRecord> {
    let (&[a0, a1, e0, e1, kind], mut rest) = record.split_first_chunk::<5>()?;
    let (application, event) = (u16::from_be_bytes([a0, a1]), u16::from_be_bytes([e0, e1]));
    let id = u32::from(application) * 100 + u32::from(event);
    let initiator = match kind {
        0 => text(prefixed(&mut rest)?),
        1 => {
            // We skip the 4-byte "user in domain" flag.
            rest = rest.get(4..)?;
            sid(prefixed(&mut rest)?)?
        }
        2 => "Local".into(),
        3 => "KVM Default Port".into(),
        _ => return None,
    };
    let (&time, rest) = rest.split_first_chunk::<4>()?;
    // We skip the location type byte.
    let mut rest = rest.get(1..)?;
    let address = text(prefixed(&mut rest)?).replace("0000:0000:0000:0000:0000:0000:0000:0001", "::1");
    let extended = prefixed(&mut rest)?;
    Some(AuditRecord {
        time: u32::from_be_bytes(time),
        application: audit_name(application.into()).map_or_else(|| format!("#{application}"), str::to_owned),
        event: audit_name(id).map_or_else(|| format!("#{event}"), str::to_owned),
        initiator,
        address,
        details: details(id, extended).unwrap_or_default(),
    })
}

/// Takes a byte length and that many bytes off the front.
fn prefixed<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (&len, rest) = bytes.split_first()?;
    let (value, rest) = rest.split_at_checked(len.into())?;
    *bytes = rest;
    Some(value)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A Windows SID in its `S-1-5-21-…` form.
fn sid(bytes: &[u8]) -> Option<String> {
    let (revision, authority) = (bytes.first()?, bytes.get(7)?);
    let subs = bytes[8..].as_chunks().0.iter().map(|&sub| format!("-{}", u32::from_le_bytes(sub)));
    Some(format!("S-{revision}-{authority}{}", subs.collect::<String>()))
}

/// MeshCommander only decodes some events' extended data.
fn details(id: u32, data: &[u8]) -> Option<String> {
    let byte = |i: usize| data.get(i).map(|&b| usize::from(b));
    const AUTH: [&str; 3] = ["NoAuth", "ServerAuth", "MutualAuth"];
    const OPT_IN: [&str; 3] = ["None", "KVM", "All"];
    let pick = |table: &[&str], i: usize| table.get(byte(i)?).map(|value| value.to_string());
    match id {
        1602 | 1604 if byte(0)? == 0 => Some(text(data.get(2..)?.get(..byte(1)?)?)),
        1603 if byte(1)? == 0 => Some(text(data.get(3..)?)),
        1605 => pick(&["Invalid ME access", "Invalid MEBx access"], 0),
        1606 => {
            let state = pick(&["Disabled", "Enabled"], 0)?;
            Some(if byte(1)? == 0 { format!("{state}, {}", text(data.get(3..)?)) } else { state })
        }
        1607 => Some(format!("Remote {}, Local {}", pick(&AUTH, 0)?, pick(&AUTH, 1)?)),
        1617 => {
            let realm = u32::from_be_bytes(*data.first_chunk::<4>()?);
            Some(format!("{}, {}", REALMS.get(realm as usize)?, pick(&["NoAuth", "Auth", "Disabled"], 4)?))
        }
        1619 => pick(&["BIOS", "MEBx", "Local MEI", "Local WSMAN", "Remote WSAMN"], 0),
        1900 => {
            let v: Vec<_> = data.get(..16)?.as_chunks().0.iter().map(|&v| u16::from_be_bytes(v)).collect();
            Some(format!("From {}.{}.{}.{} to {}.{}.{}.{}", v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]))
        }
        2100 => {
            Some(jiff::Timestamp::from_second(u32::from_be_bytes(*data.first_chunk::<4>()?).into()).ok()?.to_string())
        }
        3000 => Some(format!("From {} to {}", pick(&OPT_IN, 0)?, pick(&OPT_IN, 1)?)),
        3001 => pick(&["Success", "Failed 3 times"], 0),
        _ => None,
    }
}

const ENTITIES: &[&str] = &[
    "Unspecified",
    "Other",
    "Unknown",
    "Processor",
    "Disk",
    "Peripheral",
    "System management module",
    "System board",
    "Memory module",
    "Processor module",
    "Power supply",
    "Add in card",
    "Front panel board",
    "Back panel board",
    "Power system board",
    "Drive backplane",
    "System internal expansion board",
    "Other system board",
    "Processor board",
    "Power unit",
    "Power module",
    "Power management board",
    "Chassis back panel board",
    "System chassis",
    "Sub chassis",
    "Other chassis board",
    "Disk drive bay",
    "Peripheral bay",
    "Device bay",
    "Fan cooling",
    "Cooling unit",
    "Cable interconnect",
    "Memory device",
    "System management software",
    "BIOS",
    "Intel® ME",
    "System bus",
    "Group",
    "Intel® ME",
    "External environment",
    "Battery",
    "Processing blade",
    "Connectivity switch",
    "Processor/memory module",
    "I/O module",
    "Processor I/O module",
    "Management controller firmware",
    "IPMI channel",
    "PCI bus",
    "PCI express bus",
    "SCSI bus",
    "SATA/SAS bus",
    "Processor front side bus",
];

const FIRMWARE_ERRORS: &[&str] = &[
    "Unspecified.",
    "No system memory is physically installed in the system.",
    "No usable system memory, all installed memory has experienced an unrecoverable failure.",
    "Unrecoverable hard-disk/ATAPI/IDE device failure.",
    "Unrecoverable system-board failure.",
    "Unrecoverable diskette subsystem failure.",
    "Unrecoverable hard-disk controller failure.",
    "Unrecoverable PS/2 or USB keyboard failure.",
    "Removable boot media not found.",
    "Unrecoverable video controller failure.",
    "No video device detected.",
    "Firmware (BIOS) ROM corruption detected.",
    "CPU voltage mismatch (processors that share same supply have mismatched voltage requirements)",
    "CPU speed matching failure",
];

const FIRMWARE_PROGRESS: &[&str] = &[
    "Unspecified.",
    "Memory initialization.",
    "Starting hard-disk initialization and test",
    "Secondary processor(s) initialization",
    "User authentication",
    "Entering BIOS setup",
    "USB resource configuration",
    "PCI resource configuration",
    "Option ROM initialization",
    "Video initialization",
    "Cache initialization",
    "SM Bus initialization",
    "Keyboard controller initialization",
    "Embedded controller/management controller initialization",
    "Docking station attachment",
    "Enabling docking station",
    "Docking station ejection",
    "Disabling docking station",
    "Calling operating system wake-up vector",
    "Starting operating system boot process",
    "Baseboard or motherboard initialization",
    "reserved",
    "Floppy initialization",
    "Keyboard test",
    "Pointing device test",
    "Primary processor initialization",
];

const OCR_PROGRESS: &[&str] = &[
    "Boot parameters received from CSME",
    "CSME Boot Option % added successfully",
    "HTTPS URI name resolved",
    "HTTPS connected successfully",
    "HTTPSBoot download is completed",
    "Attempt to boot",
    "Exit boot services",
];

const OCR_ERRORS: &[&str] = &[
    "",
    "No network connection available",
    "Name resolution of URI failed",
    "Connect to URI failed",
    "OEM app not found at local URI",
    "HTTPS TLS Auth failed",
    "HTTPS Digest Auth failed",
    "Verified boot failed (bad image)",
    "HTTPS Boot File not found",
];

/// MeshCommander's names for AMT's realms, by number. Unnamed ones are reserved.
const REALMS: &[&str] = &[
    "",
    "",
    "Redirection",
    "",
    "Hardware Asset",
    "Remote Control",
    "Storage",
    "Event Manager",
    "Storage Admin",
    "Agent Presence Local",
    "Agent Presence Remote",
    "Circuit Breaker",
    "Network Time",
    "General Information",
    "Firmware Update",
    "EIT",
    "LocalUN",
    "Endpoint Access Control",
    "Endpoint Access Control Admin",
    "Event Log Reader",
    "Audit Log",
    "ACL Realm",
    "",
    "",
    "Local System",
];

/// Applications by ID, their events by application ID * 100 + event ID.
fn audit_name(id: u32) -> Option<&'static str> {
    Some(match id {
        16 => "Security Admin",
        17 => "RCO",
        18 => "Redirection Manager",
        19 => "Firmware Update Manager",
        20 => "Security Audit Log",
        21 => "Network Time",
        22 => "Network Administration",
        23 => "Storage Administration",
        24 => "Event Manager",
        25 => "Circuit Breaker Manager",
        26 => "Agent Presence Manager",
        27 => "Wireless Configuration",
        28 => "EAC",
        29 => "KVM",
        30 => "User Opt-In Events",
        32 => "Screen Blanking",
        33 => "Watchdog Events",
        1600 => "Provisioning Started",
        1601 => "Provisioning Completed",
        1602 => "ACL Entry Added",
        1603 => "ACL Entry Modified",
        1604 => "ACL Entry Removed",
        1605 => "ACL Access with Invalid Credentials",
        1606 => "ACL Entry State",
        1607 => "TLS State Changed",
        1608 => "TLS Server Certificate Set",
        1609 => "TLS Server Certificate Remove",
        1610 => "TLS Trusted Root Certificate Added",
        1611 => "TLS Trusted Root Certificate Removed",
        1612 => "TLS Preshared Key Set",
        1613 => "Kerberos Settings Modified",
        1614 => "Kerberos Main Key Modified",
        1615 => "Flash Wear out Counters Reset",
        1616 => "Power Package Modified",
        1617 => "Set Realm Authentication Mode",
        1618 => "Upgrade Client to Admin Control Mode",
        1619 => "Unprovisioning Started",
        1700 => "Performed Power Up",
        1701 => "Performed Power Down",
        1702 => "Performed Power Cycle",
        1703 => "Performed Reset",
        1704 => "Set Boot Options",
        1705 => "Remote graceful power down initiated",
        1706 => "Remote graceful reset initiated",
        1707 => "Remote Standby initiated",
        1708 => "Remote Hiberate initiated",
        1709 => "Remote NMI initiated",
        1800 => "IDER Session Opened",
        1801 => "IDER Session Closed",
        1802 => "IDER Enabled",
        1803 => "IDER Disabled",
        1804 => "SoL Session Opened",
        1805 => "SoL Session Closed",
        1806 => "SoL Enabled",
        1807 => "SoL Disabled",
        1808 => "KVM Session Started",
        1809 => "KVM Session Ended",
        1810 => "KVM Enabled",
        1811 => "KVM Disabled",
        1812 => "VNC Password Failed 3 Times",
        1900 => "Firmware Updated",
        1901 => "Firmware Update Failed",
        2000 => "Security Audit Log Cleared",
        2001 => "Security Audit Policy Modified",
        2002 => "Security Audit Log Disabled",
        2003 => "Security Audit Log Enabled",
        2004 => "Security Audit Log Exported",
        2005 => "Security Audit Log Recovered",
        2100 => "Intel® ME Time Set",
        2200 => "TCPIP Parameters Set",
        2201 => "Host Name Set",
        2202 => "Domain Name Set",
        2203 => "VLAN Parameters Set",
        2204 => "Link Policy Set",
        2205 => "IPv6 Parameters Set",
        2300 => "Global Storage Attributes Set",
        2301 => "Storage EACL Modified",
        2302 => "Storage FPACL Modified",
        2303 => "Storage Write Operation",
        2400 => "Alert Subscribed",
        2401 => "Alert Unsubscribed",
        2402 => "Event Log Cleared",
        2403 => "Event Log Frozen",
        2500 => "CB Filter Added",
        2501 => "CB Filter Removed",
        2502 => "CB Policy Added",
        2503 => "CB Policy Removed",
        2504 => "CB Default Policy Set",
        2505 => "CB Heuristics Option Set",
        2506 => "CB Heuristics State Cleared",
        2600 => "Agent Watchdog Added",
        2601 => "Agent Watchdog Removed",
        2602 => "Agent Watchdog Action Set",
        2700 => "Wireless Profile Added",
        2701 => "Wireless Profile Removed",
        2702 => "Wireless Profile Updated",
        2703 => "An existing profile sync was modified",
        2704 => "An existing profile link preference was changed",
        2705 => "Wireless profile share with UEFI enabled setting was changed",
        2800 => "EAC Posture Signer SET",
        2801 => "EAC Enabled",
        2802 => "EAC Disabled",
        2803 => "EAC Posture State",
        2804 => "EAC Set Options",
        2900 => "KVM Opt-in Enabled",
        2901 => "KVM Opt-in Disabled",
        2902 => "KVM Password Changed",
        2903 => "KVM Consent Succeeded",
        2904 => "KVM Consent Failed",
        3000 => "Opt-In Policy Change",
        3001 => "Send Consent Code Event",
        3002 => "Start Opt-In Blocked Event",
        3301 => "A user has modified the Watchdog Action settings",
        3302 => "A user has modified a Watchdog to add, remove, or alter the Watchdog Action connected to it",
        _ => return None,
    })
}
