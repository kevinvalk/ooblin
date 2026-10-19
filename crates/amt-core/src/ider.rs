//! IDER: a CD-ROM drive on the device whose disc is an image file here. AMT drives it, sending the ATAPI commands the
//! device's BIOS or OS gives the drive, and we answer them straight from the file. We follow MeshCommander's
//! `amt-ider-ws-0.0.1.js` and Intel's ui-toolkit (both Apache-2.0) in what they send, not in what they meant to: both
//! answer many a failure with success, and AMT is known to boot that way. Not where it contradicts itself, though: the
//! floppy drive AMT has too is empty and says so, and a read past the end of the disc fails. Linux kept resetting the
//! drive (a USB device since AMT 11) over both.

use std::io::SeekFrom;
use std::ops::Range;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::net::{Redirection, Session};
use crate::{Client, Error, Result};

/// A disc in the device's drive, served by `serve`.
pub struct Ider {
    redirection: Redirection,
    image: File,
    /// Its size in 2048-byte blocks.
    blocks: u64,
    /// The most a message to the device may carry.
    chunk: usize,
    sequence: u32,
    /// What of the image the command at hand read, in bytes.
    served: Range<u64>,
    /// The most the command at hand takes back, its allocation length.
    allocation: usize,
    /// Why the CD-ROM's last command failed (sense key, additional sense code and qualifier), for REQUEST SENSE.
    sense: [u8; 3],
}

/// The CD-ROM's ATAPI device id, the floppy drive's is 0xA0.
const CD: u8 = 0xB0;

/// What the device said, as far as serving goes: the rest we answer as we hear it.
enum Heard {
    Nothing,
    Command(Vec<u8>),
    Reset,
    Closed,
}

impl Client {
    /// Puts `image` in the device's CD-ROM drive from its next boot on, turning IDER on first if need be.
    pub async fn ider(&mut self, image: std::fs::File) -> Result<Ider> {
        let image = File::from_std(image);
        let blocks = image.metadata().await?.len() / 2048;
        let redirection = self.redirect(Session::Ider).await?;
        let mut ider =
            Ider { redirection, image, blocks, chunk: 0, sequence: 0, served: 0..0, allocation: 0, sense: [0; 3] };
        // Timeouts of 30 seconds to receive and none to send, a heartbeat every 20 seconds, version 1.
        ider.send(0x40, &[0x30, 0x75, 0, 0, 0x20, 0x4E, 1, 0, 0, 0], false, false).await?;
        let reply = ider.redirection.read_exact(30).await?;
        ider.redirection.read_exact(reply[29].into()).await?;
        ider.chunk = u16::from_le_bytes([reply[16], reply[17]]).into();
        if reply[0] != 0x41 || reply[21] != 0 || !(1..=8192).contains(&ider.chunk) {
            return Err(Error::Protocol(format!("an IDER session we don't speak: {reply:?}")));
        }
        // The drive shows up when the device next boots, not under a running OS.
        ider.send(0x48, &[3, 0x01 | 0x08, 0, 0, 0], false, false).await?;
        Ok(ider)
    }
}

impl Ider {
    /// Answers the device until it closes the session, telling `read` what of the image, in bytes, after every read.
    pub async fn serve(mut self, mut read: impl FnMut(Range<u64>)) -> Result<()> {
        loop {
            let kind = self.redirection.read_exact(1).await?[0];
            match self.hear(kind).await? {
                Heard::Closed => return Ok(()),
                Heard::Command(command) => {
                    let device = if command[6] & 0x10 == 0 { 0xA0 } else { CD };
                    self.command(device, command[6], &command[8..], command[1] & 1 == 1).await?;
                    let served = std::mem::take(&mut self.served);
                    if !served.is_empty() {
                        read(served);
                    }
                }
                Heard::Reset | Heard::Nothing => {}
            }
        }
    }

    /// The rest of a message of `kind` from the device, answered if an answer is all it needs.
    async fn hear(&mut self, kind: u8) -> Result<Heard> {
        self.redirection.read_exact(7).await?; // the rest of the header, its sequence we don't check
        Ok(match kind {
            0x43 => Heard::Closed,
            0x44 => {
                // A ping, we pong.
                self.send(0x45, &[], false, false).await?;
                Heard::Nothing
            }
            0x45 | 0x4B => Heard::Nothing, // a pong, a heartbeat
            0x46 => {
                // The drive was reset, the host resetting its USB device say.
                self.redirection.read_exact(1).await?;
                self.send(0x47, &[], false, false).await?;
                Heard::Reset
            }
            // How the drive was switched on, an error AMT ran into.
            0x49 | 0x4A => {
                self.redirection.read_exact(if kind == 0x49 { 5 } else { 3 }).await?;
                Heard::Nothing
            }
            0x50 => Heard::Command(self.redirection.read_exact(20).await?),
            other => return Err(Error::Protocol(format!("an IDER message we don't know: {other:#04x}"))),
        })
    }

    /// One ATAPI command (`cdb`) for `device`, its `flags` as they came. `dma` is how the device wants its data.
    async fn command(&mut self, device: u8, flags: u8, cdb: &[u8], dma: bool) -> Result<()> {
        let u16_at = |at: usize| u16::from_be_bytes([cdb[at], cdb[at + 1]]);
        // Unlike the references we stop at the allocation length, as SCSI wants: more is an overrun to the host.
        self.allocation = match cdb[0] {
            0x03 | 0x1A => cdb[4].into(),
            0x23 | 0x43 | 0x46 | 0x4A | 0x5A => u16_at(7).into(),
            _ => usize::MAX,
        };
        // REQUEST SENSE: why the last command failed, in fixed format, which it then forgets. The floppy drive is empty.
        if cdb[0] == 0x03 {
            let [key, asc, ascq] = if device == CD { std::mem::take(&mut self.sense) } else { [2, 0x3A, 0] };
            return self.data(device, &[0x70, 0, key, 0, 0, 0, 0, 10, 0, 0, 0, 0, asc, ascq, 0, 0, 0, 0], dma).await;
        }
        if device != CD {
            return self.end(device, 2, 0x3A, 0).await; // medium not present
        }
        self.sense = [0; 3];
        let u32_at = |at: usize| u64::from(u32::from_be_bytes([cdb[at], cdb[at + 1], cdb[at + 2], cdb[at + 3]]));
        match cdb[0] {
            // READ (6), READ (10) and READ (12).
            0x08 | 0x28 | 0xA8 => {
                let (lba, len) = match cdb[0] {
                    // A length of 0 is 256 blocks here.
                    0x08 => (
                        u64::from(cdb[1] & 0x1f) << 16 | u64::from(u16_at(2)),
                        if cdb[4] == 0 { 256 } else { cdb[4].into() },
                    ),
                    0x28 => (u32_at(2), u16_at(7).into()),
                    _ => (u32_at(2), u32_at(6)),
                };
                match len {
                    0 => self.ok(device).await,
                    _ if lba + len > self.blocks => self.end(device, 5, 0x21, 0).await, // out of range
                    _ => self.read(lba, len, dma).await,
                }
            }
            // MODE SENSE (6), all pages: write-protected.
            0x1A if cdb[2] == 0x3F && cdb[3] == 0 => self.data(device, &[0, 5, 0x80, 0], dma).await,
            // READ FORMAT CAPACITIES, the same for any disc.
            0x23 => self.data(device, &[0, 0, 0, 8, 0, 0, 0x0B, 0x40, 2, 0, 2, 0], dma).await,
            // READ CAPACITY: the last block and the block size, addressed as the command was.
            0x25 => {
                let last = u32::try_from(self.blocks.saturating_sub(1)).unwrap_or(u32::MAX).to_be_bytes();
                self.data(flags, &[&last[..], &[0, 0, 8, 0]].concat(), dma).await
            }
            // READ TOC: one data track.
            0x43 => {
                let format = match cdb[2] & 7 {
                    0 => cdb[9] >> 6,
                    format => format,
                };
                match (format, cdb[1] & 2 != 0) {
                    (1, _) => self.data(device, &[0, 0x0A, 1, 1, 0, 0x14, 1, 0, 0, 0, 0, 0], dma).await,
                    (0, true) => self.data(device, &TOC_MSF, dma).await,
                    (0, false) => self.data(device, &TOC, dma).await,
                    // Both references leave other formats unanswered, we say we don't have them.
                    _ => self.end(device, 5, 0x24, 0).await,
                }
            }
            0x46 => self.data(device, &configuration(cdb[1] != 2, u16_at(2)), dma).await,
            // GET EVENT STATUS NOTIFICATION, for media: whether a disc is in.
            0x4A if cdb[1] == 1 || cdb[4] == 0x10 => self.data(device, &[0, 2, 0x80, 0], dma).await,
            // MODE SENSE (10).
            0x5A => match cdb[2] & 0x3F {
                0x01 => self.data(device, &CD_ERROR_RECOVERY, dma).await,
                0x1A => self.data(device, &CD_POWER, dma).await,
                0x1D => self.data(device, &CD_TIMEOUT, dma).await,
                0x2A => self.data(device, &CD_CAPABILITIES, dma).await,
                0x3F => self.data(device, &CD_ALL, dma).await,
                _ => self.end(device, 5, 0x20, 0).await,
            },
            0xAC => self.data(device, &[0, 0, 0, 4, 2, 0, 0, 0], dma).await, // GET PERFORMANCE
            // TEST UNIT READY, MODE SENSE (6) of a page, START STOP, PREVENT ALLOW MEDIUM REMOVAL, events of another
            // class, MODE SELECT (10): both references say they went well.
            0x00 | 0x1A | 0x1B | 0x1E | 0x4A | 0x55 => self.ok(device).await,
            // Anything else, like READ DISC INFORMATION or a write: an invalid command.
            _ => self.end(device, 5, 0x20, 0).await,
        }
    }

    /// Blocks `lba` on, `len` of them, in the largest messages the device takes. Like MeshCommander we listen between
    /// them: once the drive is reset the read is over, and sending on only confuses AMT.
    async fn read(&mut self, lba: u64, len: u64, dma: bool) -> Result<()> {
        self.image.seek(SeekFrom::Start(lba * 2048)).await?;
        self.served = lba * 2048..lba * 2048;
        let mut left = len * 2048;
        let mut buf = vec![0; self.chunk];
        while left > 0 {
            let chunk = &mut buf[..self.chunk.min(left as usize)];
            self.image.read_exact(chunk).await?;
            left -= chunk.len() as u64;
            self.served.end += chunk.len() as u64;
            self.transfer(CD, chunk, left == 0, dma).await?;
            // After the last chunk the read is done, what comes next is the next command. Before it, reading a byte
            // gives up nothing when there's none yet.
            if left == 0 {
                break;
            }
            let mut kind = [0];
            if let Ok(read) = tokio::time::timeout(Duration::ZERO, self.redirection.read(&mut kind)).await {
                if read? == 0 {
                    return Err(Error::Transport("the device closed the connection".into()));
                }
                match self.hear(kind[0]).await? {
                    Heard::Nothing => {}
                    Heard::Reset => return Ok(()),
                    Heard::Closed | Heard::Command(_) => {
                        return Err(Error::Protocol("the device moved on in the middle of a read".into()));
                    }
                }
            }
        }
        Ok(())
    }

    /// What the command asked for, as much of it as it takes, and success.
    async fn data(&mut self, device: u8, data: &[u8], dma: bool) -> Result<()> {
        match &data[..data.len().min(self.allocation)] {
            [] => self.ok(device).await,
            data => self.transfer(device, data, true, dma).await,
        }
    }

    /// Data for the device, `last` ending the command with success.
    async fn transfer(&mut self, device: u8, data: &[u8], last: bool, dma: bool) -> Result<()> {
        let len = (data.len() as u16).to_le_bytes();
        let pio = if dma { [0, 0] } else { len };
        let mut message =
            vec![0, len[0], len[1], 0, if dma { 0xB4 } else { 0xB5 }, 0, 2, 0, pio[0], pio[1], device, 0x58];
        message.extend(if last { [0x85, 0, 3, 0, 0, 0, device, 0x50, 0, 0, 0, 0, 0, 0] } else { [0; 14] });
        message.extend(data);
        self.send(0x54, &message, last, dma).await
    }

    /// The command went well.
    async fn ok(&mut self, device: u8) -> Result<()> {
        let mut message = vec![0; 12];
        message.extend([0xC5, 0, 3, 0, 0, 0, device, 0x50, 0, 0, 0]);
        self.send(0x51, &message, true, false).await
    }

    /// The command failed, with its sense key and additional sense code and qualifier.
    async fn end(&mut self, device: u8, sense: u8, asc: u8, ascq: u8) -> Result<()> {
        if device == CD {
            self.sense = [sense, asc, ascq];
        }
        let mut message = vec![0; 12];
        message.extend([0x87, sense << 4, 3, 0, 0, 0, device, 0x51, sense, asc, ascq]);
        self.send(0x51, &message, true, false).await
    }

    async fn send(&mut self, command: u8, data: &[u8], last: bool, dma: bool) -> Result<()> {
        let attributes = u8::from(last && command > 0x50) << 1 | u8::from(dma);
        let message = [&[command, 0, 0, attributes][..], &self.sequence.to_le_bytes(), data].concat();
        self.sequence += 1;
        self.redirection.write_all(&message).await
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
