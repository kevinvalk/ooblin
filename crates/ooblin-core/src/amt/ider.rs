use std::ops::Range;
use std::time::Duration;

use super::Client;
use crate::cdrom::{Answer, Cdrom};
use crate::net::{Redirection, RedirectionSession};
use crate::{Error, Result};

// NOTE: IDER is a CD-ROM drive on the device whose disc is an image file here. AMT drives it by sending us the ATAPI
// commands the device's BIOS or OS gives the drive, which `Cdrom` answers.

pub struct Ider {
    redirection: Redirection,
    cdrom: Cdrom,
    chunk_length: usize, // The most bytes a message to the device may carry
    sequence: u32,
    served: Range<u64>, // The bytes of the image that the command at hand read
}

// The ATAPI device ids of the two drives AMT has.
const CD: u8 = 0xB0;
const FLOPPY: u8 = 0xA0;

// What the device said, as far as `answer` and `read` need to know. We answer the rest as we hear it.
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
        let redirection = self.redirect(RedirectionSession::Ider).await?;
        let mut ider = Ider { redirection, cdrom, chunk_length: 0, sequence: 0, served: 0..0 };

        // We ask for timeouts of 30 seconds to receive and none to send, a heartbeat every 20 seconds, and version 1.
        ider.send(0x40, &[0x30, 0x75, 0, 0, 0x20, 0x4E, 1, 0, 0, 0], false, false).await?;

        let reply = ider.redirection.read_exact(30).await?;
        ider.redirection.read_exact(reply[29].into()).await?;
        ider.chunk_length = u16::from_le_bytes([reply[16], reply[17]]).into();
        if reply[0] != 0x41 || reply[21] != 0 || !(1..=8192).contains(&ider.chunk_length) {
            return Err(Error::Protocol(format!("an IDER session we don't speak: {reply:?}")));
        }

        // We switch the drive on for the next boot, so it does not show up under a running OS.
        ider.send(0x48, &[3, 0x01 | 0x08, 0, 0, 0], false, false).await?;

        Ok(ider)
    }
}

impl Ider {
    /// Answers the device until it closes the session, or `eject` does. After every read we tell `read` which bytes
    /// of the image it was.
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

    /// Reads the rest of a message of `kind` from the device, and answers it when an answer is all it needs.
    async fn hear(&mut self, kind: u8) -> Result<Heard> {
        self.redirection.read_exact(7).await?; // The rest of the header, we do not check its sequence

        Ok(match kind {
            0x43 => Heard::Closed,
            // A ping, which we answer with a pong.
            0x44 => {
                self.send(0x45, &[], false, false).await?;
                Heard::Nothing
            }
            0x45 | 0x4B => Heard::Nothing, // A pong and a heartbeat
            // The drive was reset, by the host resetting its USB device say.
            0x46 => {
                self.redirection.read_exact(1).await?;
                self.send(0x47, &[], false, false).await?;
                Heard::Reset
            }
            // How the drive was switched on (0x49) and an error AMT ran into (0x4A), neither of which we use.
            0x49 | 0x4A => {
                self.redirection.read_exact(if kind == 0x49 { 5 } else { 3 }).await?;
                Heard::Nothing
            }
            0x50 => Heard::Command(self.redirection.read_exact(20).await?),
            other => return Err(Error::Protocol(format!("an IDER message we don't know: {other:#04x}"))),
        })
    }

    /// One ATAPI command (`cdb`) for the drive that `flags` addresses. `is_dma` is how the device wants its data.
    async fn command(&mut self, flags: u8, cdb: &[u8], is_dma: bool) -> Result<()> {
        // The floppy drive AMT has too is empty and we say so. MeshCommander's does not, and Linux kept resetting the
        // drive (a USB device since AMT 11) when ours did not either!
        if flags & 0x10 == 0 {
            return match cdb[0] {
                0x03 => {
                    let sense = [0x70, 0, 2, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x3A, 0, 0, 0, 0, 0];
                    self.transfer(FLOPPY, &sense[..sense.len().min(cdb[4].into())], true, is_dma).await
                }
                _ => self.end(FLOPPY, 2, 0x3A, 0).await,
            };
        }

        let answer = match cdb[0] {
            0x12 => self.cdrom.fail(5, 0x20, 0), // AMT answers INQUIRY itself, so we refuse one that still reaches us.
            _ => self.cdrom.answer(cdb),
        };

        match answer {
            // NOTE: The answer to READ CAPACITY goes back addressed as the command was, the others as the CD.
            Answer::Data(data) => self.transfer(if cdb[0] == 0x25 { flags } else { CD }, &data, true, is_dma).await,
            Answer::Read { lba, blocks } => self.read(lba, blocks, is_dma).await,
            Answer::Done => self.ok(CD).await,
            Answer::Failed([key, asc, ascq]) => self.end(CD, key, asc, ascq).await,
        }
    }

    /// Sends `blocks` blocks from `lba` on, in the largest messages the device takes. Like MeshCommander we listen
    /// between them: once the drive is reset the read is over, and sending on only confuses AMT.
    async fn read(&mut self, lba: u64, blocks: u64, is_dma: bool) -> Result<()> {
        self.served = lba * 2048..lba * 2048;
        let mut left = blocks * 2048;
        let mut buffer = vec![0; self.chunk_length];

        while left > 0 {
            let chunk = &mut buffer[..self.chunk_length.min(usize::try_from(left).unwrap_or(usize::MAX))];
            self.cdrom.read(self.served.end, chunk).await?;
            left -= chunk.len() as u64;
            self.served.end += chunk.len() as u64;
            self.transfer(CD, chunk, left == 0, is_dma).await?;

            // After the last chunk the read is done and what comes next is the next command, so we do not listen.
            if left == 0 {
                break;
            }

            // We read a single byte without waiting, because that loses nothing when the device said nothing yet.
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

    /// Sends `data` to the device, where `is_last` ends the command with success.
    async fn transfer(&mut self, device: u8, data: &[u8], is_last: bool, is_dma: bool) -> Result<()> {
        let length = (data.len() as u16).to_le_bytes();
        let pio = if is_dma { [0, 0] } else { length };
        let mut message =
            vec![0, length[0], length[1], 0, if is_dma { 0xB4 } else { 0xB5 }, 0, 2, 0, pio[0], pio[1], device, 0x58];
        message.extend(if is_last { [0x85, 0, 3, 0, 0, 0, device, 0x50, 0, 0, 0, 0, 0, 0] } else { [0; 14] });
        message.extend(data);

        self.send(0x54, &message, is_last, is_dma).await
    }

    /// Tells the device the command went well.
    async fn ok(&mut self, device: u8) -> Result<()> {
        let mut message = vec![0; 12];
        message.extend([0xC5, 0, 3, 0, 0, 0, device, 0x50, 0, 0, 0]);
        self.send(0x51, &message, true, false).await
    }

    /// Tells the device the command failed, with its sense key and additional sense code and qualifier.
    async fn end(&mut self, device: u8, sense: u8, asc: u8, ascq: u8) -> Result<()> {
        let mut message = vec![0; 12];
        message.extend([0x87, sense << 4, 3, 0, 0, 0, device, 0x51, sense, asc, ascq]);
        self.send(0x51, &message, true, false).await
    }

    async fn send(&mut self, command: u8, data: &[u8], is_last: bool, is_dma: bool) -> Result<()> {
        let attributes = u8::from(is_last) << 1 | u8::from(is_dma);
        let message = [&[command, 0, 0, attributes][..], &self.sequence.to_le_bytes(), data].concat();
        self.sequence += 1;

        self.redirection.write_all(&message).await
    }
}
