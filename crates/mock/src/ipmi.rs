//! The BMC's IPMI on UDP, the fixture's `ipmi.txt` replayed (see `Replay`): its presence pongs, RMCP+ sessions logged
//! in with RAKP as `USER` (SHA-256 or SHA-1 as asked, SHA-1 alone where its cipher suites lack SHA-256), its sensors,
//! event log, FRU, LAN and identity, the chassis and SOL commands as they change, and a serial console that greets with
//! a login prompt and echoes what's typed. Only from this machine, nothing else on the network should find us.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ooblin_core::ipmi::{Change, Client, Hash, Sealing, Session as Device, message, unsealed};
use ooblin_core::{Error, Target};
use tokio::net::{ToSocketAddrs, UdpSocket};

use crate::scrub::fake;
use crate::{Bmc, PASSWORD, USER, hex, receive};

/// A recording's answers: as asked, and its SDR records, SEL entries and FRU 0 put together, so any reads of them,
/// in any chunks, get what the BMC holds. What we change sticks: the chassis's status, LAN and SOL settings, the FRU.
#[derive(Default)]
pub(crate) struct Replay {
    /// By the RMCP sequence number of the presence ping it answers (`ping 00 -> ...`), the pong.
    pongs: HashMap<u8, Vec<u8>>,
    rakp: Rakp,
    /// By request, its function, command and data: the completion code and data.
    answers: HashMap<Vec<u8>, (u8, Vec<u8>)>,
    records: Vec<Part>,
    entries: Vec<Part>,
    fru: Vec<u8>,
    /// What tests have go wrong, see `Bmc::fault`.
    faults: Vec<(String, Fault)>,
}

/// What becomes of a request, for tests of what a BMC or the way to it does at its worst.
pub enum Fault {
    /// Its answer is lost on the way.
    Lost,
    /// Its answer takes until it's asked again, which we refuse as a duplicate (0xCF) first.
    Late,
    /// We refuse it with this completion code rather than do it.
    Refused(u8),
    /// What's typed gets these SOL packets rather than its acknowledgement and echo.
    Answered(Vec<Vec<u8>>),
}

impl Replay {
    pub(crate) fn read(recording: &str) -> Self {
        let mut replay = Self { rakp: IPMI_SIM, ..Self::default() };
        for line in recording.lines().filter(|line| !line.starts_with('#')) {
            let Some((asked, answer)) = line.split_once(" -> ") else { continue };
            if let Some(sequence) = asked.strip_prefix("ping ") {
                replay.pongs.insert(hex(sequence)[0], hex(answer));
                continue;
            }
            if asked == "rakp" {
                let parts: Vec<Vec<u8>> = answer.split(' ').map(hex).collect();
                let id = u32::from_le_bytes(parts[2][..].try_into().unwrap());
                replay.rakp = (parts[0][..].try_into().unwrap(), parts[1][..].try_into().unwrap(), id);
                continue;
            }
            let mut asked = asked.split(' ');
            let (Some(function), Some(command)) = (asked.next(), asked.next()) else { continue };
            let answer = match answer.strip_prefix("ERR ") {
                Some(code) => (hex(code)[0], vec![]),
                None => (0, hex(answer)),
            };
            replay.learn(hex(function)[0], hex(command)[0], &hex(asked.next().unwrap_or_default()), answer);
        }
        replay
    }

    /// Takes what the BMC answered to `command` of `function`: its completion code and data. An answer too short to
    /// be a part of anything is only kept as it is.
    fn learn(&mut self, function: u8, command: u8, data: &[u8], answer: (u8, Vec<u8>)) {
        if answer.0 == 0 {
            match (function, command, data, &answer.1[..]) {
                // An SDR record's part: [next id, its bytes from offset].
                (0x0A, 0x23, &[_, _, low, high, offset, _], _) => {
                    put(&mut self.records, u16::from_le_bytes([low, high]), offset, &answer.1)
                }
                (0x0A, 0x43, _, &[_, _, low, high, ..]) => {
                    put(&mut self.entries, u16::from_le_bytes([low, high]), 0, &answer.1)
                }
                (0x0A, 0x11, &[_, low, high, ..], [_, part @ ..]) => {
                    let at = usize::from(u16::from_le_bytes([low, high]));
                    self.fru.resize(self.fru.len().max(at + part.len()), 0);
                    self.fru[at..at + part.len()].copy_from_slice(part);
                }
                _ => {}
            }
        }
        self.answers.insert(asked(function, command, data), answer);
    }

    /// The fault a test set for a request with this line, which it happens to once.
    fn fault(&mut self, request: &str) -> Option<Fault> {
        let at = self.faults.iter().position(|(start, _)| request.starts_with(start))?;
        Some(self.faults.remove(at).1)
    }

    /// The SDR record (`command` 0x23) or SEL entry `id`, 0 the first.
    fn part(&self, command: u8, id: [u8; 2]) -> Option<&Part> {
        let (list, id) = (if command == 0x23 { &self.records } else { &self.entries }, u16::from_le_bytes(id));
        list.iter().find(|(part, ..)| *part == id || id == 0)
    }

    /// Answers `command` of `function` from the recording, by what's put together where reads may be in any chunks.
    fn answer(&mut self, function: u8, command: u8, data: &[u8]) -> (u8, Vec<u8>) {
        match (function, command, data) {
            // Get SDR, a part: only with the reservation the recording gives, as the record may have changed since.
            (0x0A, 0x23, &[low, high, _, _, offset, _])
                if offset != 0 && self.answers.get(&[0x0A, 0x22][..]).is_none_or(|(_, id)| [low, high] != id[..]) =>
            {
                (0xC5, vec![])
            }
            // Get SDR or Get SEL Entry: [reservation, id, offset, count].
            (0x0A, 0x23 | 0x43, &[_, _, low, high, offset, count]) => {
                let Some((_, next, bytes)) = self.part(command, [low, high]) else { return (0xCB, vec![]) };
                let end =
                    if count == 0xFF { bytes.len() } else { bytes.len().min(usize::from(offset) + usize::from(count)) };
                let part = bytes.get(usize::from(offset)..end).unwrap_or_default();
                (0, [&next.to_le_bytes()[..], part].concat())
            }
            (0x0A, 0x10, _) if !self.fru.is_empty() => (0, [&(self.fru.len() as u16).to_le_bytes()[..], &[0]].concat()),
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
            _ => self.answers.get(&asked(function, command, data)).cloned().unwrap_or((0xC1, vec![])),
        }
    }

    /// Whether we answer `command` of `function` from the recording as the BMC would.
    fn knows(&self, function: u8, command: u8, data: &[u8]) -> bool {
        match (function, command, data) {
            (0x0A, 0x23 | 0x43, &[_, _, low, high, offset, count]) => {
                self.part(command, [low, high]).is_some_and(|(.., bytes)| {
                    (count == 0xFF && !bytes.is_empty()) || bytes.len() >= usize::from(offset) + usize::from(count)
                })
            }
            (0x0A, 0x11, &[_, low, high, count]) => {
                self.fru.len() >= usize::from(u16::from_le_bytes([low, high])) + usize::from(count)
            }
            _ => self.answers.contains_key(&asked(function, command, data)),
        }
    }

    /// Swaps what tells whose BMC this is for fakes of the same shape (see `scrub`): `addresses` (MAC, IPv4 or IPv6, as
    /// our reads wrote them) in the LAN settings, the GUIDs, the user names.
    fn scrub(&mut self, addresses: &[String]) {
        let swaps: Vec<_> = addresses
            .iter()
            .filter_map(|address| Some((bytes(address)?, bytes(&fake(address))?)))
            .filter(|(from, to)| from.len() == to.len() && from.iter().any(|&byte| byte != 0))
            .collect();
        for (asked, (_, answer)) in &mut self.answers {
            match asked[..2] {
                [0x0C, 0x02] => {
                    for (from, to) in &swaps {
                        let mut at = 0;
                        while let Some(found) = answer[at..].windows(from.len()).position(|window| window == from) {
                            at += found;
                            answer[at..at + to.len()].copy_from_slice(to);
                            at += to.len();
                        }
                    }
                }
                [0x06, 0x08 | 0x37] => *answer = hex(&fake(&text(answer))),
                // Get User Name: the name, padded with zeros.
                [0x06, 0x46] => {
                    let end = answer.iter().position(|&byte| byte == 0).unwrap_or(answer.len());
                    if let Ok(name) = std::str::from_utf8(&answer[..end])
                        && name.is_ascii()
                    {
                        let name = fake(name);
                        answer[..end].copy_from_slice(name.as_bytes());
                    }
                }
                _ => {}
            }
        }
    }

    /// The recording, after `kept` (comments, say): every answer, SDR records, SEL entries and the FRU whole.
    fn save(&self, kept: &[&str]) -> String {
        let mut lines: Vec<String> = kept.iter().map(|line| line.to_string()).collect();
        let mut pongs: Vec<_> = self.pongs.iter().collect();
        pongs.sort();
        lines
            .extend(pongs.into_iter().map(|(sequence, pong)| format!("ping {} -> {}", text(&[*sequence]), text(pong))));
        let mut answers: Vec<_> =
            self.answers.iter().filter(|(asked, _)| !matches!(asked[..2], [0x0A, 0x23 | 0x43 | 0x11])).collect();
        answers.sort();
        for (asked, (code, answer)) in answers {
            let answer = if *code == 0 { text(answer) } else { format!("ERR {}", text(&[*code])) };
            lines.push(format!("{} {} {} -> {answer}", text(&asked[..1]), text(&asked[1..2]), text(&asked[2..])));
        }
        for (command, list) in [(0x23, &self.records), (0x43, &self.entries)] {
            for (id, next, bytes) in list {
                let (id, next) = (text(&id.to_le_bytes()), text(&next.to_le_bytes()));
                lines.push(format!("0a {} 0000{id}00ff -> {next}{}", text(&[command]), text(bytes)));
            }
        }
        for (at, part) in (0u16..).step_by(32).zip(self.fru.chunks(32)) {
            let length = text(&[part.len() as u8]);
            lines.push(format!("0a 11 00{}{length} -> {length}{}", text(&at.to_le_bytes()), text(part)));
        }
        lines.join("\n") + "\n"
    }

    /// Whether the BMC has cipher suite 17, SHA-256's: unless the recording lists its suites to the end (Get Channel
    /// Cipher Suites, 16 bytes at a time) without it, as ipmi_sim's does.
    fn sha256(&self) -> bool {
        let parts: Vec<&[u8]> =
            (0x80..0xC0).map_while(|index| self.answers.get(&[0x06, 0x54, 0x0E, 0, index][..])?.1.get(1..)).collect();
        parts.last().is_none_or(|last| last.len() == 16) || parts.concat().windows(2).any(|suite| suite == [0xC0, 17])
    }
}

/// Bytes as a recording writes them, in hex.
fn text(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A request as `Replay::answers` knows it.
fn asked(function: u8, command: u8, data: &[u8]) -> Vec<u8> {
    [&[function, command][..], data].concat()
}

/// What an IPMI message's header, and what follows it, add up to with their checksums.
fn sum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, byte| sum.wrapping_add(*byte))
}

/// A MAC, IPv4 or IPv6 address's bytes.
fn bytes(address: &str) -> Option<Vec<u8>> {
    if let Ok(ipv4) = address.parse::<Ipv4Addr>() {
        return Some(ipv4.octets().to_vec());
    }
    if let Ok(ipv6) = address.parse::<Ipv6Addr>() {
        return Some(ipv6.octets().to_vec());
    }
    let mac: Vec<u8> = address.split(':').map(|byte| u8::from_str_radix(byte, 16).ok()).collect::<Option<_>>()?;
    (mac.len() == 6).then_some(mac)
}

/// An SDR record or SEL entry by its id: the id of the one after it, as the BMC said, and its bytes.
type Part = (u16, u16, Vec<u8>);

/// Puts what `answer` (the next id, then bytes) says of `id` at `offset` among `list`, in the order they first came.
fn put(list: &mut Vec<Part>, id: u16, offset: u8, answer: &[u8]) {
    let &[low, high, ref part @ ..] = answer else { return };
    let next = u16::from_le_bytes([low, high]);
    let at = list.iter().position(|(known, ..)| *known == id).unwrap_or_else(|| {
        list.push((id, next, vec![]));
        list.len() - 1
    });
    let bytes = &mut list[at].2;
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
    /// The sequence number of its last datagram to us: one that isn't past it is a replay.
    heard: u32,
    /// An answer we hold until its request, by its sequence number, comes again: see `Fault::Late`.
    late: Option<(u8, Vec<u8>)>,
    /// Our SOL packets' number while it's active.
    sol: Option<u8>,
    /// Whether it may do more than raise or close: a recording that raises it (Set Session Privilege Level) is of a
    /// BMC that starts it at User, as ipmi_sim's does.
    raised: bool,
}

/// Our random number, GUID and first session id in RAKP, fixed so what ipmitool once sent a BMC gets what that BMC
/// answered (`tests/ipmi.rs`): the recording's (`rakp -> RANDOM GUID ID`, the id as sent), else ipmi_sim's.
type Rakp = ([u8; 16], [u8; 16], u32);
const IPMI_SIM: Rakp = (
    0x202A7304A2A2C470A53651349E49A82D_u128.to_be_bytes(),
    0xA123456789ABCDEFA123456789ABCDEF_u128.to_be_bytes(),
    0x412,
);

impl Bmc {
    /// For tests: `fault` becomes of the next request whose line in `requests` starts with `request`.
    pub fn fault(&self, request: &str, fault: Fault) {
        self.ipmi.lock().unwrap().faults.push((request.into(), fault));
    }

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
            while let Ok((length, from)) = receive(&socket, &mut datagram).await {
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

        // A presence ping, ASF's, and nothing that only looks like one.
        if let &[0x06, 0, sequence, 0x06, 0, 0, 0x11, 0xBE, 0x80, tag, 0, 0] = datagram {
            let pong = self.ipmi.lock().unwrap().pongs.get(&sequence).cloned();
            if let Some(mut pong) = pong.filter(|pong| pong.len() > 9) {
                pong[9] = tag;
                reply(pong).await;
            }
            return;
        }

        if self.ipmi_off && self.ipmi_device.get().is_none() {
            return;
        }

        let (random, guid, first) = self.ipmi.lock().unwrap().rakp;
        if datagram.len() < 16 || datagram[4] != 6 {
            return;
        }
        let kind = datagram[5] & 0x3F;
        let payload = &datagram[16..];
        // What's too short to be the setup message it says it is, we drop.
        let least = match kind {
            0x10 => 32,
            0x12 => 28,
            0x14 => 8,
            _ => 0,
        };
        if payload.len() < least {
            return;
        }

        match kind {
            // Open Session: we take SHA-256 or SHA-1 with AES, as asked, nothing else.
            0x10 => {
                let (sha256, raised) = {
                    let replay = self.ipmi.lock().unwrap();
                    (replay.sha256(), !replay.answers.contains_key(&[0x06, 0x3B, 4][..]))
                };
                let hash = [Hash::Sha256, Hash::Sha1].into_iter().find(|hash| {
                    let (authentication, integrity) = hash.algorithms();
                    (sha256 || *hash == Hash::Sha1)
                        && (payload[12], payload[20], payload[28]) == (authentication, integrity, 1)
                });
                let id = sessions.keys().max().map_or(first, |id| id + 1);
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
                            heard: 0,
                            late: None,
                            sol: None,
                            raised,
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
                    &[&ours, &id.to_le_bytes(), &session.console, &random, &guid, &session.user],
                );
                let answer = [&[payload[0], 0, 0, 0][..], &ours, &random, &guid, &code].concat();
                reply(unsealed(0x13, &answer)).await;
            }
            // RAKP 3, answered by 4: a code from another password gets status 0x0F.
            0x14 => {
                let id = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                let Some(session) = sessions.get_mut(&id) else { return };
                let key = PASSWORD.as_bytes();
                let ours = session.ours.to_le_bytes();
                if payload[8..] != session.hash.sign(key, &[&random, &ours, &session.user]) {
                    reply(unsealed(0x15, &[payload[0], 0x0F, 0, 0, 0, 0, 0, 0])).await;
                    sessions.remove(&id);
                    return;
                }
                let sik = session.hash.sign(key, &[&session.console, &random, &session.user]);
                let length = session.hash.code_length();
                let check = session.hash.sign(&sik, &[&session.console, &id.to_le_bytes(), &guid]);
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
                // As a BMC does, we drop what came before: someone sending it again.
                let heard = u32::from_le_bytes(datagram[10..14].try_into().unwrap());
                if heard <= session.heard {
                    return;
                }
                session.heard = heard;

                let (mut answers, mut closed) = (Vec::new(), false);
                match kind {
                    // An IPMI message whose two checksums hold, as a BMC drops any other.
                    0 if payload.len() >= 7 && sum(&payload[..3]) == 0 && sum(&payload[3..]) == 0 => {
                        let (function, sequence, command) = (payload[1] >> 2, payload[4] >> 2, payload[5]);
                        let data = &payload[6..payload.len() - 1];
                        let request = format!("IPMI {function:02x} {command:02x} {data:02x?}");
                        let fault = self.ipmi.lock().unwrap().fault(&request);
                        self.requests.lock().unwrap().push(request);

                        // Asked again what we answer late: a duplicate, then that answer.
                        let late = session.late.take_if(|(late, _)| *late == sequence);
                        let refusal = match (&late, &fault) {
                            (Some(_), _) => Some(0xCF),
                            (_, Some(Fault::Refused(code))) => Some(*code),
                            _ => None,
                        };
                        let (code, answer, sol) = match (refusal, function, command, data) {
                            (Some(code), ..) => (code, vec![], None),
                            // Set Session Privilege Level: 0 asks for the present one, User's (2) until raised.
                            (_, 0x06, 0x3B, &[0]) => (0, vec![if session.raised { 4 } else { 2 }], None),
                            (_, 0x06, 0x3B, &[level]) => {
                                session.raised |= level == 4;
                                (0, vec![level], None)
                            }
                            // Any session may end itself: Close Session's privilege is Callback, the lowest.
                            _ if session.raised || (function, command) == (0x06, 0x3C) => {
                                self.forward(function, command, data).await;
                                self.command(function, command, data, port, session.sol.is_some())
                            }
                            _ => (0xD4, vec![], None),
                        };

                        let answer = [&[code][..], &answer].concat();
                        let answer = message(0x81, function + 1, 0x20, sequence, command, &answer);
                        match fault {
                            Some(Fault::Lost) => {}
                            Some(Fault::Late) => session.late = Some((sequence, answer)),
                            _ => answers.push((0, answer)),
                        }
                        answers.extend(late.map(|(_, late)| (0, late)));

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
                        let request = format!("SOL {:?}", String::from_utf8_lossy(typed));
                        let fault = self.ipmi.lock().unwrap().fault(&request);
                        self.requests.lock().unwrap().push(request);

                        if let Some(Fault::Answered(packets)) = fault {
                            answers.extend(packets.into_iter().map(|packet| (1, packet)));
                        } else {
                            answers.push((1, vec![0, payload[0], typed.len() as u8, 0]));
                            if let Some(sequence) = &mut session.sol {
                                *sequence = *sequence % 15 + 1;
                                let echo = String::from_utf8_lossy(typed).replace('\r', "\r\n");
                                answers.push((1, [&[*sequence, 0, 0, 0][..], echo.as_bytes()].concat()));
                            }
                        }
                    }
                    // Its acknowledgement of a packet of ours: which, and how many of its characters.
                    1 if payload.len() == 4 && payload[0] == 0 => {
                        self.requests.lock().unwrap().push(format!("SOL ack {:02x?}", &payload[1..3]));
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

    /// Recording: asks the BMC what we can't answer yet and keeps what it says. What would change the BMC we answer
    /// ourselves, so recording is read-only.
    async fn forward(&self, function: u8, command: u8, data: &[u8]) {
        let Some(device) = self.ipmi_device.get() else { return };
        let ours = matches!(
            (function, command),
            (0x06, 0x3C | 0x48 | 0x49) | (0x00, 0x02 | 0x04 | 0x06 | 0x08) | (0x0C, 0x01 | 0x21) | (0x0A, 0x12)
        );
        if ours || self.ipmi.lock().unwrap().knows(function, command, data) {
            return;
        }
        let answer = match device.request(function, command, data).await {
            Ok(answer) => (0, answer),
            Err(Error::Refused(code)) => (code, vec![]),
            Err(_) => return,
        };
        self.ipmi.lock().unwrap().learn(function, command, data, answer);
    }

    /// Answers an IPMI command: its completion code, its data, and whether it activates (`true`) or deactivates SOL.
    fn command(&self, function: u8, command: u8, data: &[u8], port: u16, sol: bool) -> (u8, Vec<u8>, Option<bool>) {
        let mut replay = self.ipmi.lock().unwrap();
        // Get Chassis Status's answer, which the chassis's commands change.
        let status = replay.answers.get_mut(&[0x00, 0x01][..]).map(|(_, status)| status);
        match (function, command, data) {
            (0x06, 0x3C, _) => (0, vec![], None),
            // Activate Payload: SOL already active in this session is code 0x80.
            (0x06, 0x48, _) if sol => (0x80, vec![], None),
            (0x06, 0x48, _) => {
                let answer = [&[0, 0, 0, 0, 0xFF, 0, 0xFF, 0][..], &port.to_le_bytes(), &[0xFF, 0xFF]].concat();
                (0, answer, Some(true))
            }
            (0x06, 0x49, _) => (0, vec![], Some(false)),
            // Chassis Control: down, up, cycle, reset, NMI, soft off. An NMI changes nothing.
            (0x00, 0x02, &[control, ..]) => {
                if let Some(status) = status
                    && control != 4
                {
                    status[0] = status[0] & !1 | u8::from(matches!(control, 1..=3));
                }
                (0, vec![], None)
            }
            // Chassis Identify: lit until turned off (2), or off.
            (0x00, 0x04, _) => {
                if let Some(status) = status {
                    status[2] = status[2] & !0x30 | if data.get(1) == Some(&1) { 0x20 } else { 0 };
                }
                (0, vec![], None)
            }
            (0x00, 0x06, &[policy, ..]) => {
                if let Some(status) = status {
                    status[0] = status[0] & !0x60 | (policy & 0x03) << 5;
                }
                (0, vec![0x07], None)
            }
            // Set System Boot Options, which we only take.
            (0x00, 0x08, _) => (0, vec![], None),
            // Set LAN or SOL Configuration Parameters: what's set reads back.
            (0x0C, 0x01 | 0x21, _) => {
                if let &[channel, parameter, ref value @ ..] = data
                    && parameter != 0
                {
                    let asked = [&[0x0C, command + 1, channel, parameter][..], &[0, 0]].concat();
                    replay.answers.insert(asked, (0, [&[0x11][..], value].concat()));
                }
                (0, vec![], None)
            }
            // Too short for what it sets.
            (0x00, 0x02 | 0x06, _) => (0xC7, vec![], None),
            _ => {
                let (code, answer) = replay.answer(function, command, data);
                (code, answer, None)
            }
        }
    }
}

/// Records `target`'s IPMI into `fixture`'s `ipmi.txt`: what our client reads of it, through the mock asking the BMC
/// what it can't answer yet, scrubbed. The file's comments and `rakp` line stay, a login's isn't ours to record.
pub async fn record(target: &Target, fixture: &Path) -> anyhow::Result<()> {
    eprint!("   IPMI  recording…");
    let device = Device::open(target).await?;
    let mut replay = Replay { pongs: pongs(target).await?, ..Replay::read("") };
    // Set Session Privilege Level 0 asks for the present one: a BMC that starts sessions below Administrator is raised.
    if device.request(0x06, 0x3B, &[0]).await? != [4] {
        let raised = device.request(0x06, 0x3B, &[4]).await?;
        replay.learn(0x06, 0x3B, &[4], (0, raised));
    }
    // Get Channel Cipher Suites, 16 at a time, which our login doesn't ask: till a part's short, or refused.
    for index in 0x80..0xC0 {
        let Ok(suites) = device.request(0x06, 0x54, &[0x0E, 0, index]).await else { break };
        let last = suites.len() < 17;
        replay.learn(0x06, 0x54, &[0x0E, 0, index], (0, suites));
        if last {
            break;
        }
    }

    let proxy = Bmc::start("127.0.0.1:0", fixture).await;
    *proxy.ipmi.lock().unwrap() = replay;
    assert!(proxy.ipmi_device.set(device).is_ok());
    let client = Client::new(&proxy.target(PASSWORD));
    client.info().await?;
    client.power().await?;
    client.panel().await?;
    client.sensors().await?;
    client.event_log().await?;
    client.settings().await?;
    let status = client.status().await?;

    let mut fru = client.fru().await?;
    let read = fru.clone();
    let serials = fru.chassis.iter_mut().map(|chassis| &mut chassis.serial_number);
    let serials = serials.chain(fru.board.iter_mut().map(|board| &mut board.serial_number));
    for value in serials.chain(fru.product.iter_mut().flat_map(|p| [&mut p.serial_number, &mut p.asset_tag])) {
        *value = fake(value);
    }
    if fru != read {
        client.change(Change::Fru(fru)).await?;
    }
    let ports = status.ports.iter().flat_map(|port| {
        let ipv4 = &port.ipv4;
        let ipv6 = port
            .ipv6
            .iter()
            .flat_map(|v6| v6.addresses.iter().chain([&v6.gateway, &v6.primary_dns, &v6.secondary_dns]));
        [&port.mac, &ipv4.address, &ipv4.gateway, &ipv4.primary_dns, &ipv4.secondary_dns].into_iter().chain(ipv6)
    });
    let addresses: Vec<String> = ports.cloned().collect();
    proxy.ipmi_device.get().unwrap().close().await;

    let file = fixture.join("ipmi.txt");
    let old = std::fs::read_to_string(&file).unwrap_or_default();
    let kept: Vec<&str> = old.lines().filter(|line| line.starts_with('#') || line.starts_with("rakp ")).collect();
    let recording = {
        let mut replay = proxy.ipmi.lock().unwrap();
        replay.scrub(&addresses);
        replay.save(&kept)
    };
    std::fs::create_dir_all(fixture)?;
    std::fs::write(&file, &recording)?;
    eprintln!("\r   IPMI  {} lines", recording.lines().count());
    Ok(())
}

/// The BMC's pongs to presence pings by their RMCP sequence number, 0 and 255: some drop either.
async fn pongs(target: &Target) -> std::io::Result<HashMap<u8, Vec<u8>>> {
    let address =
        tokio::net::lookup_host((target.ipmi_host(), target.ipmi_port)).await?.min_by_key(SocketAddr::is_ipv6);
    let address = address.ok_or_else(|| std::io::Error::other(format!("no address for {}", target.ipmi_host())))?;
    let socket = UdpSocket::bind(if address.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }).await?;
    socket.connect(address).await?;
    let mut pongs = HashMap::new();
    for sequence in [0, 0xFF] {
        socket.send(&[0x06, 0, sequence, 0x06, 0, 0, 0x11, 0xBE, 0x80, 0, 0, 0]).await?;
        let mut pong = [0; 64];
        if let Ok(Ok(length)) = tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut pong)).await {
            pongs.insert(sequence, pong[..length].to_vec());
        }
    }
    Ok(pongs)
}
