//! The BMC's IPMI on UDP, with its presence pongs: RMCP+ sessions logged in with RAKP as `USER` (either suite we ask
//! for), and the fixture's `ipmi.txt` replayed (see `Replay`): its sensors, event log, FRU, LAN and identity, the
//! chassis and SOL commands as they change, and a serial console that greets with a login prompt and echoes what's
//! typed. Only from this machine, nothing else on the network should find us.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use ooblin_core::ipmi::{Hash, Sealing, message, unsealed};
use tokio::net::{ToSocketAddrs, UdpSocket};

use crate::{Bmc, PASSWORD, USER};

/// What the X10 answers a presence ping with, its tag set to the ping's: IPMI, no version.
const PONG: [u8; 28] =
    [0x06, 0, 0xFF, 0x06, 0, 0, 0x11, 0xBE, 0x40, 0, 0, 0x10, 0, 0, 0x11, 0xBE, 0, 0, 0, 0, 0x81, 0, 0, 0, 0, 0, 0, 0];

/// What our commands change, starting as the X10's: on, power restore "previous" (1), SOL at 115200 (10); and what
/// the fixture says.
pub(crate) struct State {
    pub on: bool,
    pub identify: bool,
    pub restore: u8,
    pub sol_rate: u8,
    replay: Replay,
}

impl State {
    pub(crate) fn new(fixture: &Path) -> Self {
        let replay = Replay::new(&std::fs::read_to_string(fixture.join("ipmi.txt")).unwrap_or_default());
        Self { on: true, identify: false, restore: 1, sol_rate: 10, replay }
    }
}

/// A recording's answers: as asked, and its SDR records, SEL entries and FRU 0 put together, so any reads of them,
/// in any chunks, get what the BMC holds. Writes to the FRU stick.
#[derive(Default)]
struct Replay {
    /// By request, its function, command and data: the completion code and data.
    answers: HashMap<Vec<u8>, (u8, Vec<u8>)>,
    records: Vec<(u16, Vec<u8>)>,
    entries: Vec<(u16, Vec<u8>)>,
    fru: Vec<u8>,
}

impl Replay {
    fn new(recording: &str) -> Self {
        let mut replay = Self::default();
        let hex = |text: &str| {
            (0..text.len())
                .step_by(2)
                .filter_map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
                .collect::<Vec<u8>>()
        };
        for line in recording.lines().filter(|line| !line.starts_with('#')) {
            let Some((asked, answer)) = line.split_once(" -> ") else { continue };
            let mut asked = asked.split(' ');
            let (Some(function), Some(command)) = (asked.next(), asked.next()) else { continue };
            let (function, command) = (hex(function)[0], hex(command)[0]);
            let data = hex(asked.next().unwrap_or_default());
            let answer = match answer.strip_prefix("ERR ") {
                Some(code) => (hex(code)[0], vec![]),
                None => (0, hex(answer)),
            };
            if answer.0 == 0 {
                match (function, command) {
                    // An SDR record's part: [next id, its bytes from offset].
                    (0x0A, 0x23) => {
                        put(&mut replay.records, u16::from_le_bytes([data[2], data[3]]), data[4], &answer.1[2..])
                    }
                    (0x0A, 0x43) => {
                        put(&mut replay.entries, u16::from_le_bytes([answer.1[2], answer.1[3]]), 0, &answer.1[2..])
                    }
                    (0x0A, 0x11) => {
                        let at = usize::from(u16::from_le_bytes([data[1], data[2]]));
                        let part = &answer.1[1..];
                        replay.fru.resize(replay.fru.len().max(at + part.len()), 0);
                        replay.fru[at..at + part.len()].copy_from_slice(part);
                    }
                    _ => {}
                }
            }
            replay.answers.insert([&[function, command][..], &data].concat(), answer);
        }
        replay
    }

    /// Answers `command` of `function` from the recording, by what's put together where reads may be in any chunks.
    fn answer(&mut self, function: u8, command: u8, data: &[u8]) -> (u8, Vec<u8>) {
        match (function, command, data) {
            (0x0A, 0x22 | 0x42, _) => (0, vec![0x14, 0]),
            // Get SDR or Get SEL Entry: [reservation, id, offset, count], id 0 the first.
            (0x0A, 0x23 | 0x43, &[_, _, low, high, offset, count]) => {
                let list = if command == 0x23 { &self.records } else { &self.entries };
                let id = u16::from_le_bytes([low, high]);
                let Some(at) = list.iter().position(|(record, _)| *record == id || id == 0) else {
                    return (0xCB, vec![]);
                };
                let next = list.get(at + 1).map_or(0xFFFF, |(next, _)| *next);
                let bytes = &list[at].1;
                let end =
                    if count == 0xFF { bytes.len() } else { bytes.len().min(usize::from(offset) + usize::from(count)) };
                let part = bytes.get(usize::from(offset)..end).unwrap_or_default();
                (0, [&next.to_le_bytes()[..], part].concat())
            }
            (0x0A, 0x10, _) => (0, [&(self.fru.len() as u16).to_le_bytes()[..], &[0]].concat()),
            (0x0A, 0x11, &[0, low, high, count]) => {
                let at = usize::from(u16::from_le_bytes([low, high]));
                let part = self.fru.get(at..(at + usize::from(count)).min(self.fru.len())).unwrap_or_default();
                (0, [&[part.len() as u8][..], part].concat())
            }
            (0x0A, 0x12, &[0, low, high, ref written @ ..]) => {
                let at = usize::from(u16::from_le_bytes([low, high]));
                match self.fru.get_mut(at..at + written.len()) {
                    Some(place) => {
                        place.copy_from_slice(written);
                        (0, vec![written.len() as u8])
                    }
                    None => (0xC9, vec![]),
                }
            }
            _ => self.answers.get(&[&[function, command][..], data].concat()).cloned().unwrap_or((0xC1, vec![])),
        }
    }
}

/// Puts `part` of `id`'s bytes at `offset` among `list`, in the order they first came.
fn put(list: &mut Vec<(u16, Vec<u8>)>, id: u16, offset: u8, part: &[u8]) {
    let at = list.iter().position(|(known, _)| *known == id).unwrap_or_else(|| {
        list.push((id, vec![]));
        list.len() - 1
    });
    let bytes = &mut list[at].1;
    let offset = usize::from(offset);
    bytes.resize(bytes.len().max(offset + part.len()), 0);
    bytes[offset..offset + part.len()].copy_from_slice(part);
}

/// A session, by the id we gave it: whose it is, and once RAKP is done how its datagrams are sealed.
struct Session {
    from: SocketAddr,
    ours: u32,
    hash: Hash,
    console: [u8; 16],
    user: Vec<u8>,
    sealing: Option<Sealing>,
    sequence: u32,
    /// Our SOL packets' number while it's active.
    sol: Option<u8>,
}

/// Our random number and GUID in RAKP, fixed: we're no BMC to break into.
const RANDOM: [u8; 16] = [7; 16];
const GUID: [u8; 16] = [9; 16];

impl Bmc {
    /// Answers presence pings and IPMI on `address` too: UDP 623, for the app.
    pub async fn serve_ipmi(self: &Arc<Self>, address: impl ToSocketAddrs) -> std::io::Result<()> {
        self.answer_ipmi(UdpSocket::bind(address).await?);
        Ok(())
    }

    pub(crate) fn answer_ipmi(self: &Arc<Self>, socket: UdpSocket) {
        let port = socket.local_addr().unwrap().port();
        let bmc = self.clone();
        tokio::spawn(async move {
            let mut sessions = HashMap::new();
            let mut datagram = [0; 1024];
            while let Ok((length, from)) = socket.recv_from(&mut datagram).await {
                if from.ip().is_loopback() {
                    bmc.ipmi_datagram(&socket, port, &mut sessions, &datagram[..length], from).await;
                }
            }
        });
    }

    async fn ipmi_datagram(
        &self,
        socket: &UdpSocket,
        port: u16,
        sessions: &mut HashMap<u32, Session>,
        datagram: &[u8],
        from: SocketAddr,
    ) {
        let reply = |datagram: Vec<u8>| async move { socket.send_to(&datagram, from).await.unwrap() };
        if datagram.len() == 12 && datagram[3] == 0x06 {
            let mut pong = PONG;
            pong[9] = datagram[9];
            reply(pong.to_vec()).await;
            return;
        }
        if datagram.len() < 16 || datagram[4] != 6 {
            return;
        }
        let kind = datagram[5] & 0x3F;
        let payload = &datagram[16..];
        // What's too short to be the setup message it says it is, we drop.
        if payload.len()
            < [(0x10, 32), (0x12, 28), (0x14, 8)]
                .iter()
                .find(|(setup, _)| *setup == kind)
                .map_or(0, |(_, length)| *length)
        {
            return;
        }
        match kind {
            // Open Session: we take SHA-256 or SHA-1 with AES, as asked, nothing else.
            0x10 => {
                let hash = [Hash::Sha256, Hash::Sha1].into_iter().find(|hash| {
                    let (authentication, integrity, _) = hash.algorithms();
                    (payload[12], payload[20], payload[28]) == (authentication, integrity, 1)
                });
                let id = sessions.keys().max().map_or(0x100, |id| id + 1);
                let mut answer =
                    [&[payload[0], 0x11, 4, 0][..], &payload[4..8], &id.to_le_bytes(), &payload[8..32]].concat();
                match hash {
                    Some(hash) => {
                        let ours = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                        let session = Session {
                            from,
                            ours,
                            hash,
                            console: [0; 16],
                            user: vec![],
                            sealing: None,
                            sequence: 1,
                            sol: None,
                        };
                        sessions.insert(id, session);
                        answer[1] = 0;
                    }
                    None => answer.truncate(8),
                }
                reply(unsealed(0x11, &answer)).await;
            }
            // RAKP 1, answered by 2: a user of another name gets status 0x0D.
            0x12 => {
                let id = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                let Some(session) = sessions.get_mut(&id) else { return };
                session.console = payload[8..24].try_into().unwrap();
                session.user = [&payload[24..25], &payload[27..28], &payload[28..]].concat();
                let name = &payload[28..];
                if name != USER.as_bytes() {
                    reply(unsealed(0x13, &[payload[0], 0x0D, 0, 0, 0, 0, 0, 0])).await;
                    return;
                }
                let ours = session.ours.to_le_bytes();
                let code = session.hash.sign(
                    PASSWORD.as_bytes(),
                    &[&ours, &id.to_le_bytes(), &session.console, &RANDOM, &GUID, &session.user],
                );
                let answer = [&[payload[0], 0, 0, 0][..], &ours, &RANDOM, &GUID, &code].concat();
                reply(unsealed(0x13, &answer)).await;
            }
            // RAKP 3, answered by 4: a code from another password gets status 0x0F.
            0x14 => {
                let id = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                let Some(session) = sessions.get_mut(&id) else { return };
                let key = PASSWORD.as_bytes();
                let ours = session.ours.to_le_bytes();
                if payload[8..] != session.hash.sign(key, &[&RANDOM, &ours, &session.user]) {
                    reply(unsealed(0x15, &[payload[0], 0x0F, 0, 0, 0, 0, 0, 0])).await;
                    sessions.remove(&id);
                    return;
                }
                let sik = session.hash.sign(key, &[&session.console, &RANDOM, &session.user]);
                let length = session.hash.algorithms().2;
                let check = session.hash.sign(&sik, &[&session.console, &id.to_le_bytes(), &GUID]);
                session.sealing = Some(Sealing::new(session.hash, &sik));
                reply(unsealed(0x15, &[&[payload[0], 0, 0, 0][..], &ours, &check[..length]].concat())).await;
            }
            _ => {
                let id = u32::from_le_bytes(datagram[6..10].try_into().unwrap());
                let Some(session) = sessions.get_mut(&id).filter(|session| session.from == from) else { return };
                let Some((kind, _, payload)) = session.sealing.as_ref().and_then(|sealing| sealing.open(datagram))
                else {
                    return;
                };
                let (mut answers, mut closed) = (Vec::new(), false);
                match kind {
                    0 if payload.len() >= 7 => {
                        let (function, sequence, command) = (payload[1] >> 2, payload[4] >> 2, payload[5]);
                        let data = &payload[6..payload.len() - 1];
                        self.requests.lock().unwrap().push(format!("IPMI {function:02x} {command:02x} {data:02x?}"));
                        let (code, answer, sol) = self.command(function, command, data, port, session.sol.is_some());
                        let answer = [&[code][..], &answer].concat();
                        answers.push((0, message(0x81, function + 1, 0x20, sequence, command, &answer)));
                        match (function, command, sol) {
                            (0x06, 0x3C, _) => closed = true,
                            (_, _, Some(true)) => {
                                session.sol = Some(1);
                                let login = b"\r\nUbuntu 24.04 LTS mock ttyS1\r\n\r\nmock login: ";
                                answers.push((1, [&[1, 0, 0, 0][..], login].concat()));
                            }
                            (_, _, Some(false)) => session.sol = None,
                            _ => {}
                        }
                    }
                    // SOL: we acknowledge what's typed, then echo it as a terminal does.
                    1 if payload.len() > 4 && payload[0] != 0 => {
                        let typed = &payload[4..];
                        self.requests.lock().unwrap().push(format!("SOL {:?}", String::from_utf8_lossy(typed)));
                        answers.push((1, vec![0, payload[0], typed.len() as u8, 0]));
                        if let Some(sequence) = &mut session.sol {
                            *sequence = *sequence % 15 + 1;
                            let echo = String::from_utf8_lossy(typed).replace('\r', "\r\n");
                            answers.push((1, [&[*sequence, 0, 0, 0][..], echo.as_bytes()].concat()));
                        }
                    }
                    _ => {}
                }
                for (kind, answer) in answers {
                    session.sequence += 1;
                    let sealing = session.sealing.as_ref().unwrap();
                    reply(sealing.seal(kind, session.ours, session.sequence, &answer)).await;
                }
                if closed {
                    sessions.remove(&id);
                }
            }
        }
    }

    /// Answers an IPMI command: its completion code, its data, and whether it activates (`true`) or deactivates SOL.
    fn command(&self, function: u8, command: u8, data: &[u8], port: u16, sol: bool) -> (u8, Vec<u8>, Option<bool>) {
        let mut state = self.ipmi.lock().unwrap();
        match (function, command) {
            (0x06, 0x3C) => (0, vec![], None),
            // Activate Payload: SOL already active in this session is code 0x80.
            (0x06, 0x48) if sol => (0x80, vec![], None),
            (0x06, 0x48) => {
                let answer = [&[0, 0, 0, 0, 0xFF, 0, 0xFF, 0][..], &port.to_le_bytes(), &[0xFF, 0xFF]].concat();
                (0, answer, Some(true))
            }
            (0x06, 0x49) => (0, vec![], Some(false)),
            // Get Chassis Status: the identify light's state reported.
            (0x00, 0x01) => {
                let power = u8::from(state.on) | state.restore << 5;
                (0, vec![power, 0, 0x40 | u8::from(state.identify) << 5, 0], None)
            }
            // Chassis Control: down, up, cycle, reset, NMI, soft off.
            (0x00, 0x02) => {
                // An NMI changes nothing.
                state.on = match data.first() {
                    Some(4) => state.on,
                    control => matches!(control, Some(1..=3)),
                };
                (0, vec![], None)
            }
            (0x00, 0x04) => {
                state.identify = data.get(1) == Some(&1);
                (0, vec![], None)
            }
            (0x00, 0x06) => {
                state.restore = data[0];
                (0, vec![0x07], None)
            }
            // Set System Boot Options, which we only take.
            (0x00, 0x08) => (0, vec![], None),
            // Set LAN Configuration Parameters: what's set reads back.
            (0x0C, 0x01) => {
                if let &[channel, parameter, ref value @ ..] = data
                    && parameter != 0
                {
                    let asked = [&[0x0C, 0x02, channel, parameter][..], &[0, 0]].concat();
                    state.replay.answers.insert(asked, (0, [&[0x11][..], value].concat()));
                }
                (0, vec![], None)
            }
            // SOL's settings: enabled, and its bit rate both lasting and now.
            (0x0C, 0x22) => (0, vec![0x11, if data[1] == 1 { 1 } else { state.sol_rate }], None),
            (0x0C, 0x21) => {
                state.sol_rate = data[2];
                (0, vec![], None)
            }
            _ => {
                let (code, answer) = state.replay.answer(function, command, data);
                (code, answer, None)
            }
        }
    }
}
