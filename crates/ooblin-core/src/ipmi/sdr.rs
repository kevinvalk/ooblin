use std::collections::HashSet;

use super::{SENSOR, STORAGE, Session};
use crate::{Error, Result, Sensor, SensorKind, Severity};
// We keep the severities to one letter so that the tables of states below stay readable.
use Severity::{Critical as C, Info as I, Warning as W};

const BMC_ADDRESS: u8 = 0x20; // The only owner whose sensors we read, as we do not bridge to others

// NOTE: A sensor's record in the BMC's SDR repository and what it reads now are IPMI 2.0 chapters 33, 35 and 43.
#[derive(Debug, Clone)]
pub(super) struct Record(Vec<u8>);

impl Record {
    fn byte(&self, offset: usize) -> u8 {
        self.0.get(offset).copied().unwrap_or(0)
    }

    pub(super) fn owner(&self) -> u8 {
        self.byte(5)
    }

    pub(super) fn number(&self) -> u8 {
        self.byte(7)
    }

    pub(super) fn name(&self) -> String {
        let offset = match self.byte(3) {
            0x01 => 47,
            0x02 => 31,
            _ => 16,
        };
        let length = usize::from(self.byte(offset) & 0x1F);
        let name = self.0.iter().skip(offset + 1).take(length).map(|&byte| char::from(byte)).collect::<String>();

        name.trim_matches(['\0', ' ']).to_string()
    }

    /// Only a full record of a threshold sensor reads a number.
    fn is_analog(&self) -> bool {
        self.byte(3) == 0x01 && self.byte(13) == 0x01 && self.byte(20) >> 6 != 3
    }

    pub(super) fn unit(&self) -> String {
        let unit_name = |code: u8| UNITS.get(usize::from(code)).copied().unwrap_or("invalid");
        let (units, modifier) = (self.byte(20), unit_name(self.byte(22)));
        let unit = format!("{}{}", if units & 1 != 0 { "% " } else { "" }, unit_name(self.byte(21)));
        let unit = match units >> 1 & 0x03 {
            1 => format!("{unit}/{modifier}"),
            2 => format!("{unit}*{modifier}"),
            _ => unit,
        };

        unit.trim_end().to_string()
    }

    /// Converts a raw reading or threshold as y = L((M x + B 10^Bexp) 10^Rexp), rounded to three decimals.
    pub(super) fn value(&self, raw: u8) -> Option<f64> {
        if !self.is_analog() {
            return None;
        }

        let x = f64::from(match self.byte(20) >> 6 {
            0 => i16::from(raw),
            // One's complement: -0 is 0xFF.
            1 => i16::from(raw.wrapping_add(raw >> 7) as i8),
            _ => i16::from(raw as i8),
        });

        let ten_bits = |low: u8, high: u8| f64::from((i16::from(high >> 6) << 8 | i16::from(low)) << 6 >> 6);
        let (m, b) = (ten_bits(self.byte(24), self.byte(25)), ten_bits(self.byte(26), self.byte(27)));
        let exponents = self.byte(29);
        let (r, b_exponent) = (i32::from(exponents as i8 >> 4), i32::from((exponents << 4) as i8 >> 4));

        let y = (m * x + b * 10f64.powi(b_exponent)) * 10f64.powi(r);
        let y = match self.byte(23) & 0x7F {
            0x01 => y.ln(),
            0x02 => y.log10(),
            0x03 => y.log2(),
            0x04 => y.exp(),
            0x05 => 10f64.powf(y),
            0x06 => y.exp2(),
            0x07 => y.recip(),
            0x08 => y * y,
            0x09 => y * y * y,
            0x0A => y.sqrt(),
            0x0B => y.cbrt(),
            // Linear, or non-linear, whose factors we do not ask for (like ipmitool).
            _ => y,
        };

        Some((y * 1000.0).round() / 1000.0).filter(|y| y.is_finite())
    }
}

pub(super) async fn records(session: &Session) -> Result<Vec<Record>> {
    let mut reservation = reserve(session).await?;
    let (mut records, mut seen, mut id) = (Vec::new(), HashSet::new(), 0);

    while id != 0xFFFF && seen.insert(id) {
        let (next, mut record) = match read(session, &mut reservation, id, 0, 5).await {
            // An empty repository has no first record either.
            Err(Error::Refused(0xCB)) if id == 0 => break,
            header => header?,
        };
        let length = 5 + usize::from(*record.get(4).ok_or_else(short)?);

        // We start at 16 bytes, which X10s answer at once. FreeIPMI starts there too and shrinks by 4 when that is too
        // much.
        let mut chunk_length = 16;
        while record.len() < length {
            let count = chunk_length.min(length - record.len()) as u8;
            // Get SDR's offset is a byte, so no record goes on past it.
            let offset = u8::try_from(record.len()).map_err(|_| Error::Protocol("an SDR record too long".into()))?;
            match read(session, &mut reservation, id, offset, count).await {
                Ok((_, bytes)) => record.extend(bytes),
                Err(Error::Refused(0xCA)) if chunk_length > 4 => chunk_length -= 4,
                Err(error) => return Err(error),
            }
        }

        // We keep a full, compact or event-only sensor record that reaches its name, as one cut shorter would read as
        // zeros.
        if matches!((record[3], record.len()), (0x01, 48..) | (0x02, 32..) | (0x03, 17..)) {
            records.extend(shared(record));
        }
        id = next;
    }

    Ok(records)
}

/// A compact or event-only record may stand for several sensors alike, numbered on from its own. We make a record of
/// each, named apart by the number or letter the record says to add (IPMI 2.0 43.2 and 43.3).
fn shared(record: Vec<u8>) -> Vec<Record> {
    let (sharing_offset, name_offset) = match record[3] {
        0x02 => (23, 31),
        0x03 => (12, 16),
        _ => return vec![Record(record)],
    };
    let sharing = record[sharing_offset];
    let (count, is_lettered, first) = (sharing & 0x0F, sharing & 0xC0 == 0x40, record[sharing_offset + 1] & 0x7F);
    let letter = |instance: u8| char::from(b'A' + instance % 26);

    let sensors = (0..count.max(1)).map(|sensor| {
        let suffix = match first + sensor {
            _ if count < 2 => String::new(),
            instance if !is_lettered => instance.to_string(),
            instance @ 0..26 => letter(instance).to_string(),
            instance => format!("{}{}", letter(instance / 26 - 1), letter(instance)),
        };
        let length = usize::from(record[name_offset] & 0x1F);
        let end = (name_offset + 1 + length).min(record.len());

        let mut record = record.clone();
        record[7] = record[7].wrapping_add(sensor);
        record.splice(end..end, suffix.bytes());
        record[name_offset] = record[name_offset] & 0xE0 | (length + suffix.len()).min(0x1F) as u8;
        Record(record)
    });

    sensors.collect()
}

async fn reserve(session: &Session) -> Result<[u8; 2]> {
    let answer = session.request(STORAGE, 0x22, &[]).await?;

    answer.get(..2).and_then(|id| id.try_into().ok()).ok_or_else(short)
}

/// Reads part of record `id`, which gives the next record's ID and the bytes. We reserve the repository again when
/// someone else did meanwhile.
async fn read(session: &Session, reservation: &mut [u8; 2], id: u16, offset: u8, count: u8) -> Result<(u16, Vec<u8>)> {
    let [low, high] = id.to_le_bytes();

    for _ in 0..4 {
        match session.request(STORAGE, 0x23, &[reservation[0], reservation[1], low, high, offset, count]).await {
            Err(Error::Refused(0xC5)) => *reservation = reserve(session).await?,
            Ok(answer) if answer.len() > 2 => {
                return Ok((u16::from_le_bytes([answer[0], answer[1]]), answer[2..].to_vec()));
            }
            Ok(_) => return Err(short()),
            Err(error) => return Err(error),
        }
    }

    Err(Error::Refused(0xC5))
}

fn short() -> Error {
    Error::Protocol("a short SDR answer".into())
}

/// Reads every sensor the BMC owns. A sensor it cannot read reads `None` instead of failing the rest.
pub(super) async fn sensors(session: &Session, records: &[Record]) -> Result<Vec<Sensor>> {
    let mut sensors = Vec::new();

    for record in records.iter().filter(|record| record.byte(3) != 0x03) {
        let (sensor_type, event_type) = (record.byte(12), record.byte(13));
        let kind = match (sensor_type, record.byte(21)) {
            (0x01, _) | (_, 1..=3) => SensorKind::Temperature,
            (0x04, _) | (_, 18) => SensorKind::Fan,
            (_, 4) => SensorKind::Voltage,
            (_, 5) => SensorKind::Current,
            (_, 6) => SensorKind::Power,
            _ => SensorKind::Other,
        };
        let unit = if record.is_analog() { record.unit() } else { String::new() };
        let (name, state) = (record.name(), String::new());
        let mut sensor = Sensor { name, kind, reading: None, unit, state, severity: I, low: None, high: None };

        if record.owner() == BMC_ADDRESS && record.byte(6) & 0x03 == 0 {
            let reading = ask(session, 0x2D, record.number()).await?;
            // We only take a reading where scanning is enabled and the reading is available.
            if reading.get(1).is_some_and(|flags| flags & 0x60 == 0x40) {
                let byte = |index: usize| reading.get(index).copied().unwrap_or(0);
                let states = u16::from_le_bytes([byte(2), byte(3) & 0x7F]);
                if event_type == 0x01 {
                    sensor.reading = record.value(reading[0]);
                    // Non-recoverable or critical, else non-critical, low or high.
                    sensor.severity = match states {
                        _ if states & 0x36 != 0 => C,
                        _ if states & 0x09 != 0 => W,
                        _ => I,
                    };
                } else {
                    let (states, severities): (Vec<_>, Vec<_>) = (0..15)
                        .filter(|offset| states >> offset & 1 != 0)
                        .map(|offset| match meaning(sensor_type, event_type, offset) {
                            Some((text, severity, _)) => (text.to_string(), severity),
                            None => (format!("State {offset}"), I),
                        })
                        .unzip();
                    // Nothing asserted is all well.
                    sensor.state = if states.is_empty() { "OK".into() } else { states.join(", ") };
                    sensor.severity = severities.into_iter().max().unwrap_or(I);
                }
            }

            // We only ask for the critical thresholds where the record says they can be read.
            if record.is_analog() && record.byte(18) & 0x12 != 0 {
                let thresholds = ask(session, 0x27, record.number()).await?;
                let threshold = |mask: u8, index: usize| {
                    thresholds.get(index).filter(|_| thresholds[0] & mask != 0).and_then(|&raw| record.value(raw))
                };
                (sensor.low, sensor.high) = (threshold(0x02, 2), threshold(0x10, 5));
            }
        }

        sensors.push(sensor);
    }

    Ok(sensors)
}

/// Asks about sensor `number`. A refusal (of an absent sensor, say) answers nothing instead of failing.
async fn ask(session: &Session, command: u8, number: u8) -> Result<Vec<u8>> {
    match session.request(SENSOR, command, &[number]).await {
        Err(Error::Refused(_)) => Ok(Vec::new()),
        answer => answer,
    }
}

/// What state `offset` of event/reading type `event_type` means for a sensor of type `sensor_type`, and how bad
/// asserting and deasserting it are.
pub(super) fn meaning(sensor_type: u8, event_type: u8, offset: u8) -> Option<(&'static str, Severity, Severity)> {
    let (table, key) = if event_type == 0x6F { (SPECIFIC, sensor_type) } else { (GENERIC, event_type) };
    let states = table.iter().find(|(code, _)| *code == key)?.1;

    states.get(usize::from(offset)).copied().filter(|(text, ..)| !text.is_empty())
}

type States = &'static [(&'static str, Severity, Severity)];

// The states of the generic event/reading types by offset. We word them as ipmitool does, with how bad asserting and
// deasserting each is as FreeIPMI rates them. Type 0x01 is a threshold sensor's.
const GENERIC: &[(u8, States)] = &[
    (
        0x01,
        &[
            ("Lower Non-critical going low", W, I),
            ("Lower Non-critical going high", I, I),
            ("Lower Critical going low", C, W),
            ("Lower Critical going high", W, W),
            ("Lower Non-recoverable going low", C, C),
            ("Lower Non-recoverable going high", C, C),
            ("Upper Non-critical going low", I, I),
            ("Upper Non-critical going high", W, I),
            ("Upper Critical going low", W, W),
            ("Upper Critical going high", C, W),
            ("Upper Non-recoverable going low", C, C),
            ("Upper Non-recoverable going high", C, C),
        ],
    ),
    (0x02, &[("Transition to Idle", I, I), ("Transition to Active", I, I), ("Transition to Busy", I, I)]),
    (0x03, &[("State Deasserted", I, I), ("State Asserted", I, I)]),
    (0x04, &[("Predictive Failure Deasserted", I, I), ("Predictive Failure Asserted", C, C)]),
    (0x05, &[("Limit Not Exceeded", I, I), ("Limit Exceeded", C, C)]),
    (0x06, &[("Performance Met", I, I), ("Performance Lags", C, W)]),
    (
        0x07,
        &[
            ("Transition to OK", I, I),
            ("Transition to Non-critical from OK", W, W),
            ("Transition to Critical from less severe", C, C),
            ("Transition to Non-recoverable from less severe", C, C),
            ("Transition to Non-critical from more severe", W, W),
            ("Transition to Critical from Non-recoverable", C, C),
            ("Transition to Non-recoverable", C, C),
            ("Monitor", W, W),
            ("Informational", I, I),
        ],
    ),
    (0x08, &[("Device Absent", I, I), ("Device Present", I, I)]),
    (0x09, &[("Device Disabled", I, I), ("Device Enabled", I, I)]),
    (
        0x0A,
        &[
            ("Transition to Running", I, I),
            ("Transition to In Test", I, I),
            ("Transition to Power Off", I, I),
            ("Transition to On Line", I, I),
            ("Transition to Off Line", I, I),
            ("Transition to Off Duty", I, I),
            ("Transition to Degraded", I, I),
            ("Transition to Power Save", I, I),
            ("Install Error", I, I),
        ],
    ),
    (
        0x0B,
        &[
            ("Fully Redundant", I, I),
            ("Redundancy Lost", C, W),
            ("Redundancy Degraded", W, W),
            ("Non-Redundant: Sufficient from Redundant", C, W),
            ("Non-Redundant: Sufficient from Insufficient", C, W),
            ("Non-Redundant: Insufficient Resources", C, W),
            ("Redundancy Degraded from Fully Redundant", W, W),
            ("Redundancy Degraded from Non-Redundant", W, W),
        ],
    ),
    (0x0C, &[("D0 Power State", I, I), ("D1 Power State", I, I), ("D2 Power State", I, I), ("D3 Power State", I, I)]),
];

// The states of event/reading type 0x6F by sensor type and offset, worded and rated likewise.
const SPECIFIC: &[(u8, States)] = &[
    (
        0x05,
        &[
            ("General Chassis intrusion", C, C),
            ("Drive Bay intrusion", C, C),
            ("I/O Card area intrusion", C, C),
            ("Processor area intrusion", C, C),
            ("System unplugged from LAN", C, C),
            ("Unauthorized dock", C, C),
            ("FAN area intrusion", C, C),
        ],
    ),
    (
        0x06,
        &[
            ("Front Panel Lockout violation attempted", C, C),
            ("Pre-boot password violation - user password", C, C),
            ("Pre-boot password violation - setup password", C, C),
            ("Pre-boot password violation - network boot password", C, C),
            ("Other pre-boot password violation", C, C),
            ("Out-of-band access password violation", C, C),
        ],
    ),
    (
        0x07,
        &[
            ("IERR", C, C),
            ("Thermal Trip", C, C),
            ("FRB1/BIST failure", C, C),
            ("FRB2/Hang in POST failure", C, C),
            ("FRB3/Processor startup/init failure", C, C),
            ("Configuration Error", C, C),
            ("SM BIOS Uncorrectable CPU-complex Error", C, C),
            ("Presence detected", I, I),
            ("Disabled", C, C),
            ("Terminator presence detected", C, C),
            ("Throttled", W, W),
            ("Uncorrectable machine check exception", C, C),
            ("Correctable machine check error", W, W),
        ],
    ),
    (
        0x08,
        &[
            ("Presence detected", I, I),
            ("Failure detected", C, C),
            ("Predictive failure", C, C),
            ("Power Supply AC lost", C, W),
            ("AC lost or out-of-range", C, W),
            ("AC out-of-range, but present", C, W),
            ("Config Error", C, C),
            ("Power Supply Inactive", W, W),
        ],
    ),
    (
        0x09,
        &[
            ("Power off/down", I, I),
            ("Power cycle", I, I),
            ("240VA power down", W, I),
            ("Interlock power down", W, W),
            ("AC lost", C, W),
            ("Soft-power control failure", C, C),
            ("Failure detected", C, C),
            ("Predictive failure", C, C),
        ],
    ),
    (
        0x0C,
        &[
            ("Correctable ECC", W, W),
            ("Uncorrectable ECC", C, C),
            ("Parity", C, C),
            ("Memory Scrub Failed", C, C),
            ("Memory Device Disabled", C, C),
            ("Correctable ECC logging limit reached", W, W),
            ("Presence Detected", I, I),
            ("Configuration Error", C, C),
            ("Spare", I, I),
            ("Throttled", W, W),
            ("Critical Overtemperature", C, C),
        ],
    ),
    (
        0x0D,
        &[
            ("Drive Present", I, I),
            ("Drive Fault", C, C),
            ("Predictive Failure", C, C),
            ("Hot Spare", I, I),
            ("Parity Check In Progress", I, I),
            ("In Critical Array", C, C),
            ("In Failed Array", C, C),
            ("Rebuild In Progress", I, I),
            ("Rebuild Aborted", C, C),
        ],
    ),
    (0x0F, &[("System Firmware Error", C, C), ("System Firmware Hang", C, C), ("System Firmware Progress", I, I)]),
    (
        0x10,
        &[
            ("Correctable memory error logging disabled", C, C),
            ("Event logging disabled", C, C),
            ("Log area reset/cleared", I, I),
            ("All event logging disabled", C, C),
            ("Log full", C, C),
            ("Log almost full", W, W),
            ("Correctable machine check error logging disabled", C, C),
        ],
    ),
    (
        0x12,
        &[
            ("System Reconfigured", W, W),
            ("OEM System boot event", I, I),
            ("Undetermined system hardware failure", C, C),
            ("Entry added to auxiliary log", I, I),
            ("PEF Action", I, I),
            ("Timestamp Clock Sync", W, W),
        ],
    ),
    (
        0x13,
        &[
            ("NMI/Diag Interrupt", C, C),
            ("Bus Timeout", C, C),
            ("I/O Channel check NMI", C, C),
            ("Software NMI", W, W),
            ("PCI PERR", C, C),
            ("PCI SERR", C, C),
            ("EISA failsafe timeout", C, C),
            ("Bus Correctable error", W, W),
            ("Bus Uncorrectable error", C, C),
            ("Fatal NMI", C, C),
            ("Bus Fatal Error", C, C),
            ("Bus Degraded", W, W),
        ],
    ),
    (
        0x14,
        &[
            ("Power Button pressed", I, I),
            ("Sleep Button pressed", I, I),
            ("Reset Button pressed", I, I),
            ("FRU Latch", W, W),
            ("FRU Service", W, W),
        ],
    ),
    (0x19, &[("Soft Power Control Failure", C, C), ("Thermal Trip", C, C)]),
    (0x1B, &[("Connected", I, I), ("Config Error", C, C)]),
    (
        0x1D,
        &[
            ("Initiated by power up", I, I),
            ("Initiated by hard reset", I, I),
            ("Initiated by warm reset", I, I),
            ("User requested PXE boot", I, I),
            ("Automatic boot to diagnostic", I, I),
            ("OS initiated hard reset", W, W),
            ("OS initiated warm reset", W, W),
            ("System Restart", I, I),
        ],
    ),
    (
        0x1E,
        &[
            ("No bootable media", C, C),
            ("Non-bootable disk in drive", C, C),
            ("PXE server not found", C, C),
            ("Invalid boot sector", C, C),
            ("Timeout waiting for selection", W, W),
        ],
    ),
    (
        0x1F,
        &[
            ("A: boot completed", I, I),
            ("C: boot completed", I, I),
            ("PXE boot completed", I, I),
            ("Diagnostic boot completed", I, I),
            ("CD-ROM boot completed", I, I),
            ("ROM boot completed", I, I),
            ("boot completed - device not specified", W, W),
            ("Installation started", I, I),
            ("Installation completed", I, I),
            ("Installation aborted", W, W),
            ("Installation failed", C, C),
        ],
    ),
    (
        0x20,
        &[
            ("Error during system startup", C, C),
            ("Run-time critical stop", C, C),
            ("OS graceful stop", W, W),
            ("OS graceful shutdown", W, W),
            ("PEF initiated soft shutdown", W, W),
            ("Agent not responding", C, C),
        ],
    ),
    (
        0x21,
        &[
            ("Fault Status", C, C),
            ("Identify Status", W, W),
            ("Device Installed", I, I),
            ("Ready for Device Installation", I, I),
            ("Ready for Device Removal", I, I),
            ("Slot Power is Off", I, I),
            ("Device Removal Request", W, W),
            ("Interlock", W, W),
            ("Slot is Disabled", W, W),
            ("Spare Device", I, I),
        ],
    ),
    (
        0x22,
        &[
            ("S0/G0: working", I, I),
            ("S1: sleeping with system hw & processor context maintained", I, I),
            ("S2: sleeping, processor context lost", I, I),
            ("S3: sleeping, processor & hw context lost, memory retained", I, I),
            ("S4: non-volatile sleep/suspend-to-disk", I, I),
            ("S5/G2: soft-off", I, I),
            ("S4/S5: soft-off", I, I),
            ("G3: mechanical off", I, I),
            ("Sleeping in S1/S2/S3 state", I, I),
            ("G1: sleeping", I, I),
            ("S5: entered by override", I, I),
            ("Legacy ON state", I, I),
            ("Legacy OFF state", I, I),
            ("", I, I),
            ("Unknown", C, C),
        ],
    ),
    (
        0x23,
        &[
            ("Timer expired", W, W),
            ("Hard reset", C, C),
            ("Power down", C, C),
            ("Power cycle", C, C),
            ("", I, I),
            ("", I, I),
            ("", I, I),
            ("", I, I),
            ("Timer interrupt", W, W),
        ],
    ),
    (
        0x24,
        &[
            ("Platform generated page", I, I),
            ("Platform generated LAN alert", I, I),
            ("Platform Event Trap generated", I, I),
            ("Platform generated SNMP trap, OEM format", I, I),
        ],
    ),
    (0x25, &[("Present", I, I), ("Absent", C, C), ("Disabled", C, C)]),
    (0x27, &[("Heartbeat Lost", W, W), ("Heartbeat", I, I)]),
    (
        0x28,
        &[
            ("Sensor access degraded or unavailable", C, C),
            ("Controller access degraded or unavailable", C, C),
            ("Management controller off-line", C, C),
            ("Management controller unavailable", C, C),
            ("Sensor failure", C, C),
            ("FRU failure", C, C),
        ],
    ),
    (0x29, &[("Low", W, W), ("Failed", C, C), ("Presence Detected", I, I)]),
    (0x2B, &[("Hardware change detected", I, I), ("Firmware or software change detected", I, I)]),
    (
        0x2C,
        &[
            ("Not Installed", C, C),
            ("Inactive", C, C),
            ("Activation Requested", W, W),
            ("Activation in Progress", W, W),
            ("Active", I, I),
            ("Deactivation Requested", W, W),
            ("Deactivation in Progress", W, W),
            ("Communication lost", C, C),
        ],
    ),
];

// The units by their IPMI code. We name them as ipmitool does, but for the everyday ones, which go by their signs.
const UNITS: [&str; 93] = [
    "",
    "°C",
    "°F",
    "K",
    "V",
    "A",
    "W",
    "Joules",
    "Coulombs",
    "VA",
    "Nits",
    "lumen",
    "lux",
    "Candela",
    "kPa",
    "PSI",
    "Newton",
    "CFM",
    "RPM",
    "Hz",
    "microsecond",
    "millisecond",
    "second",
    "minute",
    "hour",
    "day",
    "week",
    "mil",
    "inches",
    "feet",
    "cu in",
    "cu feet",
    "mm",
    "cm",
    "m",
    "cu cm",
    "cu m",
    "liters",
    "fluid ounce",
    "radians",
    "steradians",
    "revolutions",
    "cycles",
    "gravities",
    "ounce",
    "pound",
    "ft-lb",
    "oz-in",
    "gauss",
    "gilberts",
    "henry",
    "millihenry",
    "farad",
    "microfarad",
    "ohms",
    "siemens",
    "mole",
    "becquerel",
    "PPM",
    "reserved",
    "Decibels",
    "DbA",
    "DbC",
    "gray",
    "sievert",
    "color temp deg K",
    "bit",
    "kilobit",
    "megabit",
    "gigabit",
    "byte",
    "kilobyte",
    "megabyte",
    "gigabyte",
    "word",
    "dword",
    "qword",
    "line",
    "hit",
    "miss",
    "retry",
    "reset",
    "overflow",
    "underrun",
    "collision",
    "packets",
    "messages",
    "characters",
    "error",
    "correctable error",
    "uncorrectable error",
    "fatal error",
    "grams",
];

#[cfg(test)]
mod tests {
    // We only test how a sensor's record converts readings here, `tests/ipmi.rs` reads the X10's through the mock.

    use super::*;

    /// A full record of sensor 1 with `units` (format, base, modifier), M, B, R and B exponents, and a linearization.
    fn full(units: [u8; 3], [m, b, r, b_exponent]: [i16; 4], linearization: u8) -> Record {
        let mut record = vec![0; 48];
        record[2..=3].copy_from_slice(&[0x51, 0x01]);
        record[5..=7].copy_from_slice(&[0x20, 0x00, 1]);
        record[12..=13].copy_from_slice(&[0x0B, 0x01]);
        record[20..=23].copy_from_slice(&[units[0], units[1], units[2], linearization]);
        record[24..=27].copy_from_slice(&[m as u8, (m >> 2) as u8 & 0xC0, b as u8, (b >> 2) as u8 & 0xC0]);
        record[29] = (r as u8) << 4 | b_exponent as u8 & 0x0F;
        record[47] = 0xC0;
        record[4] = record.len() as u8 - 5;
        Record(record)
    }

    #[test]
    fn a_shared_record_is_a_sensor_each() {
        let sensors = |record: &[u8]| -> Vec<_> {
            let sensors = shared(record.to_vec());
            sensors.iter().map(|sensor| format!("{:02X} {}", sensor.number(), sensor.name())).collect()
        };
        let mut compact = vec![0; 32];
        (compact[3], compact[7], compact[31]) = (0x02, 0x40, 0xC3);
        compact.extend(b"Fan");
        assert_eq!(sensors(&compact), ["40 Fan"]);

        // Three of them, numbered from 1.
        compact[23..=24].copy_from_slice(&[0x03, 1]);
        assert_eq!(sensors(&compact), ["40 Fan1", "41 Fan2", "42 Fan3"]);

        // Two that only send events, lettered from Z.
        let mut event_only = vec![0; 17];
        (event_only[3], event_only[12], event_only[13], event_only[16]) = (0x03, 0x42, 25, 0xC2);
        event_only.extend(b"PS");
        assert_eq!(sensors(&event_only), ["00 PSZ", "01 PSAA"]);
    }

    /// What a server that is well logs every day, and a state the specification skips.
    #[test]
    fn words_a_sensors_own_states() {
        let state = |sensor, offset| meaning(sensor, 0x6F, offset).map(|(text, asserting, _)| (text, asserting));
        assert_eq!(state(0x14, 0), Some(("Power Button pressed", I)));
        assert_eq!(state(0x1D, 0), Some(("Initiated by power up", I)));
        assert_eq!(state(0x22, 5), Some(("S5/G2: soft-off", I)));
        assert_eq!(state(0x27, 0), Some(("Heartbeat Lost", W)));
        assert_eq!(state(0x22, 13), None);
    }

    #[test]
    fn converts_signed_scaled_and_linearized_readings() {
        // Two's and one's complement: 0xFB is -5 either way, 0xFE is -2 and -1.
        assert_eq!(full([0x80, 1, 0], [1, 0, 0, 0], 0).value(0xFB), Some(-5.0));
        assert_eq!(full([0x40, 1, 0], [1, 0, 0, 0], 0).value(0xFE), Some(-1.0));
        // (-3 x + 500 10^-1) 10^-2, M and B signed.
        assert_eq!(full([0, 4, 0], [-3, 500, -2, -1], 0).value(10), Some(0.2));
        // (300 x - 400 10^-2) 10^-2, squared: M and B past 8 bits.
        assert_eq!(full([0, 6, 0], [300, -400, -2, -2], 0x08).value(2), Some(35.522));
        // No analog reading.
        assert_eq!(full([0xC0, 4, 0], [1, 0, 0, 0], 0).value(2), None);
        assert_eq!(
            (full([0, 1, 0], [1, 0, 0, 0], 0).unit(), full([0, 18, 0], [1, 0, 0, 0], 0).unit()),
            ("°C".into(), "RPM".into())
        );
    }
}
