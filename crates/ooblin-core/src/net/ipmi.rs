//! IPMI 2.0 over LAN (RMCP+, UDP 623), which every BMC speaks whoever made it: a session per `Session::open`,
//! authenticated with RAKP and the password, every message after signed and encrypted. We ask for cipher suite 17
//! (SHA-256), and 3 (SHA-1) from BMCs without it, AES-128 either way: never a suite without both. Layouts from the IPMI
//! 2.0 specification, sections 13.6 to 13.32.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};
use std::time::Duration;

use aws_lc_rs::cipher::{AES_128, DecryptingKey, DecryptionContext, EncryptingKey, UnboundCipherKey};
use aws_lc_rs::iv::FixedLength;
use aws_lc_rs::{hmac, rand};
use tokio::net::UdpSocket;

use crate::{Error, GOODBYE, Result, Target};

/// The payload types we send and get: an IPMI message, SOL's characters, and the session's setup.
pub const MESSAGE: u8 = 0;
pub const SOL: u8 = 1;
const OPEN: u8 = 0x10;
const RAKP1: u8 = 0x12;
const RAKP3: u8 = 0x14;

/// The privilege we ask for: settings need an administrator, as the BMC's own pages do.
const ADMINISTRATOR: u8 = 4;

/// A BMC answers at once or not at all, a datagram may be lost on the way: we send each request three times at most.
const PATIENCE: Duration = Duration::from_secs(1);

/// An open session, its datagrams to and from one BMC.
pub struct Session {
    socket: UdpSocket,
    /// The BMC's id for it, which our datagrams carry, and ours, which its carry.
    id: u32,
    ours: u32,
    sealing: Sealing,
    sequence: AtomicU32,
    /// The IPMI requests' own sequence, six bits, which answers carry back.
    request: AtomicU8,
}

/// A suite's hash: for RAKP's codes and the integrity of every datagram after.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hash {
    Sha1,
    Sha256,
}

impl Hash {
    /// Cipher suite 17's and 3's numbers for authentication and integrity, and its integrity code's length.
    pub fn algorithms(self) -> (u8, u8, usize) {
        match self {
            Self::Sha256 => (3, 4, 16),
            Self::Sha1 => (1, 1, 12),
        }
    }

    pub fn sign(self, key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
        let algorithm = match self {
            Self::Sha256 => hmac::HMAC_SHA256,
            Self::Sha1 => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        };
        let mut context = hmac::Context::with_key(&hmac::Key::new(algorithm, key));
        parts.iter().for_each(|part| context.update(part));
        context.sign().as_ref().to_vec()
    }
}

/// How a session's datagrams are signed and encrypted, the same both ways: the mock BMC's too.
pub struct Sealing {
    hash: Hash,
    /// K1, which signs, and K2's first 16 bytes, which encrypt.
    integrity: Vec<u8>,
    encryption: [u8; 16],
}

impl Sealing {
    /// From the session integrity key RAKP agreed on.
    pub fn new(hash: Hash, sik: &[u8]) -> Self {
        let integrity = hash.sign(sik, &[&[1; 20]]);
        let encryption = hash.sign(sik, &[&[2; 20]])[..16].try_into().unwrap();
        Self { hash, integrity, encryption }
    }

    /// A datagram of `payload`, encrypted and signed, for the session the other side calls `id`.
    pub fn seal(&self, kind: u8, id: u32, sequence: u32, payload: &[u8]) -> Vec<u8> {
        // Padded to whole AES blocks with 1, 2, 3… and how many, then encrypted behind a random IV.
        let pad = 15 - payload.len() % 16;
        let mut encrypted = [payload, &(1..=pad as u8).collect::<Vec<_>>(), &[pad as u8]].concat();
        let key = EncryptingKey::cbc(UnboundCipherKey::new(&AES_128, &self.encryption).unwrap()).unwrap();
        let iv = key.encrypt(&mut encrypted).unwrap();
        let encrypted = [<&[u8]>::try_from(&iv).unwrap(), &encrypted].concat();
        let mut datagram = [&[6, 0, 0xFF, 7, 6, 0xC0 | kind][..], &id.to_le_bytes(), &sequence.to_le_bytes()].concat();
        datagram.extend((encrypted.len() as u16).to_le_bytes());
        datagram.extend(encrypted);
        // From the authentication type to the next header a multiple of 4 long, padded with 0xFF.
        let pad = (4 - (datagram.len() - 4 + 2) % 4) % 4;
        datagram.extend(std::iter::repeat_n(0xFF, pad));
        datagram.extend([pad as u8, 7]);
        let code = self.hash.sign(&self.integrity, &[&datagram[4..]]);
        datagram.extend(&code[..self.hash.algorithms().2]);
        datagram
    }

    /// A sealed datagram's payload type, session id and payload, if it is one and is authentic.
    pub fn open(&self, datagram: &[u8]) -> Option<(u8, u32, Vec<u8>)> {
        let length = self.hash.algorithms().2;
        if datagram.len() < 16 + length || datagram[4] != 6 || datagram[5] & 0xC0 != 0xC0 {
            return None;
        }
        let (signed, code) = datagram.split_at(datagram.len() - length);
        if self.hash.sign(&self.integrity, &[&signed[4..]])[..length] != *code {
            return None;
        }
        let size = u16::from_le_bytes([datagram[14], datagram[15]]) as usize;
        let encrypted = datagram.get(16..16 + size).filter(|_| size >= 32 && size.is_multiple_of(16))?;
        let (iv, encrypted) = encrypted.split_at(16);
        let key = DecryptingKey::cbc(UnboundCipherKey::new(&AES_128, &self.encryption).unwrap()).unwrap();
        let mut payload = encrypted.to_vec();
        let iv = DecryptionContext::Iv128(FixedLength::from(<[u8; 16]>::try_from(iv).unwrap()));
        let pad = *key.decrypt(&mut payload, iv).ok()?.last()? as usize;
        payload.truncate(payload.len().checked_sub(pad + 1)?);
        let id = u32::from_le_bytes(datagram[6..10].try_into().unwrap());
        Some((datagram[5] & 0x3F, id, payload))
    }
}

/// RAKP's parts both sides sign: the role asked for (by name, not slot), and the user's name with its length.
fn rakp_user(user: &str) -> Vec<u8> {
    [&[0x10 | ADMINISTRATOR, user.len() as u8][..], user.as_bytes()].concat()
}

/// A session setup datagram, which goes unsealed.
pub fn unsealed(kind: u8, payload: &[u8]) -> Vec<u8> {
    [&[6, 0, 0xFF, 7, 6, kind][..], &[0; 8], &(payload.len() as u16).to_le_bytes(), payload].concat()
}

/// An IPMI message's bytes from `from` to `to` (0x20 the BMC, 0x81 us), behind its header and checksums.
pub fn message(to: u8, function: u8, from: u8, sequence: u8, command: u8, data: &[u8]) -> Vec<u8> {
    let sum = |bytes: &[u8]| bytes.iter().fold(0u8, |sum, byte| sum.wrapping_sub(*byte));
    let header = [to, function << 2];
    let body = [&[from, sequence << 2, command][..], data].concat();
    [&header[..], &[sum(&header)], &body, &[sum(&body)]].concat()
}

impl Session {
    /// Logs in to the BMC at `target`'s host, with SHA-256 if it can. A BMC that says no to the user or the role, or
    /// signs with another password than ours, is a wrong user or password, which we don't try again.
    pub async fn open(target: &Target) -> Result<Self> {
        // A host naming its web port (`host:port`, `[v6]:port`) says nothing of IPMI's.
        let host = match target.host.rsplit_once(':') {
            Some((host, port)) if port.parse::<u16>().is_ok() && (host.ends_with(']') || !host.contains(':')) => host,
            _ => &target.host,
        };
        let socket = UdpSocket::bind(if host.contains(':') { "[::]:0" } else { "0.0.0.0:0" }).await?;
        socket.connect((host.trim_matches(['[', ']']), target.ipmi_port)).await?;
        match Self::handshake(&socket, target, Hash::Sha256).await {
            // No such authentication or integrity algorithm, or no suite with them: an older BMC.
            Err(Error::Refused(0x04..=0x07 | 0x10..=0x12)) => Self::handshake(&socket, target, Hash::Sha1).await,
            session => session,
        }
        .map(|(id, ours, sealing)| Self {
            socket,
            id,
            ours,
            sealing,
            sequence: AtomicU32::new(1),
            request: AtomicU8::new(0),
        })
    }

    async fn handshake(socket: &UdpSocket, target: &Target, hash: Hash) -> Result<(u32, u32, Sealing)> {
        let random = |bytes: &mut [u8]| rand::fill(bytes).map_err(|_| Error::Transport("no randomness".into()));
        let mut ours = [0; 4];
        random(&mut ours)?;
        let (authentication, integrity, length) = hash.algorithms();
        let algorithm = |kind: u8, algorithm: u8| [kind, 0, 0, 8, algorithm, 0, 0, 0];
        let request = [
            &[0, ADMINISTRATOR, 0, 0][..],
            &ours,
            &algorithm(0, authentication),
            &algorithm(1, integrity),
            &algorithm(2, 1),
        ]
        .concat();
        let response = exchange(socket, OPEN, &request).await?;
        let short = || Error::Protocol("a short RAKP message".into());
        let id: [u8; 4] = response.get(8..12).ok_or_else(short)?.try_into().unwrap();

        let mut console = [0; 16];
        random(&mut console)?;
        let user = rakp_user(&target.user);
        let rakp1 = [&[0; 4][..], &id, &console, &user[..1], &[0, 0], &user[1..]].concat();
        let rakp2 = exchange(socket, RAKP1, &rakp1).await?;
        let (bmc, guid) = (rakp2.get(8..24).ok_or_else(short)?, rakp2.get(24..40).ok_or_else(short)?);
        let key = target.password.as_bytes();
        if rakp2.get(40..) != Some(&hash.sign(key, &[&ours, &id, &console, bmc, guid, &user])) {
            return Err(Error::Unauthorized);
        }
        let sik = hash.sign(key, &[&console, bmc, &user]);
        let rakp3 = [&[0; 4][..], &id, &hash.sign(key, &[bmc, &ours, &user])].concat();
        let rakp4 = exchange(socket, RAKP3, &rakp3).await?;
        if rakp4.get(8..) != Some(&hash.sign(&sik, &[&console, &id, guid])[..length]) {
            return Err(Error::Protocol("the BMC's last RAKP message doesn't check out".into()));
        }
        Ok((u32::from_le_bytes(id), u32::from_le_bytes(ours), Sealing::new(hash, &sik)))
    }

    /// Asks the BMC `command` of `function` (0x00 chassis, 0x06 application, 0x0C transport) and says the answer's
    /// data, after its completion code: one that isn't 0 is the error.
    pub async fn request(&self, function: u8, command: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.ask(function, command, data, 3, PATIENCE).await
    }

    /// Like `request`, sent only once and waited for longer: for what mustn't happen twice on a BMC that doesn't tell
    /// a resend from a new request, a reset say.
    pub async fn request_once(&self, function: u8, command: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.ask(function, command, data, 1, 5 * PATIENCE).await
    }

    /// Sends a request up to `tries` times, each answer waited for `patience`. A resend the BMC says it already did
    /// (0xCF) went through.
    async fn ask(&self, function: u8, command: u8, data: &[u8], tries: u8, patience: Duration) -> Result<Vec<u8>> {
        let sequence = self.request.fetch_add(1, Relaxed) & 0x3F;
        for resend in 0..tries {
            self.send(MESSAGE, &message(0x20, function, 0x81, sequence, command, data)).await?;
            let answer = async {
                loop {
                    let (kind, payload) = self.receive().await?;
                    if kind == MESSAGE && payload.len() >= 8 && payload[4] >> 2 == sequence && payload[5] == command {
                        return Ok::<_, Error>(payload);
                    }
                }
            };
            if let Ok(answer) = tokio::time::timeout(patience, answer).await {
                let answer = answer?;
                return match answer[6] {
                    0 => Ok(answer[7..answer.len() - 1].to_vec()),
                    0xCF if resend > 0 => Ok(Vec::new()),
                    code => Err(Error::Refused(code)),
                };
            }
        }
        Err(Error::Timeout)
    }

    /// Sends a payload of `kind` without waiting for anything back.
    pub async fn send(&self, kind: u8, payload: &[u8]) -> Result<()> {
        let datagram = self.sealing.seal(kind, self.id, self.sequence.fetch_add(1, Relaxed), payload);
        self.socket.send(&datagram).await?;
        Ok(())
    }

    /// The next authentic payload of this session and its type, waiting for it as long as it takes: whatever else
    /// arrives we drop.
    pub async fn receive(&self) -> Result<(u8, Vec<u8>)> {
        let mut datagram = [0; 1024];
        loop {
            let length = match self.socket.recv(&mut datagram).await {
                // Windows says so when an earlier datagram was refused.
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => continue,
                received => received?,
            };
            if let Some((kind, id, payload)) = self.sealing.open(&datagram[..length])
                && id == self.ours
            {
                return Ok((kind, payload));
            }
        }
    }

    /// Ends the session at the BMC, within `GOODBYE`: it has only a few.
    pub async fn close(&self) {
        let _ = tokio::time::timeout(GOODBYE, self.request(0x06, 0x3C, &self.id.to_le_bytes())).await;
    }
}

/// Sends a setup payload of `kind` and says the answer, sent back as `kind + 1`, once it says it went: its status
/// (its second byte) otherwise says why not.
async fn exchange(socket: &UdpSocket, kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let datagram = unsealed(kind, payload);
    for _ in 0..3 {
        socket.send(&datagram).await?;
        let answer = async {
            let mut answer = [0; 512];
            loop {
                let length = socket.recv(&mut answer).await?;
                let answer = &answer[..length];
                if answer.len() >= 18 && answer[5] == kind + 1 && answer[16] == payload[0] {
                    return Ok::<_, Error>(answer[16..].to_vec());
                }
            }
        };
        if let Ok(answer) = tokio::time::timeout(PATIENCE, answer).await {
            let answer = answer?;
            return match answer[1] {
                0 if answer.len() >= 8 => Ok(answer),
                // An unknown user or role, or the password's code didn't check out.
                0x0D | 0x09 | 0x0F => Err(Error::Unauthorized),
                status => Err(Error::Refused(status)),
            };
        }
    }
    Err(Error::Timeout)
}
