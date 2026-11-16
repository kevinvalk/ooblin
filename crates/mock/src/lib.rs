#![allow(non_snake_case, reason = "We name the fields of our XML types the way SOAP names its elements, `Body` say")]

mod amt;
mod bmc;
pub mod ikvm;
pub mod ipmi;
mod record;
pub mod redfish;
mod redirection;
mod scrub;
mod transcript;
mod wsman;

pub use amt::{Amt, PONG, answer_pings};
pub use bmc::Bmc;
pub use record::{crawl, record, record_sol};
pub use transcript::{Direction, Transcript, received, sent};

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use md5::{Digest as _, Md5};
use ooblin_core::Flavor;
use tokio::net::UdpSocket;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

pub const USER: &str = "admin";
pub const PASSWORD: &str = "P@ssw0rd";
const REALM: &str = "Digest:F3EB554784E729164447A89F60B641C5";

/// The fixtures live in the `fixtures/` of the repository, where `go-wsman-messages` holds the sample responses of
/// Intel.
pub fn fixtures() -> impl Iterator<Item = PathBuf> {
    let directory = fixture("");

    std::fs::read_dir(directory).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.is_dir())
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
}

/// The flavors we can log in to the device of `fixture` as, the one a login finds it to be first: a Supermicro when
/// its Redfish has their manager `1`, a BMC over IPMI when it has an `ipmi.txt`, a BMC over Redfish when that lists its
/// managers, and an AMT when it has none of these.
pub fn flavors(fixture: &Path) -> Vec<Flavor> {
    let has_file = |file: &str| fixture.join(file).is_file();
    let flavors = [
        (Flavor::Supermicro, has_file("redfish/v1/Managers/1/index.json")),
        (Flavor::Ipmi, has_file("ipmi.txt")),
        (Flavor::Redfish, has_file("redfish/v1/Managers/index.json")),
    ];
    let flavors: Vec<_> = flavors.into_iter().filter(|(_, is_found)| *is_found).map(|(flavor, _)| flavor).collect();

    if flavors.is_empty() { vec![Flavor::Amt] } else { flavors }
}

/// The flavor a login finds the device of `fixture` to be.
pub fn flavor(fixture: &Path) -> Flavor {
    flavors(fixture)[0]
}

pub fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap()).collect()
}

/// Windows reports a connection reset on our socket when an answer of ours was refused because the client had gone, so
/// we receive past those.
async fn receive(socket: &UdpSocket, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
    loop {
        match socket.recv_from(buffer).await {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => continue,
            received => return received,
        }
    }
}

/// Ed25519 signs deterministically, so a fixed seed gives us the same certificate on every start and the pin the app
/// keeps for 127.0.0.1 keeps matching. The prefix makes the seed a PKCS#8 key (RFC 8410).
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

/// The Digest response of a client that knows our password (RFC 2617, MD5, qop), on either port of an AMT.
fn digest(uri: &str, nonce: &str, nc: &str, cnonce: &str, qop: &str) -> String {
    let md5 = |input: String| format!("{:x}", Md5::digest(input));
    let (ha1, ha2) = (md5(format!("{USER}:{REALM}:{PASSWORD}")), md5(format!("POST:{uri}")));

    md5(format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}"))
}
