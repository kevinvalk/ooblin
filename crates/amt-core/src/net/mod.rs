//! Our whole egress surface: nothing outside this module may touch the network (`tests/net_boundary.rs`). AMT 19 is
//! TLS-only, so there is deliberately no plaintext path. This file speaks WS-Man on 16993, `redirection` the binary
//! SOL, KVM and IDER sessions on 16995, and both pin the device's certificate the same way.

mod redirection;

pub use redirection::{REDIRECTION_PORT, Redirection, Session, decode_fields, encode_fields};

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use digest_auth::{AuthContext, WwwAuthenticateHeader};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::{Error, Result, Target, wsman};

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
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(pin.tls())
            .pool_max_idle_per_host(1)
            // A burst of reads shares a connection, but we don't reuse one AMT may have given up on in the meantime.
            .pool_idle_timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(10))
            // AMT answers in a second or two, a device taking longer has most likely hung.
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(transport)?;

        Ok(Self {
            http,
            url: format!("https://{}:{}/wsman", target.host, target.wsman_port),
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
        let body = response.text().await.map_err(transport)?;
        match status {
            StatusCode::UNAUTHORIZED => Err(Error::Unauthorized),
            status if status.is_success() => Ok(body),
            status => Err(wsman::fault(&body).map_or(Error::Http(status.as_u16()), Error::Fault)),
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

fn protocol(error: digest_auth::Error) -> Error {
    Error::Protocol(error.to_string())
}

/// AMT certificates are self-signed, so instead of a CA we trust the SHA-256 of the leaf certificate, as `AB:CD:…`.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(":")
}

/// Handshake signatures are still verified, so the peer must actually hold the pinned certificate's key.
#[derive(Debug)]
struct Pin {
    pinned: OnceLock<String>,
    rejected: OnceLock<String>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl Pin {
    fn new(pin: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            pinned: pin.map(OnceLock::from).unwrap_or_default(),
            rejected: OnceLock::new(),
            algorithms: aws_lc_rs::default_provider().signature_verification_algorithms,
        })
    }

    fn tls(self: &Arc<Self>) -> ClientConfig {
        ClientConfig::builder().dangerous().with_custom_certificate_verifier(self.clone()).with_no_client_auth()
    }

    /// Why a connection failed, if it was because the device presented another certificate.
    fn mismatch(&self) -> Option<Error> {
        let (expected, actual) = (self.pinned.get()?.clone(), self.rejected.get()?.clone());
        Some(Error::PinMismatch { expected, actual })
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
        let _ = self.rejected.set(presented);
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
