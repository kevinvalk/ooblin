//! AMT's redirection protocol on port 16995. Every session (SOL, KVM or IDER) gets its own TLS connection, opened with
//! StartRedirectionSession and authenticated with Digest over the socket itself, after which the stream carries that
//! session's own protocol. Byte layouts follow MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0).

use digest_auth::{AuthContext, Charset, Qop, WwwAuthenticateHeader};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

use super::{HUNG, Pin, Stream, tls};
use crate::{Error, Result, Target};

pub const REDIRECTION_PORT: u16 = 16995;
const AUTH_URI: &str = "/RedirectionService";
/// Digest with qop, the one authentication type we speak.
const DIGEST: u8 = 4;
/// The most a message may say it carries, far above any AMT sends (SOL's lengths are u16, a tile 64 by 64), so a
/// garbled length can't make us allocate gigabytes.
const MOST: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Sol,
    Kvm,
    Ider,
}

impl Session {
    fn start(self) -> [u8; 8] {
        match self {
            Self::Sol => [0x10, 0, 0, 0, b'S', b'O', b'L', b' '],
            Self::Kvm => [0x10, 1, 0, 0, b'K', b'V', b'M', b'R'],
            Self::Ider => [0x10, 0, 0, 0, b'I', b'D', b'E', b'R'],
        }
    }
}

/// An authenticated redirection session, ready for the session's own protocol.
pub struct Redirection {
    stream: Stream,
}

impl Redirection {
    pub async fn open(target: &Target, pin: Option<String>, session: Session) -> Result<Self> {
        let stream = tls(&target.host, target.redirection_port, &Pin::new(pin)).await?;
        let mut redirection = Self { stream };
        redirection.handshake(target, session).await?;
        Ok(redirection)
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        Ok(self.stream.read(buf).await?)
    }

    /// The next message's first byte, however long that takes.
    pub async fn read_u8(&mut self) -> Result<u8> {
        Ok(self.stream.read_u8().await?)
    }

    pub async fn read_exact(&mut self, len: usize) -> Result<Vec<u8>> {
        read_exact(&mut self.stream, len).await
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        write_all(&mut self.stream, bytes).await
    }

    /// Its two directions, to read and write at once.
    pub fn split(self) -> (ReadHalf<Stream>, WriteHalf<Stream>) {
        tokio::io::split(self.stream)
    }

    async fn handshake(&mut self, target: &Target, session: Session) -> Result<()> {
        self.write_all(&session.start()).await?;
        let reply = self.read_exact(13).await?;
        if reply[0] != 0x11 {
            return Err(Error::Protocol(format!("expected a redirection reply, got {:#04x}", reply[0])));
        }
        if reply[1] != 0 {
            return Err(Error::Protocol(format!("redirection refused (status {}), is it enabled?", reply[1])));
        }
        self.read_exact(usize::from(reply[12])).await?; // OEM data we have no use for

        let (_, offered) = self.authenticate(0, &[]).await?;
        if !offered.contains(&DIGEST) {
            return Err(Error::Protocol(format!("no Digest authentication on offer, only {offered:?}")));
        }

        // Fields are user, realm, nonce, uri, cnonce, nc, response and qop. Sent empty, AMT answers with a challenge.
        let (user, uri) = (target.user.as_str(), AUTH_URI);
        let (_, challenge) = self.authenticate(DIGEST, &encode_fields(&[user, "", "", uri, "", "", "", ""])).await?;
        let Ok([realm, nonce, qop]) = <[String; 3]>::try_from(decode_fields(&challenge)) else {
            return Err(Error::Protocol("not a Digest challenge".into()));
        };

        // This is the session's second message, so its count is 2, the way MeshCommander has it: `respond` counts up
        // from `nc` before it answers.
        let mut challenge = WwwAuthenticateHeader {
            domain: None,
            realm,
            nonce,
            opaque: None,
            stale: false,
            algorithm: Default::default(),
            qop: Some(vec![Qop::AUTH]),
            userhash: false,
            charset: Charset::ASCII,
            nc: 1,
        };
        let digest = challenge
            .respond(&AuthContext::new_post(user, &target.password, uri, None::<&[u8]>))
            .map_err(|error| Error::Protocol(error.to_string()))?;

        let (cnonce, nc) = (digest.cnonce.unwrap_or_default(), format!("{:08x}", digest.nc));
        let answer = encode_fields(&[user, &digest.realm, &digest.nonce, uri, &cnonce, &nc, &digest.response, &qop]);
        match self.authenticate(DIGEST, &answer).await? {
            (0, _) => Ok(()),
            _ => Err(Error::Unauthorized),
        }
    }

    /// One AuthenticateSession round trip, returning the reply's status and data.
    async fn authenticate(&mut self, kind: u8, data: &[u8]) -> Result<(u8, Vec<u8>)> {
        let mut message = vec![0x13, 0, 0, 0, kind];
        message.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        message.extend_from_slice(data);
        self.write_all(&message).await?;

        let header = self.read_exact(9).await?;
        if header[0] != 0x14 {
            return Err(Error::Protocol(format!("expected an authentication reply, got {:#04x}", header[0])));
        }
        let len = u32::from_le_bytes(header[5..9].try_into().unwrap());
        Ok((header[1], self.read_exact(len as usize).await?))
    }
}

/// `len` bytes of a message under way, unless the device hung on it or says it carries more than any message does.
pub(crate) async fn read_exact(reader: &mut (impl AsyncRead + Unpin), len: usize) -> Result<Vec<u8>> {
    if len > MOST {
        return Err(Error::Protocol(format!("a {len}-byte message")));
    }
    let mut bytes = vec![0; len];
    tokio::time::timeout(HUNG, reader.read_exact(&mut bytes)).await.map_err(|_| Error::Timeout)??;
    Ok(bytes)
}

/// TLS may hold on to what we write until we flush, so we do.
pub(crate) async fn write_all(writer: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    writer.write_all(bytes).await?;
    Ok(writer.flush().await?)
}

/// Authentication fields travel as length-prefixed strings, both ways.
pub fn encode_fields(values: &[&str]) -> Vec<u8> {
    values.iter().flat_map(|value| std::iter::once(value.len() as u8).chain(value.bytes())).collect()
}

pub fn decode_fields(mut data: &[u8]) -> Vec<String> {
    let mut fields = Vec::new();
    while let Some((&len, rest)) = data.split_first() {
        let len = usize::from(len).min(rest.len());
        fields.push(String::from_utf8_lossy(&rest[..len]).into_owned());
        data = &rest[len..];
    }
    fields
}
