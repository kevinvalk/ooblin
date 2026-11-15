//! A stand-in Intel AMT: WS-Man on one port (see `wsman`), the SOL, KVM and IDER redirection sessions on another (see
//! `redirection`), both over TLS with Digest, and its presence pong. What a recording can't hold we answer by the spec:
//! enumeration contexts, successful method calls, faults for classes the device lacks, and the handshakes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ooblin_core::Target;
use ooblin_core::amt::Connection;
use tokio::net::{TcpListener, ToSocketAddrs, UdpSocket};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::CertificateDer;

use crate::transcript::Transcript;
use crate::{USER, identity, receive, redirection, wsman};

pub struct Amt {
    pub port: u16,
    pub redirection_port: u16,
    pub challenges: AtomicUsize,
    /// What was asked of us, oldest first: every authenticated WS-Man request's body, and what's typed on the serial
    /// console (`SOL "ls\r"`).
    pub requests: Mutex<Vec<String>>,
    /// The page each enumeration context is at.
    pub(crate) pages: Mutex<HashMap<String, usize>>,
    pub(crate) fixture: PathBuf,
    /// Replies in go-wsman-messages' layout, by class and lowercase action.
    pub(crate) samples: HashMap<(String, String), PathBuf>,
    pub(crate) nonce: Mutex<String>,
    pub(crate) device: Option<Device>,
    /// Whether we hang up on the next WS-Man request, see `hang_up`.
    pub(crate) hangs_up: AtomicBool,
}

/// When recording: the device we forward to, every WS-Man request over `connection` and each redirection session
/// over a connection of its own, as `target` with the certificate `pin`. And the session at hand, or the last one, as
/// it went: ours until it ends.
pub(crate) struct Device {
    pub connection: tokio::sync::Mutex<Connection>,
    pub target: Target,
    pub pin: Option<String>,
    pub session: tokio::sync::Mutex<Transcript>,
}

impl Amt {
    pub async fn start(wsman: impl ToSocketAddrs, redirection: impl ToSocketAddrs, fixture: &Path) -> Arc<Self> {
        Self::serve(wsman, redirection, fixture, None).await
    }

    /// Forwards to the device `target` logs in to, over `connection` and what it pinned, and keeps its method replies
    /// in `fixture`, see `record`.
    pub(crate) async fn forward(connection: Connection, target: &Target, fixture: &Path) -> Arc<Self> {
        let (pin, target, session) = (connection.fingerprint(), target.clone(), Default::default());
        let device = Device { connection: connection.into(), target, pin, session };
        Self::serve("127.0.0.1:0", "127.0.0.1:0", fixture, Some(device)).await
    }

    /// The redirection session we last forwarded, once it's over.
    pub(crate) async fn forwarded(&self) -> Transcript {
        std::mem::take(&mut *self.device.as_ref().expect("we're recording").session.lock().await)
    }

    async fn serve(
        wsman: impl ToSocketAddrs,
        redirection: impl ToSocketAddrs,
        fixture: &Path,
        device: Option<Device>,
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
            device,
            hangs_up: AtomicBool::new(false),
        });

        let (certificate, key) = identity();
        let tls = ServerConfig::builder().with_no_client_auth().with_single_cert(vec![certificate], key).unwrap();
        let tls = TlsAcceptor::from(Arc::new(tls));
        tokio::spawn(wsman::serve(wsman, tls.clone(), amt.clone()));
        tokio::spawn(redirection::serve(redirection, tls, amt.clone()));
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

    /// What we were asked since the last call, oldest first.
    pub fn asked(&self) -> Vec<String> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }

    /// Makes the next request fail its Digest check, as AMT does once a nonce gets old.
    pub fn expire_nonce(&self) {
        *self.nonce.lock().unwrap() = "expired".into();
    }

    /// Reads the next WS-Man request and hangs up instead of answering it, as AMT at times does.
    pub fn hang_up(&self) {
        self.hangs_up.store(true, Ordering::SeqCst);
    }
}

/// The MS-02's pong to a presence ping.
pub const PONG: [u8; 28] = [
    0x06, 0, 0, 0x06, 0, 0, 0x01, 0x57, 0x40, 0, 0, 0x10, 0, 0, 0x01, 0x57, 0x42, 0x61, 0, 0x02, 0, 0x20, 0x13, 0, 0,
    0, 0, 0,
];

/// Answers presence pings on `address` (UDP) as the MS-02 does: an acknowledgement, then its pong with the ping's tag.
/// Only from this machine, nothing else on the network should find us. Says the port it got. A `Bmc` answers its own.
pub async fn answer_pings(address: impl ToSocketAddrs) -> std::io::Result<u16> {
    let socket = UdpSocket::bind(address).await?;
    let port = socket.local_addr()?.port();
    tokio::spawn(async move {
        // Room for more than a ping, so what's longer doesn't read as one.
        let mut datagram = [0; 64];
        while let Ok((length, from)) = receive(&socket, &mut datagram).await {
            // RMCP's version, sequence number and class ASF, then ASF's enterprise number, a presence ping and its tag.
            let &[0x06, 0, sequence, 0x06, 0, 0, 0x11, 0xBE, 0x80, tag, 0, 0] = &datagram[..length] else { continue };
            if !from.ip().is_loopback() {
                continue;
            }
            let mut pong = PONG;
            pong[9] = tag;
            // A client that left before we answered is no reason to stop.
            let _ = socket.send_to(&[0x06, 0, sequence, 0x86], from).await;
            let _ = socket.send_to(&pong, from).await;
        }
    });
    Ok(port)
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
