//! Our whole egress surface: nothing outside this module may touch the network (`tests/net_boundary.rs`). AMT 19 and
//! the BMCs are TLS-only, so there is deliberately no plaintext path. This file speaks WS-Man on 16993, `redirection`
//! AMT's binary SOL, KVM and IDER sessions on 16995, `redfish` a BMC's Redfish, `web` its web pages and `ikvm` its KVM,
//! all pinning the device's certificate the same way. `discovery` finds devices with a UDP ping to 623, where `ipmi`
//! speaks to a BMC, encrypted with keys from its password instead of TLS.

mod discovery;
pub mod ikvm;
pub mod ipmi;
pub mod redfish;
mod redirection;
pub mod web;

pub use discovery::{Found, RMCP_PORT, discover, nearby};
pub use redirection::{REDIRECTION_PORT, Redirection, Session, decode_fields, encode_fields};
pub(crate) use redirection::{read_exact, write_all};

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use digest_auth::{AuthContext, WwwAuthenticateHeader};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, SignatureScheme};
use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::amt::wsman;
use crate::{Error, Result, Target};

pub const WSMAN_PORT: u16 = 16993;

/// A keep-alive HTTPS connection to `/wsman`. AMT answers parallel requests with 503s, so `&mut self` it is.
pub struct Connection {
    http: reqwest::Client,
    url: String,
    user: String,
    password: String,
    challenge: Option<WwwAuthenticateHeader>,
    pin: Arc<Pin>,
}

impl Connection {
    /// Without a `pin` we pin whatever certificate we see first, so it can't change under us mid-session.
    pub fn new(target: &Target, pin: Option<String>) -> Result<Self> {
        let pin = Pin::new(pin);
        Ok(Self {
            http: http(&pin)?,
            url: format!("https://{}:{}/wsman", target.authority(), target.wsman_port),
            user: target.user.clone(),
            password: target.password.clone(),
            challenge: None,
            pin,
        })
    }

    /// The pinned certificate, known once the first connection was made.
    pub fn fingerprint(&self) -> Option<String> {
        self.pin.pinned.get().cloned()
    }

    /// We keep answering the last Digest challenge, so only a fresh (or stale) nonce costs an extra round trip.
    pub async fn post(&mut self, envelope: String) -> Result<String> {
        let mut response = self.send(&envelope).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let challenge = response.headers().get(WWW_AUTHENTICATE).and_then(|value| value.to_str().ok());
            self.challenge = Some(digest_auth::parse(challenge.unwrap_or_default()).map_err(protocol)?);
            response = self.send(&envelope).await?;
        }

        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            body.extend_from_slice(&chunk);
            if body.len() > MOST {
                return Err(Error::Protocol(format!("a reply of more than {MOST} bytes")));
            }
        }
        let body = String::from_utf8_lossy(&body).into_owned();

        match status {
            StatusCode::UNAUTHORIZED => Err(Error::Unauthorized),
            status if status.is_success() => Ok(body),
            status => Err(match wsman::fault(&body) {
                Some(fault) if fault.contains("UnsupportedFeature") => Error::Unsupported,
                fault => fault.map_or(Error::Http(status.as_u16()), Error::Fault),
            }),
        }
    }

    async fn send(&mut self, envelope: &str) -> Result<reqwest::Response> {
        let mut request = self
            .http
            .post(&self.url)
            .header(CONTENT_TYPE, "application/soap+xml;charset=UTF-8")
            .body(envelope.to_owned());
        if let Some(challenge) = &mut self.challenge {
            let context = AuthContext::new_post(&self.user, &self.password, "/wsman", None::<&[u8]>);
            request = request.header(AUTHORIZATION, challenge.respond(&context).map_err(protocol)?.to_header_string());
        }
        request.send().await.map_err(|error| self.pin.mismatch().unwrap_or_else(|| transport(error)))
    }
}

/// A device answers in a second or two, one taking this long has most likely hung.
const HUNG: Duration = Duration::from_secs(20);

/// The most a reply may be, far above any AMT sends (the MS-02's largest pages are some 50 KB), so a device that keeps
/// sending can't fill our memory.
const MOST: usize = 8 << 20;

/// HTTPS to a device that only takes the pinned certificate.
fn http(pin: &Arc<Pin>) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .use_preconfigured_tls(pin.tls())
        .pool_max_idle_per_host(1)
        // A burst of reads shares a connection, but we don't reuse one the device may have given up on in the meantime.
        .pool_idle_timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(10))
        .timeout(HUNG)
        .build()
        .map_err(transport)
}

pub type Stream = TlsStream<TcpStream>;

/// A TLS connection to `host`'s `port` that only takes the pinned certificate, for a protocol of its own.
async fn tls(host: &str, port: u16, pin: &Arc<Pin>) -> Result<Stream> {
    let host = host.trim_matches(['[', ']']);
    let name = ServerName::try_from(host.to_owned()).map_err(|error| Error::Transport(error.to_string()))?;
    let connect = async {
        let tcp = TcpStream::connect((host, port)).await?;
        // Our messages are small and each waits on an answer, Nagle's algorithm would hold every one of them back.
        tcp.set_nodelay(true)?;
        // A session waits for the device as long as it takes, and one that vanished says nothing: the OS asks after
        // 10 quiet seconds, and hangs up when 3 askings 5 seconds apart go unanswered.
        let asking = TcpKeepalive::new()
            .with_time(Duration::from_secs(10))
            .with_interval(Duration::from_secs(5))
            .with_retries(3);
        SockRef::from(&tcp).set_tcp_keepalive(&asking)?;

        TlsConnector::from(Arc::new(pin.tls()))
            .connect(name, tcp)
            .await
            .map_err(|error| pin.mismatch().unwrap_or_else(|| error.into()))
    };
    tokio::time::timeout(Duration::from_secs(10), connect)
        .await
        .map_err(|_| Error::Transport(format!("connecting to {host}:{port} timed out")))?
}

/// reqwest's own message is just "error sending request for url (…)", the actual reason is the last of its causes. A
/// timeout's is "deadline has elapsed", which helps nobody.
fn transport(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        return Error::Timeout;
    }
    let mut cause: &dyn std::error::Error = &error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    Error::Transport(cause.to_string())
}

/// A response that succeeded, else its status as the error.
fn ok(response: reqwest::Response) -> Result<reqwest::Response> {
    match response.status() {
        status if status.is_success() => Ok(response),
        status => Err(Error::Http(status.as_u16())),
    }
}

fn protocol(error: digest_auth::Error) -> Error {
    Error::Protocol(error.to_string())
}

/// AMT certificates are self-signed, so instead of a CA we trust the SHA-256 of the leaf certificate, as `AB:CD:…`.
pub fn fingerprint(der: &[u8]) -> String {
    ::aws_lc_rs::digest::digest(&::aws_lc_rs::digest::SHA256, der)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Handshake signatures are still verified, so the peer must actually hold the pinned certificate's key.
#[derive(Debug)]
struct Pin {
    pinned: OnceLock<String>,
    /// The other certificate a device presented, until `mismatch` says so.
    rejected: Mutex<Option<String>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl Pin {
    fn new(pin: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            pinned: pin.map(OnceLock::from).unwrap_or_default(),
            rejected: Mutex::default(),
            algorithms: aws_lc_rs::default_provider().signature_verification_algorithms,
        })
    }

    fn tls(self: &Arc<Self>) -> ClientConfig {
        ClientConfig::builder().dangerous().with_custom_certificate_verifier(self.clone()).with_no_client_auth()
    }

    /// Why a connection failed, if it was because the device presented another certificate. Said once: what fails
    /// next has a reason of its own.
    fn mismatch(&self) -> Option<Error> {
        let actual = self.rejected.lock().unwrap().take()?;
        Some(Error::PinMismatch { expected: self.pinned.get()?.clone(), actual })
    }
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = fingerprint(end_entity);
        if *self.pinned.get_or_init(|| presented.clone()) == presented {
            return Ok(ServerCertVerified::assertion());
        }
        *self.rejected.lock().unwrap() = Some(presented);
        Err(rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
