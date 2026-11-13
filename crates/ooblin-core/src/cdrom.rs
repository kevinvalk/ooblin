//! A CD-ROM drive whose disc is an image file here: its SCSI (MMC) commands answered, whichever way they come, AMT's
//! IDER (`amt::ider`) or a BMC's virtual USB drive (`supermicro::media`). We answer as MeshCommander's
//! `amt-ider-ws-0.0.1.js` and Intel's ui-toolkit (both Apache-2.0) do, AMT being known to boot that way, and INQUIRY as
//! the BMC's own viewer does (`isohandler.js`).

use std::io::SeekFrom;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::Result;

pub struct Cdrom {
    image: File,
    /// Its size in 2048-byte blocks.
    blocks: u64,
    /// Why its last command failed (sense key, additional sense code and qualifier), for REQUEST SENSE.
    sense: [u8; 3],
}

/// What a command gets: data, of which as much as it takes; blocks of the disc to read; nothing but success; or a
/// failure, with its sense.
pub enum Answer {
    Data(Vec<u8>),
    Read { lba: u64, blocks: u64 },
    Done,
    Failed([u8; 3]),
}

impl Cdrom {
    pub async fn new(image: std::fs::File) -> Result<Self> {
        let image = File::from_std(image);
        let blocks = image.metadata().await?.len() / 2048;
        Ok(Self { image, blocks, sense: [0; 3] })
    }

    /// What `cdb` gets.
    pub fn answer(&mut self, cdb: &[u8]) -> Answer {
        let u16_at = |at: usize| u16::from_be_bytes([cdb[at], cdb[at + 1]]);
        // Unlike the references we stop at the allocation length, as SCSI wants: more is an overrun to the host.
        let allocation = match cdb[0] {
            0x03 | 0x12 | 0x1A => cdb[4].into(),
            0x23 | 0x43 | 0x46 | 0x4A | 0x5A => u16_at(7).into(),
            _ => usize::MAX,
        };
        let data = |data: &[u8]| match &data[..data.len().min(allocation)] {
            [] => Answer::Done,
            data => Answer::Data(data.to_vec()),
        };
        // REQUEST SENSE: why the last command failed, in fixed format, which it then forgets.
        if cdb[0] == 0x03 {
            let [key, asc, ascq] = std::mem::take(&mut self.sense);
            return data(&[0x70, 0, key, 0, 0, 0, 0, 10, 0, 0, 0, 0, asc, ascq, 0, 0, 0, 0]);
        }
        self.sense = [0; 3];
        let u32_at = |at: usize| u64::from(u32::from_be_bytes([cdb[at], cdb[at + 1], cdb[at + 2], cdb[at + 3]]));
        match cdb[0] {
            // READ (6), READ (10) and READ (12).
            0x08 | 0x28 | 0xA8 => {
                let (lba, blocks) = match cdb[0] {
                    // A length of 0 is 256 blocks here.
                    0x08 => (
                        u64::from(cdb[1] & 0x1f) << 16 | u64::from(u16_at(2)),
                        if cdb[4] == 0 { 256 } else { cdb[4].into() },
                    ),
                    0x28 => (u32_at(2), u16_at(7).into()),
                    _ => (u32_at(2), u32_at(6)),
                };
                match blocks {
                    0 => Answer::Done,
                    _ if lba + blocks > self.blocks => self.fail(5, 0x21, 0), // out of range
                    _ => Answer::Read { lba, blocks },
                }
            }
            // INQUIRY: a removable CD-ROM drive, SPC-2.
            0x12 => data(&[&[5, 0x80, 4, 2, 31, 0, 0, 0][..], b"ATEN    Virtual CDROM   YS0J"].concat()),
            // MODE SENSE (6), all pages: write-protected.
            0x1A if cdb[2] == 0x3F && cdb[3] == 0 => data(&[0, 5, 0x80, 0]),
            // READ FORMAT CAPACITIES, the same for any disc.
            0x23 => data(&[0, 0, 0, 8, 0, 0, 0x0B, 0x40, 2, 0, 2, 0]),
            // READ CAPACITY: the last block and the block size.
            0x25 => {
                let last = u32::try_from(self.blocks.saturating_sub(1)).unwrap_or(u32::MAX).to_be_bytes();
                data(&[&last[..], &[0, 0, 8, 0]].concat())
            }
            // READ TOC: one data track.
            0x43 => {
                let format = match cdb[2] & 7 {
                    0 => cdb[9] >> 6,
                    format => format,
                };
                match (format, cdb[1] & 2 != 0) {
                    (1, _) => data(&[0, 0x0A, 1, 1, 0, 0x14, 1, 0, 0, 0, 0, 0]),
                    (0, true) => data(&TOC_MSF),
                    (0, false) => data(&TOC),
                    // Both references leave other formats unanswered, we say we don't have them.
                    _ => self.fail(5, 0x24, 0),
                }
            }
            0x46 => data(&configuration(cdb[1] != 2, u16_at(2))),
            // GET EVENT STATUS NOTIFICATION, for media: whether a disc is in.
            0x4A if cdb[1] == 1 || cdb[4] == 0x10 => data(&[0, 2, 0x80, 0]),
            // MODE SENSE (10).
            0x5A => match cdb[2] & 0x3F {
                0x01 => data(&CD_ERROR_RECOVERY),
                0x1A => data(&CD_POWER),
                0x1D => data(&CD_TIMEOUT),
                0x2A => data(&CD_CAPABILITIES),
                0x3F => data(&CD_ALL),
                _ => self.fail(5, 0x20, 0),
            },
            0xAC => data(&[0, 0, 0, 4, 2, 0, 0, 0]), // GET PERFORMANCE
            // TEST UNIT READY, MODE SENSE (6) of a page, START STOP, PREVENT ALLOW MEDIUM REMOVAL, events of another
            // class, MODE SELECT (10): both references say they went well.
            0x00 | 0x1A | 0x1B | 0x1E | 0x4A | 0x55 => Answer::Done,
            // Anything else, like READ DISC INFORMATION or a write: an invalid command.
            _ => self.fail(5, 0x20, 0),
        }
    }

    /// The command failed, which REQUEST SENSE then says.
    pub fn fail(&mut self, key: u8, asc: u8, ascq: u8) -> Answer {
        self.sense = [key, asc, ascq];
        Answer::Failed(self.sense)
    }

    /// Fills `buf` from the disc, from byte `at` on.
    pub async fn read(&mut self, at: u64, buf: &mut [u8]) -> Result<()> {
        self.image.seek(SeekFrom::Start(at)).await?;
        self.image.read_exact(buf).await?;
        Ok(())
    }
}

/// GET CONFIGURATION: a CD-ROM's features from `first` on, or only that one.
fn configuration(all: bool, first: u16) -> Vec<u8> {
    let features: [(u16, &[u8]); 8] = [
        (0x000, &[0, 0, 3, 4, 0, 8, 1, 0]),                // profiles: CD-ROM, current
        (0x001, &[0, 1, 3, 4, 0, 0, 0, 2]),                // core
        (0x002, &[0, 2, 3, 4, 0, 0, 0, 0]),                // morphing
        (0x003, &[0, 3, 3, 4, 0x29, 0, 0, 2]),             // removable medium
        (0x010, &[0, 0x10, 1, 8, 0, 0, 8, 0, 0, 1, 0, 0]), // random readable, 2048-byte blocks
        (0x01E, &[0, 0x1E, 3, 0]),                         // CD read
        (0x100, &[1, 0, 3, 0]),                            // power management
        (0x105, &[1, 5, 3, 0]),                            // timeout
    ];
    let mut data = vec![0, 0, 0, 8]; // the current profile, CD-ROM
    for (code, feature) in features {
        if code == first || (all && first < code) {
            data.extend(feature);
        }
    }
    [&(data.len() as u32).to_be_bytes()[..], &data].concat()
}

// Replies the references send as they are, from MeshCommander's `amt-ider-ws-0.0.1.js`.

const TOC: [u8; 20] = [0, 0x12, 1, 1, 0, 0x14, 1, 0, 0, 0, 0, 0, 0, 0x14, 0xAA, 0, 0, 0, 0, 0];
const TOC_MSF: [u8; 20] = [0, 0x12, 1, 1, 0, 0x14, 1, 0, 0, 0, 2, 0, 0, 0x14, 0xAA, 0, 0, 0, 0x34, 0x13];
const CD_ERROR_RECOVERY: [u8; 16] = [0, 0x0E, 1, 0x80, 0, 0, 0, 0, 1, 6, 0, 0xFF, 0, 0, 0, 0];
const CD_POWER: [u8; 20] = [0, 0x12, 1, 0x80, 0, 0, 0, 0, 0x1A, 0x0A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const CD_TIMEOUT: [u8; 20] = [0, 0x12, 1, 0x80, 0, 0, 0, 0, 0x1D, 0x0A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const CD_CAPABILITIES: [u8; 34] = [
    0, 0x20, 1, 0x80, 0, 0, 0, 0, 0x2A, 0x18, 0, 0, 0, 0, 0x20, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0,
];
const CD_ALL: [u8; 42] = [
    0, 0x28, 1, 0x80, 0, 0, 0, 0, 1, 6, 0, 0xFF, 0, 0, 0, 0, 0x2A, 0x18, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0x80, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
