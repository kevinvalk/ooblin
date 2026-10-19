//! SOL: the device's serial port, which AMT carries over a redirection session in framed messages. Message layouts and
//! settings from MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0).

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::Mutex;

use crate::net::{Redirection, Session, Stream};
use crate::{Client, Error, Result};

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
    /// Opens the device's serial console, turning SOL on first if need be.
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
        loop {
            // Reading a byte gives up nothing when it times out.
            let mut kind = [0];
            match tokio::time::timeout(Duration::from_secs(1), reader.read(&mut kind)).await {
                Err(_) => {
                    let mut writer = self.writer.lock().await;
                    if writer.sent.elapsed() > Duration::from_secs(2) {
                        writer.send(0x2B, &[]).await?; // keep-alive
                    }
                    return Ok(Vec::new());
                }
                Ok(Ok(0)) => return Err(Error::Transport("the device closed the connection".into())),
                Ok(read) => read?,
            };
            let mut header = [0; 7]; // padding and the sequence, which we don't check
            reader.read_exact(&mut header).await?;
            match kind[0] {
                0x2A => {
                    let len = reader.read_u16_le().await?;
                    let mut data = vec![0; len.into()];
                    reader.read_exact(&mut data).await?;
                    return Ok(data);
                }
                // The serial port's control lines.
                0x29 => {
                    reader.read_exact(&mut [0; 2]).await?;
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

    /// Ends the session once a read still on its way is in (a second at most), saying goodbye rather than just going.
    pub async fn close(&self) {
        let _reader = self.reader.lock().await;
        let _ = self.writer.lock().await.half.shutdown().await;
    }
}

impl Writer {
    /// TLS may hold on to what we write until we flush.
    async fn send(&mut self, kind: u8, data: &[u8]) -> Result<()> {
        let message = [&[kind, 0, 0, 0][..], &self.sequence.to_le_bytes(), data].concat();
        self.half.write_all(&message).await?;
        self.half.flush().await?;
        (self.sequence, self.sent) = (self.sequence + 1, Instant::now());
        Ok(())
    }
}
