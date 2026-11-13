//! AMT's redirection protocol on port 16995. Every session (SOL, KVM or IDER) gets its own TLS connection, opened with
//! StartRedirectionSession and authenticated with Digest over the socket itself, after which the stream carries that
//! session's own protocol. Byte layouts follow MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0).

use digest_auth::{AuthContext, Charset, Qop, WwwAuthenticateHeader};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};

use super::{Pin, Stream, tls};
use crate::{Error, Result, Target};

pub const REDIRECTION_PORT: u16 = 16995;
const AUTH_URI: &str = "/RedirectionService";
/// Digest with qop, the one authentication type we speak.
const DIGEST: u8 = 4;

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

    pub async fn read_exact(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0; len];
        self.stream.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// TLS may hold on to what we write until we flush, so we do.
    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes).await?;
        Ok(self.stream.flush().await?)
    }

    /// Its two directions, to read and write at once.
    pub fn split(self) -> (ReadHalf<Stream>, WriteHalf<Stream>) {
        tokio::io::split(self.stream)
    }

    async fn handshake(&mut self, target: &Target, session: Session) -> Result<()> {
        self.write_all(&session.start()).await?;
        let reply = self.read_exact(13).await?;
        if reply[0] != 0x11 || reply[1] != 0 {
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
        // This is the session's second message, so its count is 2, the way MeshCommander has it.
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
