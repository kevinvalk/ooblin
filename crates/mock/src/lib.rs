//! A stand-in Intel AMT that replays a fixture recorded from a real one (see `record`): WS-Man on one port, the SOL,
//! KVM and IDER redirection sessions on another, both over TLS with Digest. What a recording can't hold we answer by
//! the spec: enumeration contexts, successful method calls, faults for classes the device lacks, and the handshakes.
//! A fixture with `redfish/` is a Supermicro BMC's instead, one with only `ipmi.txt` a BMC we know over IPMI alone:
//! `Bmc` stands in for both.

#![allow(non_snake_case, reason = "our XML types name their fields the way SOAP does, `Body` say")]

mod bmc;
pub mod ikvm;
pub mod ipmi;
mod record;
pub mod redfish;
mod redirection;
mod scrub;
mod transcript;
mod wsman;

pub use bmc::Bmc;
pub use record::{crawl, record, record_sol};
pub use transcript::{Direction, Transcript};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use md5::{Digest as _, Md5};
use ooblin_core::amt::Connection;
use ooblin_core::{Flavor, Target};
use tokio::net::{TcpListener, ToSocketAddrs, UdpSocket};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

pub const USER: &str = "admin";
pub const PASSWORD: &str = "P@ssw0rd";
const REALM: &str = "Digest:F3EB554784E729164447A89F60B641C5";

/// Fixtures live in the repo's `fixtures/`, `go-wsman-messages` being Intel's sample responses.
pub fn fixtures() -> impl Iterator<Item = PathBuf> {
    let dir = fixture("");
    std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.is_dir())
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
}

/// Which device `fixture` was recorded from: one with `redfish/` is a Supermicro's, one with `ipmi.txt` alone a BMC's.
pub fn flavor(fixture: &Path) -> Flavor {
    match (fixture.join("redfish").is_dir(), fixture.join("ipmi.txt").is_file()) {
        (true, _) => Flavor::Supermicro,
        (false, true) => Flavor::Ipmi,
        (false, false) => Flavor::Amt,
    }
}

pub fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
}

pub struct Amt {
    pub port: u16,
    pub redirection_port: u16,
    pub challenges: AtomicUsize,
    /// Every authenticated WS-Man request body, oldest first.
    pub requests: Mutex<Vec<String>>,
    /// The page each enumeration context is at.
    pages: Mutex<HashMap<String, usize>>,
    fixture: PathBuf,
    /// Replies in go-wsman-messages' layout, by class and lowercase action.
    samples: HashMap<(String, String), PathBuf>,
    nonce: Mutex<String>,
    /// When recording: the device we forward every WS-Man request to.
    device: Option<tokio::sync::Mutex<Connection>>,
}

impl Amt {
    /// Serves `fixture` with WS-Man on `wsman` and the redirection sessions on `redirection`.
    pub async fn start(wsman: impl ToSocketAddrs, redirection: impl ToSocketAddrs, fixture: &Path) -> Arc<Self> {
        Self::serve(wsman, redirection, fixture, None).await
    }

    /// Forwards WS-Man to `device` and keeps its method replies in `fixture`, see `record`.
    async fn forward(device: Connection, fixture: &Path) -> Arc<Self> {
        Self::serve("127.0.0.1:0", "127.0.0.1:0", fixture, Some(device)).await
    }

    async fn serve(
        wsman: impl ToSocketAddrs,
        redirection: impl ToSocketAddrs,
        fixture: &Path,
        device: Option<Connection>,
    ) -> Arc<Self> {
        let (wsman, redirection) =
            (TcpListener::bind(wsman).await.unwrap(), TcpListener::bind(redirection).await.unwrap());
        let amt = Arc::new(Self {
            port: wsman.local_addr().unwrap().port(),
            redirection_port: redirection.local_addr().unwrap().port(),
            challenges: AtomicUsize::new(0),
            requests: Mutex::default(),
            pages: Mutex::default(),
            fixture: fixture.to_owned(),
            samples: samples(fixture),
            nonce: Mutex::new("first".into()),
            device: device.map(tokio::sync::Mutex::new),
        });
        let (certificate, key) = identity();
        let tls = ServerConfig::builder().with_no_client_auth().with_single_cert(vec![certificate], key).unwrap();
        let tls = TlsAcceptor::from(Arc::new(tls));
        tokio::spawn(wsman::serve(wsman, tls.clone(), amt.clone()));
        tokio::spawn(redirection::serve(redirection, tls, amt.fixture.clone()));
        amt
    }

    /// Us at both our ports, as `USER` with `password`.
    pub fn target(&self, password: &str) -> Target {
        Target {
            wsman_port: self.port,
            redirection_port: self.redirection_port,
            ..Target::new("127.0.0.1", USER, password)
        }
    }

    pub fn certificate() -> CertificateDer<'static> {
        identity().0
    }

    /// Makes the next request fail its Digest check, as AMT does once a nonce gets old.
    pub fn expire_nonce(&self) {
        *self.nonce.lock().unwrap() = "expired".into();
    }
}

/// Answers presence pings on `address` (UDP) as the MS-02 does: an acknowledgement, then its pong with the ping's tag.
/// Only from this machine, nothing else on the network should find us. Says the port it got. A `Bmc` answers its own.
pub async fn answer_pings(address: impl ToSocketAddrs) -> std::io::Result<u16> {
    const PONG: [u8; 28] = [
        0x06, 0, 0, 0x06, 0, 0, 0x01, 0x57, 0x40, 0, 0, 0x10, 0, 0, 0x01, 0x57, 0x42, 0x61, 0, 0x02, 0, 0x20, 0x13, 0,
        0, 0, 0, 0,
    ];
    let socket = UdpSocket::bind(address).await?;
    let port = socket.local_addr()?.port();
    tokio::spawn(async move {
        let mut ping = [0; 12];
        while let Ok((length, from)) = receive(&socket, &mut ping).await {
            if length != 12 || !from.ip().is_loopback() {
                continue;
            }
            let mut pong = PONG;
            pong[9] = ping[9];
            socket.send_to(&[0x06, 0, ping[2], 0x86], from).await.unwrap();
            socket.send_to(&pong, from).await.unwrap();
        }
    });
    Ok(port)
}

/// Receives a datagram, past what Windows says when an answer of ours was refused: the client had gone.
async fn receive(socket: &UdpSocket, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
    loop {
        match socket.recv_from(buffer).await {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => continue,
            received => return received,
        }
    }
}

/// Indexes replies laid out like go-wsman-messages' (`amt/general/get.xml`), by the class each one names.
fn samples(dir: &Path) -> HashMap<(String, String), PathBuf> {
    let mut samples = HashMap::new();
    for path in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            samples.extend(self::samples(&path));
        } else if let (Some(stem), Ok(xml)) = (path.file_stem(), std::fs::read_to_string(&path)) {
            let Ok(doc) = roxmltree::Document::parse(xml.trim_start()) else { continue };
            let uri = doc.descendants().find(|node| node.has_tag_name("ResourceURI")).and_then(|node| node.text());
            let class = uri.and_then(|uri| uri.rsplit('/').next()).unwrap_or_default().to_owned();
            samples.entry((class, stem.to_string_lossy().to_lowercase())).or_insert(path);
        }
    }
    samples
}

/// Ed25519 signs deterministically, so a fixed seed gives the same certificate on every start and the app's pin for
/// 127.0.0.1 keeps matching. The prefix makes the seed a PKCS#8 key (RFC 8410).
fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let pkcs8 = [
        &[0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20][..],
        &[42; 32],
    ]
    .concat();
    let key = rcgen::KeyPair::try_from(pkcs8.as_slice()).unwrap();
    let certificate = rcgen::CertificateParams::new(["mock-amt".to_owned()]).unwrap().self_signed(&key).unwrap();
    (certificate.der().clone(), PrivateKeyDer::Pkcs8(pkcs8.into()))
}

/// The Digest response of a client that knows our password (RFC 2617, MD5, qop), on either port.
fn digest(uri: &str, nonce: &str, nc: &str, cnonce: &str, qop: &str) -> String {
    let md5 = |input: String| format!("{:x}", Md5::digest(input));
    let (ha1, ha2) = (md5(format!("{USER}:{REALM}:{PASSWORD}")), md5(format!("POST:{uri}")));
    md5(format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}"))
}
