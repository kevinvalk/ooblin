//! AMT's redirection protocol on port 16995. Every session (SOL, KVM or IDER) gets its own TLS connection, opened with
//! StartRedirectionSession and authenticated with Digest over the socket itself, after which the stream carries that
//! session's own protocol. Byte layouts follow MeshCommander's `amt-redir-ws-0.1.0.js` (Apache-2.0).

use std::time::Duration;

use md5::{Digest as _, Md5};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::Pin;
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
    stream: TlsStream<TcpStream>,
}

impl Redirection {
    pub async fn open(target: &Target, pin: Option<String>, session: Session) -> Result<Self> {
        let pin = Pin::new(pin);
        let address = (target.host.as_str(), target.redirection_port);
        let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address))
            .await
            .map_err(|_| Error::Transport(format!("connecting to {}:{} timed out", address.0, address.1)))?
            .map_err(io)?;
        let name = ServerName::try_from(target.host.clone()).map_err(|error| Error::Transport(error.to_string()))?;
        let stream = TlsConnector::from(std::sync::Arc::new(pin.tls()))
            .connect(name, tcp)
            .await
            .map_err(|error| pin.mismatch().unwrap_or_else(|| io(error)))?;

        let mut redirection = Self { stream };
        redirection.handshake(target, session).await?;
        Ok(redirection)
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.stream.read(buf).await.map_err(io)
    }

    pub async fn read_exact(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0; len];
        self.stream.read_exact(&mut buf).await.map_err(io)?;
        Ok(buf)
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes).await.map_err(io)
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
        let (_, challenge) = self.authenticate(DIGEST, &fields(&[user, "", "", uri, "", "", "", ""])).await?;
        let [realm, nonce, qop] = parse_fields(&challenge)?;
        let (cnonce, nc) = (cnonce(), "00000002");
        let ha1 = md5_hex(&format!("{user}:{realm}:{}", target.password));
        let ha2 = md5_hex(&format!("POST:{uri}"));
        let response = md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}"));

        let answer = fields(&[user, &realm, &nonce, uri, &cnonce, nc, &response, &qop]);
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

/// Length-prefixed strings, the way every authentication field travels.
fn fields(values: &[&str]) -> Vec<u8> {
    values.iter().flat_map(|value| std::iter::once(value.len() as u8).chain(value.bytes())).collect()
}

fn parse_fields<const N: usize>(mut data: &[u8]) -> Result<[String; N]> {
    let mut fields = std::array::from_fn(|_| String::new());
    for field in &mut fields {
        let (&len, rest) = data.split_first().ok_or_else(|| Error::Protocol("short Digest challenge".into()))?;
        let value = rest.get(..usize::from(len)).ok_or_else(|| Error::Protocol("short Digest challenge".into()))?;
        *field = String::from_utf8_lossy(value).into_owned();
        data = &rest[usize::from(len)..];
    }
    Ok(fields)
}

fn cnonce() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS has randomness");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn md5_hex(input: &str) -> String {
    format!("{:x}", Md5::digest(input))
}

fn io(error: std::io::Error) -> Error {
    Error::Transport(error.to_string())
}
