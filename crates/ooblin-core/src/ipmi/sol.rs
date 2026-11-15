//! SOL: the server's serial port over IPMI, carried in numbered packets each side acknowledges (IPMI 2.0, chapter 15
//! and 24).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::{APPLICATION, Session};
use crate::net::ipmi::{MESSAGE, SOL, message};
use crate::{Error, GOODBYE, Result, Target};

/// An open serial console. What the server writes and what we type go separate ways, so typing never waits on reading.
pub struct Sol {
    session: Session,
    /// The most a packet to the BMC carries, after its 4-byte header.
    most: usize,
    state: Mutex<State>,
    acknowledged: Notify,
    /// One `read` at a time, which `close` waits for: it would take the answers `close` waits for. And one `write`,
    /// its packets in order.
    reading: tokio::sync::Mutex<()>,
    writing: tokio::sync::Mutex<()>,
}

struct State {
    /// Our last packet's number, 1 to 15, and its characters while the BMC hasn't taken them all, when we last sent
    /// them and how often we sent them again without a word from the BMC.
    sequence: u8,
    unacknowledged: Option<(Vec<u8>, Instant, u8)>,
    /// The BMC's last packet's number and how many characters it had, as it sends one again until we acknowledge it,
    /// with what the server wrote meanwhile after them.
    received: (u8, u8),
    /// When we last sent anything: the session ends after a minute or so of silence.
    sent: Instant,
}

impl State {
    /// Our next packet, of `data`, which stays unacknowledged until the BMC took it.
    fn next(&mut self, data: Vec<u8>) -> Vec<u8> {
        self.sequence = self.sequence % 15 + 1;
        let packet = [&[self.sequence, 0, 0, 0][..], &data].concat();
        self.unacknowledged = Some((data, Instant::now(), 0));
        packet
    }
}

/// Activating SOL as an encrypted, signed payload whose serial alerts wait meanwhile.
const ACTIVATE: [u8; 6] = [SOL, 1, 0xC4, 0, 0, 0];
const DEACTIVATE: [u8; 6] = [SOL, 1, 0, 0, 0, 0];

impl Sol {
    /// Opens the server's serial console, taking it over from whoever else has it: the BMC lets one console have it.
    /// A session that doesn't get it is closed again.
    pub async fn open(target: &Target) -> Result<Self> {
        let session = Session::open(target).await?;
        match Self::activate(&session, target).await {
            Ok(most) => {
                let state = State { sequence: 0, unacknowledged: None, received: (0, 0), sent: Instant::now() };
                let (acknowledged, reading, writing) = Default::default();
                Ok(Self { session, most, state: Mutex::new(state), acknowledged, reading, writing })
            }
            Err(error) => {
                session.close().await;
                Err(error)
            }
        }
    }

    /// Activates SOL in `session` and says the most a packet to the BMC carries.
    async fn activate(session: &Session, target: &Target) -> Result<usize> {
        let activated = match session.request(APPLICATION, 0x48, &ACTIVATE).await {
            Err(Error::Refused(0x80)) => {
                session.request(APPLICATION, 0x49, &DEACTIVATE).await?;
                session.request(APPLICATION, 0x48, &ACTIVATE).await
            }
            activated => activated,
        };
        let activated = match activated {
            Ok(activated) if activated.len() >= 10 => activated,
            Ok(_) => return Err(Error::Protocol("a short SOL activation".into())),
            // Its payload type is turned off.
            Err(Error::Refused(0x81)) => return Err(Error::Off),
            Err(error) => return Err(error),
        };
        let port = u16::from_le_bytes([activated[8], activated[9]]);
        if port != target.ipmi_port {
            return Err(Error::Protocol(format!("the BMC wants SOL on port {port}")));
        }
        Ok(usize::from(u16::from_le_bytes([activated[4], activated[5]])).clamp(5, 255) - 4)
    }

    /// What the server wrote to its serial port since, or nothing after a second. Meanwhile we acknowledge what came,
    /// send again what the BMC didn't take, and keep the session alive.
    pub async fn read(&self) -> Result<Vec<u8>> {
        let _reading = self.reading.lock().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        // A BMC that keeps sending what holds nothing new doesn't keep us past the second.
        while tokio::time::Instant::now() < deadline {
            self.tend().await?;
            let Ok(received) = tokio::time::timeout_at(deadline, self.session.receive()).await else { break };
            let (kind, packet) = received?;
            // Both numbers are 4 bits, the rest reserved.
            let [sequence, acknowledges, accepted, status] = match packet.get(..4) {
                Some(&[a, b, c, d]) if kind == SOL => [a & 0x0F, b & 0x0F, c, d],
                _ => continue,
            };
            if status & 0x10 != 0 {
                return Err(Error::Ended("another console took the serial port, or SOL was turned off"));
            }
            if acknowledges != 0 {
                self.acknowledge(acknowledges, accepted, status & 0x40 != 0).await?;
            }
            if sequence == 0 {
                continue;
            }
            let data = &packet[4..];
            let count = data.len() as u8;
            self.send(&[0, sequence, count, 0]).await?;
            // Sent again, as our acknowledgement was lost: we have its first characters already.
            let had = match std::mem::replace(&mut self.state.lock().unwrap().received, (sequence, count)) {
                (last, had) if last == sequence => usize::from(had).min(data.len()),
                _ => 0,
            };
            if had < data.len() {
                return Ok(data[had..].to_vec());
            }
        }
        Ok(Vec::new())
    }

    /// Types `data` on the serial port, a packet at a time, each once the BMC took the one before.
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let _writing = self.writing.lock().await;
        for chunk in data.chunks(self.most) {
            tokio::time::timeout(Duration::from_secs(20), async {
                while self.state.lock().unwrap().unacknowledged.is_some() {
                    self.acknowledged.notified().await;
                }
            })
            .await
            .map_err(|_| Error::Timeout)?;
            let packet = self.state.lock().unwrap().next(chunk.to_vec());
            self.send(&packet).await?;
        }
        Ok(())
    }

    /// Deactivates SOL and ends the session, within `GOODBYE` each, once the read underway is done.
    pub async fn close(&self) {
        let _reading = self.reading.lock().await;
        let _ = tokio::time::timeout(GOODBYE, self.session.request(APPLICATION, 0x49, &DEACTIVATE)).await;
        self.session.close().await;
    }

    /// The BMC took `accepted` characters of our last packet, none if it says `nack`. What it didn't take goes again,
    /// as a packet of its own at once, or all of it as `tend` sends it: one that takes nothing would have us send
    /// without end.
    async fn acknowledge(&self, acknowledges: u8, accepted: u8, nack: bool) -> Result<()> {
        let rest = {
            let mut state = self.state.lock().unwrap();
            if acknowledges != state.sequence {
                return Ok(());
            }
            if nack || accepted == 0 {
                if let Some((.., resends)) = state.unacknowledged.as_mut() {
                    *resends = 0;
                }
                return Ok(());
            }
            let Some((mut data, ..)) = state.unacknowledged.take() else { return Ok(()) };
            data.drain(..usize::from(accepted).min(data.len()));
            if data.is_empty() {
                self.acknowledged.notify_one();
                return Ok(());
            }
            state.next(data)
        };
        self.send(&rest).await
    }

    /// Sends again what wasn't acknowledged within a second, giving up after 7 resends that got no answer at all (the
    /// most SOL's own retry count goes to), and tells the BMC we're here after 20 silent seconds.
    async fn tend(&self) -> Result<()> {
        let (again, quiet) = {
            let mut state = self.state.lock().unwrap();
            let sequence = state.sequence;
            let again = state.unacknowledged.as_mut().filter(|(_, sent, _)| sent.elapsed() > Duration::from_secs(1));
            let again = match again {
                Some((_, _, 7)) => return Err(Error::Timeout),
                Some((data, sent, resends)) => {
                    (*sent, *resends) = (Instant::now(), *resends + 1);
                    Some([&[sequence, 0, 0, 0][..], data].concat())
                }
                None => None,
            };
            (again, state.sent.elapsed() > Duration::from_secs(20))
        };
        if let Some(again) = again {
            self.send(&again).await?;
        } else if quiet {
            // Get Device ID, whose answer `read` drops.
            self.session.send(MESSAGE, &message(0x20, APPLICATION, 0x81, 0, 0x01, &[])).await?;
            self.state.lock().unwrap().sent = Instant::now();
        }
        Ok(())
    }

    async fn send(&self, packet: &[u8]) -> Result<()> {
        self.state.lock().unwrap().sent = Instant::now();
        self.session.send(SOL, packet).await
    }
}
