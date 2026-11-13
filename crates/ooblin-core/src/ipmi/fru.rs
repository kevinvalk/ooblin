//! FRU inventory (Platform Management FRU Information Storage Definition v1.0): the chassis, board and product areas.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{STORAGE, Session};
use crate::{Error, Result};

/// A FRU's information, by area; an area the FRU doesn't have is None and can't be set. Strings as decoded (any
/// encoding), trimmed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Fru {
    pub chassis: Option<Chassis>,
    pub board: Option<Board>,
    pub product: Option<Product>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "FruChassis")]
pub struct Chassis {
    /// SMBIOS chassis type.
    pub kind: u8,
    pub part_number: String,
    pub serial_number: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "FruBoard")]
pub struct Board {
    /// Unix seconds, None = unspecified.
    pub manufactured: Option<u32>,
    pub manufacturer: String,
    pub product: String,
    pub serial_number: String,
    pub part_number: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts", rename = "FruProduct")]
pub struct Product {
    pub manufacturer: String,
    pub name: String,
    pub part_number: String,
    pub version: String,
    pub serial_number: String,
    pub asset_tag: String,
}

/// Each area's name and fixed bytes between its length byte and its first field, in header order.
const AREAS: [(&str, usize); 3] = [("chassis", 1), ("board", 4), ("product", 1)];
/// Largest read or write; what ipmitool uses over lanplus.
const CHUNK: usize = 32;
/// Board dates count minutes from 1996-01-01 00:00.
const EPOCH: u32 = 820_454_400;

pub async fn read(session: &Session, id: u8) -> Result<Fru> {
    fru(&image(session, id).await?)
}

/// What a FRU's bytes say.
fn fru(image: &[u8]) -> Result<Fru> {
    let [chassis, board, product] = areas(image)?;
    Ok(Fru {
        chassis: chassis.map(|a| {
            let [part_number, serial_number] = a.values();
            Chassis { kind: a.fixed[0], part_number, serial_number }
        }),
        board: board.map(|a| {
            let [manufacturer, product, serial_number, part_number] = a.values();
            let minutes = u32::from_le_bytes([a.fixed[1], a.fixed[2], a.fixed[3], 0]);
            let manufactured = (minutes != 0).then(|| EPOCH + minutes * 60);
            Board { manufactured, manufacturer, product, serial_number, part_number }
        }),
        product: product.map(|a| {
            let [manufacturer, name, part_number, version, serial_number, asset_tag] = a.values();
            Product { manufacturer, name, part_number, version, serial_number, asset_tag }
        }),
    })
}

/// Changes the FRU to `fru`: only areas whose fields differ from what it holds now, each re-encoded in place, grown
/// into its room where it must be, written in chunks, read back and compared.
pub async fn write(session: &Session, id: u8, fru: &Fru) -> Result<()> {
    for Write { name, at, was, bytes } in changes(&image(session, id).await?, fru)? {
        let differs = |(old, new): (&u8, &u8)| old != new;
        let Some(first) = was.iter().zip(&bytes).position(differs) else { continue };
        let last = was.iter().zip(&bytes).rposition(differs).unwrap_or(first);
        write_range(session, id, name, at + first, &bytes[first..=last]).await?;
        if read_range(session, id, at, bytes.len()).await? != bytes {
            return Err(Error::Protocol(format!("the {name} area reads back other than we wrote")));
        }
    }
    Ok(())
}

/// An area's change: its name, where it starts, what's there now and what goes there.
struct Write {
    name: &'static str,
    at: usize,
    was: Vec<u8>,
    bytes: Vec<u8>,
}

/// What makes a FRU's bytes `image` hold `fru`, area by area. Every area is checked before any is written, so a
/// refusal changes nothing.
fn changes(image: &[u8], fru: &Fru) -> Result<Vec<Write>> {
    let areas = areas(image)?;
    let date = minutes(fru.board.as_ref().and_then(|b| b.manufactured))?;
    // Per area, the fixed bytes we let you change (always its last ones) and its named fields.
    let wanted = [
        fru.chassis.as_ref().map(|c| (vec![c.kind], vec![&*c.part_number, &c.serial_number])),
        fru.board.as_ref().map(|b| (date, vec![&*b.manufacturer, &b.product, &b.serial_number, &b.part_number])),
        fru.product.as_ref().map(|p| {
            (vec![], vec![&*p.manufacturer, &p.name, &p.part_number, &p.version, &p.serial_number, &p.asset_tag])
        }),
    ];
    let mut changes = vec![];
    for (((name, _), area), wanted) in AREAS.iter().zip(&areas).zip(wanted) {
        let Some((fixed, values)) = wanted else { continue };
        let area = area.as_ref().ok_or_else(|| Error::Invalid(format!("this FRU has no {name} area")))?;
        if let Some(bytes) = area.rebuild(name, &fixed, &values)? {
            changes.push(Write { name, at: area.at, was: area.room[..bytes.len()].to_vec(), bytes });
        }
    }
    Ok(changes)
}

/// One info area as the FRU holds it now, and the room it may grow into: up to the next area the header points at, or
/// the FRU's end, as nothing else is there.
struct Area {
    at: usize,
    bytes: Vec<u8>,
    room: Vec<u8>,
    fixed: Vec<u8>,
    /// Whether it runs into another area the header points at, so writing it would clobber that one.
    overlaps: bool,
    /// Every field up to the end marker, custom ones included, each with its type/length byte.
    fields: Vec<Vec<u8>>,
}

impl Area {
    fn parse(image: &[u8], name: &str, at: usize, fixed: usize, limit: usize) -> Result<Self> {
        let damaged = || Error::Invalid(format!("the FRU's {name} area is damaged"));
        let len = *image.get(at + 1).ok_or_else(damaged)? as usize * 8;
        let bytes = image.get(at..at + len).ok_or_else(damaged)?.to_vec();
        let mut fields = vec![];
        let mut next = 2 + fixed;
        // The last byte is the checksum, so the end marker must come before it.
        while next + 1 < len && bytes[next] != 0xC1 {
            let end = next + 1 + (bytes[next] & 0x3F) as usize;
            fields.push(bytes.get(next..end).ok_or_else(damaged)?.to_vec());
            next = end;
        }
        if next + 1 >= len {
            return Err(damaged());
        }
        let room = image[at..limit.min(image.len()).max(at + len)].to_vec();
        Ok(Area { at, fixed: bytes[2..2 + fixed].to_vec(), overlaps: at + len > limit, bytes, room, fields })
    }

    fn value(&self, field: usize) -> String {
        self.fields.get(field).map(|f| decode(f)).unwrap_or_default()
    }

    fn values<const N: usize>(&self) -> [String; N] {
        std::array::from_fn(|i| self.value(i))
    }

    /// The area with `fixed` as its last fixed bytes and `values` as its named fields, None if nothing changes.
    fn rebuild(&self, name: &str, fixed: &[u8], values: &[&str]) -> Result<Option<Vec<u8>>> {
        let fixed = [&self.fixed[..self.fixed.len() - fixed.len()], fixed].concat();
        let changed: Vec<usize> = (0..values.len()).filter(|&i| values[i].trim() != self.value(i)).collect();
        if fixed == self.fixed && changed.is_empty() {
            return Ok(None);
        }
        if self.overlaps {
            return Err(Error::Invalid(format!("the FRU's {name} area overlaps another, so we leave it alone")));
        }
        if sum(&self.bytes) != 0 {
            return Err(Error::Invalid(format!("the FRU's {name} area has a bad checksum, so we leave it alone")));
        }
        let mut fields = self.fields.clone();
        if fields.len() < values.len() {
            fields.resize(values.len(), vec![0xC0]);
        }
        for i in changed {
            fields[i] = encode(values[i].trim())?;
        }
        let mut bytes = [&self.bytes[..2], &fixed, &fields.concat(), &[0xC1]].concat();
        // Its length in 8-byte steps, grown into its room where it needs to be.
        let len = self.bytes.len().max((bytes.len() + 1).div_ceil(8) * 8);
        let room = (self.room.len() / 8 * 8).min(255 * 8);
        if len > room {
            let over = bytes.len() + 1 - room;
            let s = if over == 1 { "" } else { "s" };
            return Err(Error::Invalid(format!(
                "that doesn't fit the FRU's {name} area, it's {over} byte{s} too long"
            )));
        }
        bytes[1] = (len / 8) as u8;
        bytes.resize(len - 1, 0);
        bytes.push(sum(&bytes).wrapping_neg());
        Ok(Some(bytes))
    }
}

fn areas(image: &[u8]) -> Result<[Option<Area>; 3]> {
    let header = image.get(..8).filter(|h| h[0] == 1 && sum(h) == 0);
    let header = header.ok_or_else(|| Error::Invalid("the FRU's header is damaged or of an unknown version".into()))?;
    let area = |i: usize| {
        let (name, fixed) = AREAS[i];
        let at = header[2 + i] as usize * 8;
        let others = (1..6).filter(|&j| j != 2 + i).map(|j| header[j] as usize * 8);
        let next = others.filter(|&o| o >= at).min().unwrap_or(usize::MAX);
        (at != 0).then(|| Area::parse(image, name, at, fixed, next)).transpose()
    };
    Ok([area(0)?, area(1)?, area(2)?])
}

fn decode(field: &[u8]) -> String {
    let data = &field[1..];
    let text: String = match field[0] >> 6 {
        0 => data.iter().map(|b| format!("{b:02x}")).collect(),
        // BCD plus
        1 => data.iter().flat_map(|b| [b >> 4, b & 0x0F]).map(|n| b"0123456789 -.:,_"[n as usize] as char).collect(),
        // 6-bit ASCII, packed from the low bits of the first byte up
        2 => (0..data.len() * 8 / 6)
            .map(|i| {
                let (byte, shift) = (i * 6 / 8, i * 6 % 8);
                let word = u16::from_le_bytes([data[byte], *data.get(byte + 1).unwrap_or(&0)]);
                ((word >> shift) as u8 & 0x3F) + 0x20
            })
            .map(char::from)
            .collect(),
        _ => data.iter().copied().map(char::from).collect(),
    };
    text.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string()
}

fn encode(value: &str) -> Result<Vec<u8>> {
    if !value.bytes().all(|b| (0x20..0x7F).contains(&b)) {
        return Err(Error::Invalid(format!("a FRU holds only printable ASCII, not \"{value}\"")));
    }
    match value.as_bytes() {
        [] => Ok(vec![0xC0]),
        // 0xC1 ends the fields, so a single character can only be 6-bit ASCII.
        &[c @ 0x20..0x60] => Ok(vec![0x81, c - 0x20]),
        [_] => Err(Error::Invalid(format!(
            "a FRU can't hold the single character \"{value}\", only a capital, digit or sign"
        ))),
        bytes if bytes.len() <= 63 => Ok([&[0xC0 | bytes.len() as u8], bytes].concat()),
        bytes => {
            Err(Error::Invalid(format!("a FRU field holds at most 63 characters, \"{value}\" has {}", bytes.len())))
        }
    }
}

fn minutes(manufactured: Option<u32>) -> Result<Vec<u8>> {
    let Some(at) = manufactured else { return Ok(vec![0; 3]) };
    let minutes = at.saturating_sub(EPOCH) / 60;
    if !(1..1 << 24).contains(&minutes) {
        return Err(Error::Invalid("a FRU's board date can only be from 1996 through 2027".into()));
    }
    Ok(minutes.to_le_bytes()[..3].to_vec())
}

async fn image(session: &Session, id: u8) -> Result<Vec<u8>> {
    let info = session.request(STORAGE, 0x10, &[id]).await?;
    let &[low, high, access, ..] = &info[..] else {
        return Err(Error::Protocol("the FRU inventory info is too short".into()));
    };
    if access & 1 != 0 {
        return Err(Error::Invalid("this FRU is addressed in words, which we don't support".into()));
    }
    read_range(session, id, 0, u16::from_le_bytes([low, high]).into()).await
}

/// Reads up to `len` bytes from `at`, fewer if the FRU ends early (its size from the info is sometimes too big).
async fn read_range(session: &Session, id: u8, at: usize, len: usize) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(len);
    let mut chunk = CHUNK;
    while data.len() < len {
        let [low, high] = ((at + data.len()) as u16).to_le_bytes();
        let count = chunk.min(len - data.len());
        match session.request(STORAGE, 0x11, &[id, low, high, count as u8]).await {
            Ok(rsp) => {
                let Some((&returned, read)) = rsp.split_first() else { break };
                let read = &read[..read.len().min(returned.into()).min(count)];
                if read.is_empty() {
                    break;
                }
                data.extend_from_slice(read);
            }
            Err(Error::Refused(0xC7 | 0xC8 | 0xCA)) if chunk > 8 => chunk -= 8,
            Err(e) => return Err(e),
        }
    }
    Ok(data)
}

async fn write_range(session: &Session, id: u8, name: &str, at: usize, bytes: &[u8]) -> Result<()> {
    let (mut done, mut chunk, mut busy) = (0, CHUNK, 0);
    while done < bytes.len() {
        let [low, high] = ((at + done) as u16).to_le_bytes();
        let data = &bytes[done..bytes.len().min(done + chunk)];
        match session.request(STORAGE, 0x12, &[&[id, low, high], data].concat()).await {
            Ok(rsp) if rsp.first().is_some_and(|&n| n > 0) => done += data.len().min(rsp[0].into()),
            Ok(_) => return Err(Error::Protocol(format!("the BMC wrote none of the FRU's {name} area"))),
            Err(Error::Refused(0xC7 | 0xC8 | 0xCA)) if chunk > 8 => chunk -= 8,
            Err(Error::Refused(0x81)) if busy < 5 => {
                busy += 1;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(Error::Refused(0x80)) => {
                let partly = if done > 0 { ", after changing part of it" } else { "" };
                return Err(Error::Invalid(format!("the BMC write-protects the FRU's {name} area{partly}")));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn sum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, &b| sum.wrapping_add(b))
}

#[cfg(test)]
mod tests {
    //! What a write would do to a FRU's bytes; `tests/ipmi.rs` writes the X10's through the mock.

    use super::*;

    const EPOCH_PLUS_A_MINUTE: u32 = EPOCH + 60;

    fn checksum(bytes: &[u8]) -> u8 {
        sum(bytes).wrapping_neg()
    }

    /// An info area of `len` bytes: version, length, fixed bytes, fields, end marker, padding, checksum.
    fn area(len: usize, fixed: &[u8], fields: &[&[u8]]) -> Vec<u8> {
        let mut bytes = [&[1, (len / 8) as u8], fixed, &fields.concat(), &[0xC1]].concat();
        bytes.resize(len - 1, 0);
        bytes.push(checksum(&bytes));
        bytes
    }

    /// A 256-byte FRU: header, an internal use area, chassis, board and product, then filler no area holds.
    fn synthetic() -> Vec<u8> {
        let chassis = area(
            32,
            &[0x01],
            &[
                &[0x83, 0xA1, 0x38, 0x46], // 6-bit "ABC1"
                &[0x43, 0x12, 0xB3, 0x45], // BCD+ "12-345"
                &[0xC3, b'O', b'E', b'M'], // custom
            ],
        );
        let board =
            area(40, &[0x00, 0x56, 0x34, 0x12], &[b"\xC4ACME", &[0xC0], &[0xC0], &[0xC0], b"\xC2F1", b"\xC4cust"]);
        let product = area(48, &[0x00], &[&[0xC0][..]; 7]);
        let mut header = vec![1, 1, 2, 6, 11, 0, 0];
        header.push(checksum(&header));
        let mut image = [header, vec![0x55; 8], chassis, board, product].concat();
        image.resize(256, 0xAA);
        image
    }

    /// `image` once `fru` is written to it.
    fn written(image: &[u8], fru: &Fru) -> Result<Vec<u8>> {
        let mut image = image.to_vec();
        for Write { at, bytes, .. } in changes(&image, fru)? {
            image[at..at + bytes.len()].copy_from_slice(&bytes);
        }
        Ok(image)
    }

    fn refusal(result: Result<Vec<u8>>) -> String {
        match result {
            Err(Error::Invalid(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn reads_six_bit_bcd_and_dates() {
        let fru = fru(&synthetic()).unwrap();
        let chassis = fru.chassis.unwrap();
        assert_eq!((chassis.kind, &*chassis.part_number, &*chassis.serial_number), (1, "ABC1", "12-345"));
        let board = fru.board.unwrap();
        assert_eq!((board.manufactured, &*board.manufacturer), (Some(EPOCH + 0x123456 * 60), "ACME"));
    }

    #[test]
    fn edits_keep_custom_fields_other_encodings_and_other_areas() {
        let before = synthetic();
        let mut fru = fru(&before).unwrap();
        let chassis = fru.chassis.as_mut().unwrap();
        (chassis.kind, chassis.serial_number) = (0x17, "ABC123".into());
        let board = fru.board.as_mut().unwrap();
        (board.product, board.manufactured) = ("X".into(), Some(EPOCH_PLUS_A_MINUTE));
        let product = fru.product.as_mut().unwrap();
        (product.name, product.asset_tag) = ("Homelab".into(), "rack-2".into());
        let after = written(&before, &fru).unwrap();
        assert_eq!(super::fru(&after).unwrap(), fru);

        assert_eq!(after[..16], before[..16], "header and internal use area");
        assert_eq!(after[136..], before[136..], "what no area holds");
        let chassis = &after[16..48];
        assert_eq!(chassis[..3], [1, 4, 0x17]);
        assert_eq!(chassis[3..14], [&[0x83, 0xA1, 0x38, 0x46][..], b"\xC6ABC123"].concat(), "6-bit kept, serial ASCII");
        assert_eq!(chassis[14..19], *b"\xC3OEM\xC1");
        let board = &after[48..88];
        assert_eq!(board[2..6], [0, 1, 0, 0]);
        assert_eq!(board[6..20], *b"\xC4ACME\x81\x38\xC0\xC0\xC2F1\xC4c", "one character goes 6-bit");
    }

    #[test]
    fn writing_what_is_there_changes_nothing() {
        let image = synthetic();
        assert!(changes(&image, &fru(&image).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn refuses_a_bad_checksum() {
        let mut image = synthetic();
        image[16 + 20] ^= 1; // padding in the chassis area
        let mut fru = fru(&image).unwrap();
        fru.chassis.as_mut().unwrap().kind = 0x17;
        assert!(refusal(written(&image, &fru)).contains("chassis area has a bad checksum"));
    }

    #[test]
    fn refuses_values_a_fru_cannot_hold() {
        let image = synthetic();
        for (value, refusal) in [
            ("a", "single character"),
            ("é!", "printable ASCII"),
            ("tab\there", "printable ASCII"),
            (&"x".repeat(64), "at most 63"),
        ] {
            let mut fru = fru(&image).unwrap();
            fru.product.as_mut().unwrap().name = value.into();
            assert!(super::tests::refusal(written(&image, &fru)).contains(refusal), "{value}");
        }
    }

    /// The board area (40 bytes at 48) claims 64 bytes, swallowing the product area at 88, yet still sums to zero.
    #[test]
    fn refuses_an_area_that_overlaps_another() {
        let mut image = synthetic();
        image[49] += 3;
        image[87] = image[87].wrapping_sub(3);
        let mut fru = fru(&image).unwrap();
        fru.board.as_mut().unwrap().product = "X10SL7F".into();
        assert!(refusal(written(&image, &fru)).contains("board area overlaps another"));
    }

    /// The last area grows into the room after it, up to the FRU's end; one that won't fit stops every area's write.
    #[test]
    fn grows_the_last_area_up_to_the_end() {
        let image = synthetic();
        let mut fru = fru(&image).unwrap();
        fru.product.as_mut().unwrap().name = "a name longer than the product area has room for".into();
        let after = written(&image, &fru).unwrap();
        assert!(after[89] > image[89], "the product area's length grew");
        assert_eq!(super::fru(&after).unwrap(), fru);

        fru.chassis.as_mut().unwrap().kind = 0x17;
        let product = fru.product.as_mut().unwrap();
        for field in [&mut product.name, &mut product.part_number, &mut product.version, &mut product.asset_tag] {
            *field = "x".repeat(60);
        }
        assert!(refusal(written(&image, &fru)).contains("product area, it's"));
    }
}
