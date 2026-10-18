//! A stand-in Intel AMT that replays a fixture recorded from a real one (see `record`): WS-Man on one port, the SOL,
//! KVM and IDER redirection sessions on another, both over TLS with Digest. What a recording can't hold we answer by
//! the spec: enumeration contexts, successful method calls, faults for classes the device lacks, and the handshakes.

mod record;
mod redirection;
mod scrub;
mod transcript;
mod wsman;

pub use record::{crawl, record};
pub use transcript::{Direction, Transcript};

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use md5::{Digest as _, Md5};
use tokio::net::{TcpListener, ToSocketAddrs};
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

pub struct Amt {
    pub port: u16,
    pub redirection_port: u16,
    pub connections: AtomicUsize,
    pub challenges: AtomicUsize,
    /// Every authenticated WS-Man request body, oldest first.
    pub requests: Mutex<Vec<String>>,
    fixture: PathBuf,
    nonce: Mutex<String>,
}

impl Amt {
    /// Serves `fixture` with WS-Man on `wsman` and the redirection sessions on `redirection`.
    pub async fn start(wsman: impl ToSocketAddrs, redirection: impl ToSocketAddrs, fixture: &Path) -> Arc<Self> {
        let (wsman, redirection) =
            (TcpListener::bind(wsman).await.unwrap(), TcpListener::bind(redirection).await.unwrap());
        let amt = Arc::new(Self {
            port: wsman.local_addr().unwrap().port(),
            redirection_port: redirection.local_addr().unwrap().port(),
            connections: AtomicUsize::new(0),
            challenges: AtomicUsize::new(0),
            requests: Mutex::default(),
            fixture: fixture.to_owned(),
            nonce: Mutex::new("first".into()),
        });
        let (certificate, key) = identity();
        let tls = ServerConfig::builder().with_no_client_auth().with_single_cert(vec![certificate], key).unwrap();
        let tls = TlsAcceptor::from(Arc::new(tls));
        tokio::spawn(wsman::serve(wsman, tls.clone(), amt.clone()));
        tokio::spawn(redirection::serve(redirection, tls, amt.fixture.clone()));
        amt
    }

    pub fn certificate() -> CertificateDer<'static> {
        identity().0
    }

    /// Makes the next request fail its Digest check, as AMT does once a nonce gets old.
    pub fn expire_nonce(&self) {
        *self.nonce.lock().unwrap() = "expired".into();
    }
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
