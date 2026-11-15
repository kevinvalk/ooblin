//! SOL: the device's serial port, which AMT carries over a redirection session in framed messages. Message layouts and
//! settings from MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0).

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::Mutex;

use super::Client;
use crate::net::{Redirection, Session, Stream, read_exact, write_all};
use crate::{Error, GOODBYE, Result};

/// An open serial console. What the device writes and what we type go separate ways, so typing never waits.
pub struct Sol {
    reader: Mutex<ReadHalf<Stream>>,
    writer: Mutex<Writer>,
}

struct Writer {
    half: WriteHalf<Stream>,
    sequence: u32,
    /// When we last sent anything: AMT wants to hear from us every couple of seconds.
    sent: Instant,
}

impl Client {
    /// Opens the device's serial console, where SOL is on.
    pub async fn sol(&mut self) -> Result<Sol> {
        Sol::open(self.redirect(Session::Sol).await?).await
    }
}

impl Sol {
    async fn open(mut redirection: Redirection) -> Result<Self> {
        // Our transmit buffer of 10000 bytes, then timeouts in milliseconds: to send 100, to send on overflow none, to
        // receive 10000, to flush what we received 100, and no heartbeat.
        let mut start = vec![0x20, 0, 0, 0, 1, 0, 0, 0];
        for setting in [10000u16, 100, 0, 10000, 100, 0] {
            start.extend(setting.to_le_bytes());
        }
        redirection.write_all(&[&start[..], &[0; 4]].concat()).await?;
        let reply = redirection.read_exact(23).await?;
        if reply[0] != 0x21 || reply[1] != 0 {
            return Err(Error::Protocol(format!("the device refused SOL (status {})", reply[1])));
        }
        // The serial port's control lines, as MeshCommander sets them.
        redirection.write_all(&[0x27, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0x1B, 0, 0, 0]).await?;
        let (reader, half) = redirection.split();
        let writer = Writer { half, sequence: 3, sent: Instant::now() };
        Ok(Self { reader: Mutex::new(reader), writer: Mutex::new(writer) })
    }

    /// What the device wrote to its serial port since, or nothing after a second.
    pub async fn read(&self) -> Result<Vec<u8>> {
        let mut reader = self.reader.lock().await;
        // Before every read, not only after a quiet one: a device that keeps writing wants to hear from us too.
        let mut writer = self.writer.lock().await;
        if writer.sent.elapsed() > Duration::from_secs(2) {
            writer.send(0x2B, &[]).await?; // keep-alive
        }
        drop(writer);

        loop {
            // Reading a byte gives up nothing when it times out.
            let kind = match tokio::time::timeout(Duration::from_secs(1), reader.read_u8()).await {
                Err(_) => return Ok(Vec::new()),
                Ok(kind) => kind?,
            };
            read_exact(&mut *reader, 7).await?; // padding and the sequence, which we don't check
            match kind {
                0x2A => {
                    let len = read_exact(&mut *reader, 2).await?;
                    return read_exact(&mut *reader, u16::from_le_bytes([len[0], len[1]]).into()).await;
                }
                // The serial port's control lines.
                0x29 => {
                    read_exact(&mut *reader, 2).await?;
                }
                0x2B => {} // a keep-alive back
                other => return Err(Error::Protocol(format!("an SOL message we don't know: {other:#04x}"))),
            }
        }
    }

    /// Types `data` on the serial port, in messages as large as AMT takes.
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().await;
        for chunk in data.chunks(10000) {
            let len = (chunk.len() as u16).to_le_bytes();
            writer.send(0x28, &[&len[..], chunk].concat()).await?;
        }
        Ok(())
    }

    /// Ends the session once a read still on its way is in (a second at most), saying goodbye rather than just going,
    /// as `Kvm::close` does.
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
        (self.sequence, self.sent) = (self.sequence + 1, Instant::now());
        Ok(())
    }
}
