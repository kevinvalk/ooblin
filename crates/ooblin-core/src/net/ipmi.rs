use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering::Relaxed};
use std::time::Duration;

use aws_lc_rs::cipher::{AES_128, DecryptingKey, DecryptionContext, EncryptingKey, UnboundCipherKey};
use aws_lc_rs::iv::FixedLength;
use aws_lc_rs::{hmac, rand};
use tokio::net::UdpSocket;

use crate::{Error, GOODBYE, Result, Target};

// The payload types we send and get: an IPMI message, SOL's characters, and the session's setup.
pub const MESSAGE: u8 = 0;
pub const SOL: u8 = 1;
const OPEN: u8 = 0x10;
const RAKP1: u8 = 0x12;
const RAKP3: u8 = 0x14;

// The privilege we ask for, because settings need an administrator, as the BMC's own pages do.
const ADMINISTRATOR: u8 = 4;

// A BMC answers at once or not at all and a datagram may be lost on the way, so we send each request `TRIES` times at
// most.
const PATIENCE: Duration = Duration::from_secs(1);
const TRIES: u8 = 3;

// NOTE: This is IPMI 2.0 over LAN (RMCP+, UDP 623), which every BMC speaks whoever made it. `Session::open`
// authenticates with RAKP and the password, and every message after is signed and encrypted. We ask for cipher suite
// 17 (SHA-256), and 3 (SHA-1) from BMCs without it, with AES-128 either way: NEVER a suite without both. The layouts
// are from the IPMI 2.0 specification, sections 13.6 to 13.32.
pub struct Session {
    socket: UdpSocket,
    bmc_id: u32, // The BMC's id for the session, which our datagrams carry
    our_id: u32, // Our id for the session, which the BMC's datagrams carry
    sealing: Sealing,
    sequence: AtomicU32,
    request_sequence: AtomicU8, // The IPMI requests' own sequence, six bits, which answers carry back

    // Closed, or lost: a BMC that answered none of a request's tries will not answer the next one, nor a goodbye.
    is_over: AtomicBool,
}

/// A suite's hash, for RAKP's codes and the integrity of every datagram after.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hash {
    Sha1,
    Sha256,
}

impl Hash {
    /// The numbers cipher suites 17 and 3 give their authentication and integrity algorithms.
    pub fn algorithms(self) -> (u8, u8) {
        match self {
            Self::Sha256 => (3, 4),
            Self::Sha1 => (1, 1),
        }
    }

    /// How much of a signature is a datagram's integrity code, and RAKP 4's.
    pub fn code_length(self) -> usize {
        match self {
            Self::Sha256 => 16,
            Self::Sha1 => 12,
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

/// How a session's datagrams are signed and encrypted. It is the same both ways, so the mock BMC uses it too.
pub struct Sealing {
    hash: Hash,
    integrity: Vec<u8>,   // K1, which signs
    encryption: [u8; 16], // The first 16 bytes of K2, which encrypt
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
        // We pad to whole AES blocks with 1, 2, 3… and how many, then encrypt behind a random IV.
        let padding = 15 - payload.len() % 16;
        let mut encrypted = [payload, &(1..=padding as u8).collect::<Vec<_>>(), &[padding as u8]].concat();
        let key = EncryptingKey::cbc(UnboundCipherKey::new(&AES_128, &self.encryption).unwrap()).unwrap();
        let iv = key.encrypt(&mut encrypted).unwrap();
        let encrypted = [<&[u8]>::try_from(&iv).unwrap(), &encrypted].concat();

        let mut datagram = [&[6, 0, 0xFF, 7, 6, 0xC0 | kind][..], &id.to_le_bytes(), &sequence.to_le_bytes()].concat();
        datagram.extend((encrypted.len() as u16).to_le_bytes());
        datagram.extend(encrypted);

        // From the authentication type to the next header has to be a multiple of 4 long, so we pad with 0xFF.
        let padding = (4 - (datagram.len() - 4 + 2) % 4) % 4;
        datagram.extend(std::iter::repeat_n(0xFF, padding));
        datagram.extend([padding as u8, 7]);

        let code = self.hash.sign(&self.integrity, &[&datagram[4..]]);
        datagram.extend(&code[..self.hash.code_length()]);
        datagram
    }

    /// A sealed datagram's payload type, session id and payload, if it is one and is authentic.
    pub fn open(&self, datagram: &[u8]) -> Option<(u8, u32, Vec<u8>)> {
        let code_length = self.hash.code_length();
        if datagram.len() < 16 + code_length || datagram[4] != 6 || datagram[5] & 0xC0 != 0xC0 {
            return None;
        }

        let (signed, code) = datagram.split_at(datagram.len() - code_length);
        if self.hash.sign(&self.integrity, &[&signed[4..]])[..code_length] != *code {
            return None;
        }

        let size = u16::from_le_bytes([datagram[14], datagram[15]]) as usize;
        let encrypted = datagram.get(16..16 + size).filter(|_| size >= 32 && size.is_multiple_of(16))?;
        let (iv, encrypted) = encrypted.split_at(16);
        let key = DecryptingKey::cbc(UnboundCipherKey::new(&AES_128, &self.encryption).unwrap()).unwrap();
        let mut payload = encrypted.to_vec();
        let iv = DecryptionContext::Iv128(FixedLength::from(<[u8; 16]>::try_from(iv).unwrap()));
        let padding = *key.decrypt(&mut payload, iv).ok()?.last()? as usize;
        payload.truncate(payload.len().checked_sub(padding + 1)?);

        let id = u32::from_le_bytes(datagram[6..10].try_into().unwrap());
        Some((datagram[5] & 0x3F, id, payload))
    }
}

/// The parts of RAKP both sides sign: the role asked for (by name, not slot), and the user's name with its length.
fn rakp_user(user: &str) -> Vec<u8> {
    [&[0x10 | ADMINISTRATOR, user.len() as u8][..], user.as_bytes()].concat()
}

/// A session setup datagram, which goes unsealed.
pub fn unsealed(kind: u8, payload: &[u8]) -> Vec<u8> {
    [&[6, 0, 0xFF, 7, 6, kind][..], &[0; 8], &(payload.len() as u16).to_le_bytes(), payload].concat()
}

/// An IPMI message's bytes from `from` to `to` (0x20 the BMC, 0x81 us), behind its header and checksums.
pub fn message(to: u8, function: u8, from: u8, sequence: u8, command: u8, data: &[u8]) -> Vec<u8> {
    let checksum = |bytes: &[u8]| bytes.iter().fold(0u8, |checksum, byte| checksum.wrapping_sub(*byte));
    let header = [to, function << 2];
    let body = [&[from, sequence << 2, command][..], data].concat();
    [&header[..], &[checksum(&header)], &body, &[checksum(&body)]].concat()
}

impl Session {
    /// Logs in to the BMC at `target`'s host, with SHA-256 if it can. A BMC that says no to the user or the role, or
    /// signs with another password than ours, means a wrong user or password, which we do not try again.
    pub async fn open(target: &Target) -> Result<Self> {
        if target.user.len() > 16 {
            return Err(Error::Invalid("An IPMI user name is at most 16 characters.".into()));
        }

        // Resolvers list IPv6 first, yet a BMC with both may answer IPMI on IPv4 only.
        let host = target.ipmi_host();
        let address = tokio::net::lookup_host((host, target.ipmi_port)).await?;
        let address = address.min_by_key(|address| address.is_ipv6());
        let address = address.ok_or_else(|| Error::Transport(format!("no address for {host}")))?;

        let socket = UdpSocket::bind(if address.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }).await?;
        socket.connect(address).await?;

        match Self::handshake(&socket, target, Hash::Sha256).await {
            // No such authentication or integrity algorithm, or no suite with them, means an older BMC.
            Err(Error::Refused(0x04..=0x07 | 0x10..=0x12)) => Self::handshake(&socket, target, Hash::Sha1).await,
            session => session,
        }
        .map(|(bmc_id, our_id, sealing)| Self {
            socket,
            bmc_id,
            our_id,
            sealing,
            sequence: AtomicU32::new(1),
            request_sequence: AtomicU8::new(0),
            is_over: AtomicBool::new(false),
        })
    }

    async fn handshake(socket: &UdpSocket, target: &Target, hash: Hash) -> Result<(u32, u32, Sealing)> {
        let random = |bytes: &mut [u8]| rand::fill(bytes).map_err(|_| Error::Transport("no randomness".into()));
        let short = || Error::Protocol("a short RAKP message".into());

        let mut our_id = [0; 4];
        random(&mut our_id)?;
        let (authentication, integrity) = hash.algorithms();
        let algorithm = |kind: u8, algorithm: u8| [kind, 0, 0, 8, algorithm, 0, 0, 0];

        // The BMC's answers carry our tag. We use another one for the SHA-1 try, so a late answer to the SHA-256 try
        // is not taken for its own. The tag of SHA-256 is the one ipmitool uses, 0.
        let tag = u8::from(hash == Hash::Sha1);
        let request = [
            &[tag, ADMINISTRATOR, 0, 0][..],
            &our_id,
            &algorithm(0, authentication),
            &algorithm(1, integrity),
            &algorithm(2, 1),
        ]
        .concat();
        let response = exchange(socket, OPEN, &request).await?;
        let bmc_id: [u8; 4] = response.get(8..12).ok_or_else(short)?.try_into().unwrap();

        let mut console_random = [0; 16];
        random(&mut console_random)?;
        let user = rakp_user(&target.user);
        let rakp1 = [&[tag, 0, 0, 0][..], &bmc_id, &console_random, &user[..1], &[0, 0], &user[1..]].concat();
        let rakp2 = exchange(socket, RAKP1, &rakp1).await?;
        let (bmc_random, bmc_guid) = (rakp2.get(8..24).ok_or_else(short)?, rakp2.get(24..40).ok_or_else(short)?);

        let password = target.password.as_bytes();
        let signature = hash.sign(password, &[&our_id, &bmc_id, &console_random, bmc_random, bmc_guid, &user]);
        if rakp2.get(40..) != Some(&signature) {
            return Err(Error::Unauthorized);
        }

        let sik = hash.sign(password, &[&console_random, bmc_random, &user]);
        let rakp3 = [&[tag, 0, 0, 0][..], &bmc_id, &hash.sign(password, &[bmc_random, &our_id, &user])].concat();
        let rakp4 = exchange(socket, RAKP3, &rakp3).await?;
        if rakp4.get(8..) != Some(&hash.sign(&sik, &[&console_random, &bmc_id, bmc_guid])[..hash.code_length()]) {
            return Err(Error::Protocol("the BMC's last RAKP message doesn't check out".into()));
        }

        Ok((u32::from_le_bytes(bmc_id), u32::from_le_bytes(our_id), Sealing::new(hash, &sik)))
    }

    /// Asks the BMC `command` of `function` (0x00 chassis, 0x06 application, 0x0C transport) and returns the answer's
    /// data, after its completion code: one that is not 0 is the error.
    pub async fn request(&self, function: u8, command: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.ask(function, command, data, TRIES, PATIENCE).await
    }

    /// Like `request`, but sent only once and waited for longer. It is for what must not happen twice (a reset, say)
    /// on a BMC that does not tell a resend from a new request.
    pub async fn request_once(&self, function: u8, command: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.ask(function, command, data, 1, 5 * PATIENCE).await
    }

    /// Like `attempt`, but we raise the session to administrator once the BMC says it is too low. RMCP+ lets a BMC
    /// start it at user (ipmi_sim does), which is why ipmitool always raises it. A session that is over times out at
    /// once.
    async fn ask(&self, function: u8, command: u8, data: &[u8], tries: u8, patience: Duration) -> Result<Vec<u8>> {
        if self.is_over.load(Relaxed) {
            return Err(Error::Timeout);
        }

        match self.attempt(function, command, data, tries, patience).await {
            Err(Error::Refused(0xD4)) if self.attempt(0x06, 0x3B, &[ADMINISTRATOR], TRIES, PATIENCE).await.is_ok() => {
                self.attempt(function, command, data, tries, patience).await
            }
            answer => answer,
        }
    }

    /// Sends a request up to `tries` times and waits `patience` for each answer. A BMC that answers none loses the
    /// session.
    async fn attempt(&self, function: u8, command: u8, data: &[u8], tries: u8, patience: Duration) -> Result<Vec<u8>> {
        let sequence = self.request_sequence.fetch_add(1, Relaxed) & 0x3F;
        let mut is_duplicate = false;

        for resend in 0..tries {
            self.send(MESSAGE, &message(0x20, function, 0x81, sequence, command, data)).await?;

            let answer = async {
                loop {
                    let (kind, payload) = self.receive().await?;
                    if kind != MESSAGE || payload.len() < 8 || payload[4] >> 2 != sequence || payload[5] != command {
                        continue;
                    }

                    // The BMC will not do a resend twice (0xCF) because it is still at the first, so we wait on the
                    // answer of the first, as ipmitool does.
                    if payload[6] == 0xCF && resend > 0 {
                        is_duplicate = true;
                        continue;
                    }

                    return Ok::<_, Error>(payload);
                }
            };

            if let Ok(answer) = tokio::time::timeout(patience, answer).await {
                let answer = answer?;
                return match answer[6] {
                    0 => Ok(answer[7..answer.len() - 1].to_vec()),
                    code => Err(Error::Refused(code)),
                };
            }
        }

        // The answer to the first never came, so the request went through but what it said is lost.
        if is_duplicate {
            return Ok(Vec::new());
        }

        self.is_over.store(true, Relaxed);
        Err(Error::Timeout)
    }

    /// Sends a payload of `kind` without waiting for anything back.
    pub async fn send(&self, kind: u8, payload: &[u8]) -> Result<()> {
        self.socket.send(&self.sealed(kind, payload)).await?;
        Ok(())
    }

    fn sealed(&self, kind: u8, payload: &[u8]) -> Vec<u8> {
        self.sealing.seal(kind, self.bmc_id, self.sequence.fetch_add(1, Relaxed), payload)
    }

    /// The next authentic payload of this session and its type. We wait for it as long as it takes and drop whatever
    /// else arrives.
    pub async fn receive(&self) -> Result<(u8, Vec<u8>)> {
        let mut datagram = [0; 1024];
        loop {
            let (length, _) = receive(&self.socket, &mut datagram).await?;
            if let Some((kind, id, payload)) = self.sealing.open(&datagram[..length])
                && id == self.our_id
            {
                return Ok((kind, payload));
            }
        }
    }

    /// Ends the session at the BMC, within `GOODBYE`, because a BMC has only a few.
    pub async fn close(&self) {
        if !self.is_over.swap(true, Relaxed) {
            let id = self.bmc_id.to_le_bytes();
            let _ = tokio::time::timeout(GOODBYE, self.attempt(0x06, 0x3C, &id, TRIES, PATIENCE)).await;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A session dropped unclosed (its future was cancelled, say) still has to end at the BMC, so we tell it
        // without waiting for its answer.
        if !self.is_over.load(Relaxed) {
            let sequence = self.request_sequence.fetch_add(1, Relaxed) & 0x3F;
            let close = message(0x20, 0x06, 0x81, sequence, 0x3C, &self.bmc_id.to_le_bytes());
            let _ = self.socket.try_send(&self.sealed(MESSAGE, &close));
        }
    }
}

/// Receives a datagram, past what Windows says about a single datagram rather than about the socket: an earlier one
/// of ours was refused, or this one is longer than `buffer` (WSAEMSGSIZE, 10040), which elsewhere arrives cut short.
pub(super) async fn receive(socket: &UdpSocket, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
    loop {
        match socket.recv_from(buffer).await {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => continue,
            Err(error) if error.raw_os_error() == Some(10040) => continue,
            received => return received,
        }
    }
}

/// Sends a setup payload of `kind` and returns the answer, which comes back as `kind + 1`, once it says the setup
/// went well. Otherwise its status (its second byte) says why not.
async fn exchange(socket: &UdpSocket, kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let request = unsealed(kind, payload);

    for _ in 0..TRIES {
        socket.send(&request).await?;

        let answer = async {
            let mut datagram = [0; 512];
            loop {
                let (length, _) = receive(socket, &mut datagram).await?;
                let answer = &datagram[..length];
                if answer.len() >= 18 && answer[5] == kind + 1 && answer[16] == payload[0] {
                    return Ok::<_, Error>(answer[16..].to_vec());
                }
            }
        };

        if let Ok(answer) = tokio::time::timeout(PATIENCE, answer).await {
            let answer = answer?;
            return match answer[1] {
                0 if answer.len() >= 8 => Ok(answer),
                // An unknown user, a role it may not have, or the code of the password did not check out.
                0x09 | 0x0A | 0x0D | 0x0F => Err(Error::Unauthorized),
                status => Err(Error::Refused(status)),
            };
        }
    }

    Err(Error::Timeout)
}
