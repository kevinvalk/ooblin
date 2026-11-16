use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::Mutex;

use super::Client;
use crate::net::{Redirection, RedirectionSession, Stream, read_exact, write_all};
use crate::{Error, GOODBYE, Result};

// NOTE: SOL is the device's serial port, which AMT carries over a redirection session in framed messages. We took the
// message layouts and settings from MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0).

/// An open serial console. What the device writes and what we type go separate ways, so typing never waits.
pub struct Sol {
    reader: Mutex<ReadHalf<Stream>>,
    writer: Mutex<Writer>,
}

struct Writer {
    half: WriteHalf<Stream>,
    sequence: u32,
    sent_at: Instant, // AMT wants to hear from us every couple of seconds, so we keep when we last sent anything.
}

impl Client {
    pub async fn sol(&mut self) -> Result<Sol> {
        Sol::open(self.redirect(RedirectionSession::Sol).await?).await
    }
}

impl Sol {
    async fn open(mut redirection: Redirection) -> Result<Self> {
        // The settings are our transmit buffer of 10000 bytes, then timeouts in milliseconds: 100 to send, none to send
        // on overflow, 10000 to receive, 100 to flush what we received, and no heartbeat.
        let mut start = vec![0x20, 0, 0, 0, 1, 0, 0, 0];
        for setting in [10000u16, 100, 0, 10000, 100, 0] {
            start.extend(setting.to_le_bytes());
        }
        redirection.write_all(&[&start[..], &[0; 4]].concat()).await?;

        let reply = redirection.read_exact(23).await?;
        if reply[0] != 0x21 || reply[1] != 0 {
            return Err(Error::Protocol(format!("the device refused SOL (status {})", reply[1])));
        }

        // We set the serial port's control lines the way MeshCommander does.
        redirection.write_all(&[0x27, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0x1B, 0, 0, 0]).await?;

        let (reader, half) = redirection.split();
        let writer = Writer { half, sequence: 3, sent_at: Instant::now() };

        Ok(Self { reader: Mutex::new(reader), writer: Mutex::new(writer) })
    }

    /// What the device wrote to its serial port since the last read, or nothing after a second.
    pub async fn read(&self) -> Result<Vec<u8>> {
        let mut reader = self.reader.lock().await;

        // We do this before every read and not only after a quiet one, because a device that keeps writing wants to
        // hear from us too.
        let mut writer = self.writer.lock().await;
        if writer.sent_at.elapsed() > Duration::from_secs(2) {
            writer.send(0x2B, &[]).await?; // A keep-alive
        }
        drop(writer);

        loop {
            // We read a single byte here because that loses nothing when it times out.
            let kind = match tokio::time::timeout(Duration::from_secs(1), reader.read_u8()).await {
                Err(_) => return Ok(Vec::new()),
                Ok(kind) => kind?,
            };
            read_exact(&mut *reader, 7).await?; // Padding and the sequence, which we do not check

            match kind {
                0x2A => {
                    let length = read_exact(&mut *reader, 2).await?;
                    return read_exact(&mut *reader, u16::from_le_bytes([length[0], length[1]]).into()).await;
                }
                // The serial port's control lines, which we have no use for.
                0x29 => {
                    read_exact(&mut *reader, 2).await?;
                }
                0x2B => {} // A keep-alive back
                other => return Err(Error::Protocol(format!("an SOL message we don't know: {other:#04x}"))),
            }
        }
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().await;

        // 10000 bytes is our transmit buffer, the largest message AMT takes (see `open`).
        for chunk in data.chunks(10000) {
            let length = (chunk.len() as u16).to_le_bytes();
            writer.send(0x28, &[&length[..], chunk].concat()).await?;
        }

        Ok(())
    }

    /// We wait for a read that is still on its way (a second at most) and then say goodbye instead of just going, as
    /// `Kvm::close` does.
    pub async fn close(&self) {
        let goodbye = async {
            let _reader = self.reader.lock().await;
            self.writer.lock().await.half.shutdown().await
        };
        let _ = tokio::time::timeout(GOODBYE, goodbye).await;
    }
}

impl Writer {
    async fn send(&mut self, kind: u8, data: &[u8]) -> Result<()> {
        write_all(&mut self.half, &[&[kind, 0, 0, 0][..], &self.sequence.to_le_bytes(), data].concat()).await?;
        (self.sequence, self.sent_at) = (self.sequence + 1, Instant::now());
        Ok(())
    }
}
