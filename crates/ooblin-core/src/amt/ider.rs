//! IDER: a CD-ROM drive on the device whose disc is an image file here. AMT drives it, sending the ATAPI commands the
//! device's BIOS or OS gives the drive, which `Cdrom` answers. The floppy drive AMT has too is empty and says so: Linux
//! kept resetting the drive (a USB device since AMT 11) when it didn't, as MeshCommander's doesn't.

use std::ops::Range;
use std::time::Duration;

use super::Client;
use crate::cdrom::{Answer, Cdrom};
use crate::net::{Redirection, Session};
use crate::{Error, Result};

/// A disc in the device's drive, served by `serve`.
pub struct Ider {
    redirection: Redirection,
    cdrom: Cdrom,
    /// The most a message to the device may carry.
    chunk: usize,
    sequence: u32,
    /// What of the image the command at hand read, in bytes.
    served: Range<u64>,
}

/// The drives' ATAPI device ids.
const CD: u8 = 0xB0;
const FLOPPY: u8 = 0xA0;

/// What the device said, as far as serving goes: the rest we answer as we hear it.
enum Heard {
    Nothing,
    Command(Vec<u8>),
    Reset,
    Closed,
}

impl Client {
    /// Puts `image` in the device's CD-ROM drive from its next boot on, where IDER is on.
    pub async fn ider(&mut self, image: std::fs::File) -> Result<Ider> {
        let cdrom = Cdrom::new(image).await?;
        let redirection = self.redirect(Session::Ider).await?;
        let mut ider = Ider { redirection, cdrom, chunk: 0, sequence: 0, served: 0..0 };

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
    /// Answers the device until it closes the session, or `eject` does, telling `read` what of the image, in bytes,
    /// after every read.
    pub async fn serve(mut self, read: impl FnMut(Range<u64>), eject: impl Future<Output = ()>) -> Result<()> {
        tokio::select! {
            served = self.answer(read) => served,
            () = eject => Ok(()),
        }
    }

    async fn answer(&mut self, mut read: impl FnMut(Range<u64>)) -> Result<()> {
        loop {
            let kind = self.redirection.read_u8().await?;
            match self.hear(kind).await? {
                Heard::Closed => return Ok(()),
                Heard::Command(command) => {
                    self.command(command[6], &command[8..], command[1] & 1 == 1).await?;
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

    /// One ATAPI command (`cdb`) for the drive `flags` address, which come back as they came. `dma` is how the device
    /// wants its data.
    async fn command(&mut self, flags: u8, cdb: &[u8], dma: bool) -> Result<()> {
        // The floppy drive is empty.
        if flags & 0x10 == 0 {
            return match cdb[0] {
                0x03 => {
                    let sense = [0x70, 0, 2, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x3A, 0, 0, 0, 0, 0];
                    self.transfer(FLOPPY, &sense[..sense.len().min(cdb[4].into())], true, dma).await
                }
                _ => self.end(FLOPPY, 2, 0x3A, 0).await,
            };
        }
        let answer = match cdb[0] {
            // AMT answers INQUIRY itself.
            0x12 => self.cdrom.fail(5, 0x20, 0),
            _ => self.cdrom.answer(cdb),
        };
        match answer {
            // READ CAPACITY's answer goes back addressed as the command was.
            Answer::Data(data) => self.transfer(if cdb[0] == 0x25 { flags } else { CD }, &data, true, dma).await,
            Answer::Read { lba, blocks } => self.read(lba, blocks, dma).await,
            Answer::Done => self.ok(CD).await,
            Answer::Failed([key, asc, ascq]) => self.end(CD, key, asc, ascq).await,
        }
    }

    /// Blocks `lba` on, `blocks` of them, in the largest messages the device takes. Like MeshCommander we listen between
    /// them: once the drive is reset the read is over, and sending on only confuses AMT.
    async fn read(&mut self, lba: u64, blocks: u64, dma: bool) -> Result<()> {
        self.served = lba * 2048..lba * 2048;
        let mut left = blocks * 2048;
        let mut buf = vec![0; self.chunk];
        while left > 0 {
            let chunk = &mut buf[..self.chunk.min(usize::try_from(left).unwrap_or(usize::MAX))];
            self.cdrom.read(self.served.end, chunk).await?;
            left -= chunk.len() as u64;
            self.served.end += chunk.len() as u64;
            self.transfer(CD, chunk, left == 0, dma).await?;
            // After the last chunk the read is done, what comes next is the next command. Before it, reading a byte
            // gives up nothing when there's none yet.
            if left == 0 {
                break;
            }
            if let Ok(kind) = tokio::time::timeout(Duration::ZERO, self.redirection.read_u8()).await {
                match self.hear(kind?).await? {
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
        let mut message = vec![0; 12];
        message.extend([0x87, sense << 4, 3, 0, 0, 0, device, 0x51, sense, asc, ascq]);
        self.send(0x51, &message, true, false).await
    }

    async fn send(&mut self, command: u8, data: &[u8], last: bool, dma: bool) -> Result<()> {
        let attributes = u8::from(last) << 1 | u8::from(dma);
        let message = [&[command, 0, 0, attributes][..], &self.sequence.to_le_bytes(), data].concat();
        self.sequence += 1;
        self.redirection.write_all(&message).await
    }
}
