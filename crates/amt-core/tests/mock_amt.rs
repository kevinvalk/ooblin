//! `Client` against a stand-in AMT: TLS with a self-signed certificate, Digest with a nonce we can expire, and canned
//! responses from go-wsman-messages. The real device has the final word, this keeps TLS, pinning and Digest honest.

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use amt_core::{Client, ControlMode, Error, Fingerprint, Target, Trust};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use md5::{Digest, Md5};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

const REALM: &str = "Digest:F3EB554784E729164447A89F60B641C5";
const PASSWORD: &str = "P@ssw0rd";

struct Amt {
    port: u16,
    fingerprint: Fingerprint,
    nonce: Mutex<String>,
    connections: AtomicUsize,
    challenges: AtomicUsize,
}

impl Amt {
    async fn start() -> Arc<Self> {
        let cert = rcgen::generate_simple_self_signed(["localhost".to_owned()]).unwrap();
        let der = CertificateDer::from(cert.cert.der().to_vec());
        let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
        let tls = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![der.clone()], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

        let amt = Arc::new(Self {
            port: listener.local_addr().unwrap().port(),
            fingerprint: Fingerprint::of(&der),
            nonce: Mutex::new("first".into()),
            connections: AtomicUsize::new(0),
            challenges: AtomicUsize::new(0),
        });
        let server = amt.clone();
        tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                server.connections.fetch_add(1, Ordering::SeqCst);
                let (acceptor, amt) = (acceptor.clone(), server.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let service = service_fn(move |request| {
                        let amt = amt.clone();
                        async move { Ok::<_, Infallible>(amt.respond(request).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), service)
                        .await;
                });
            }
        });
        amt
    }

    fn client(&self, password: &str, pin: Option<Fingerprint>) -> Client {
        let target = Target {
            host: "127.0.0.1".into(),
            port: self.port,
            user: "admin".into(),
            password: password.into(),
        };
        Client::new(&target, pin).unwrap()
    }

    async fn respond(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        assert!(
            request.headers().contains_key("content-length"),
            "AMT needs a Content-Length"
        );
        assert!(
            !request.headers().contains_key("transfer-encoding"),
            "AMT rejects chunked bodies"
        );

        let authorization = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        if !authorization.is_some_and(|header| self.verify(header)) {
            self.challenges.fetch_add(1, Ordering::SeqCst);
            let nonce = self.nonce.lock().unwrap().clone();
            let challenge = format!(r#"Digest realm="{REALM}", nonce="{nonce}", stale="false", qop="auth""#);
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("www-authenticate", challenge)
                .body(Full::default())
                .unwrap();
        }

        let body = request.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&body).unwrap();
        let fixture = [
            ("AMT_GeneralSettings", "amt_general_settings_get.xml"),
            (
                "AMT_SetupAndConfigurationService",
                "amt_setup_and_configuration_service_get.xml",
            ),
            ("enumeration/Enumerate<", "cim_software_identity_enumerate.xml"),
            ("enumeration/Pull<", "cim_software_identity_pull.xml"),
        ]
        .into_iter()
        .find_map(|(needle, fixture)| body.contains(needle).then_some(fixture))
        .unwrap_or_else(|| panic!("unexpected request: {body}"));
        let xml = std::fs::read(format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        Response::new(Full::new(xml.into()))
    }

    fn verify(&self, header: &str) -> bool {
        let param = |name: &str| {
            let value = header
                .split(", ")
                .find_map(|param| param.strip_prefix(&format!("{name}=")))?;
            Some(value.trim_matches('"').to_owned())
        };
        let md5 = |input: String| format!("{:x}", Md5::digest(input));
        let (Some(nonce), Some(nc), Some(cnonce), Some(response)) =
            (param("nonce"), param("nc"), param("cnonce"), param("response"))
        else {
            return false;
        };
        let (ha1, ha2) = (md5(format!("admin:{REALM}:{PASSWORD}")), md5("POST:/wsman".into()));
        nonce == *self.nonce.lock().unwrap() && response == md5(format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"))
    }
}

#[tokio::test]
async fn trusts_on_first_use_and_reads_info_over_one_connection() {
    let amt = Amt::start().await;
    let info = amt.client(PASSWORD, None).info().await.unwrap();

    assert_eq!(info.hostname, "Test Host Name");
    assert_eq!(info.version, "12.0.67");
    assert_eq!(info.control_mode, ControlMode::Admin);
    assert_eq!((info.fingerprint, info.trust), (amt.fingerprint, Trust::FirstUse));
    assert_eq!(amt.connections.load(Ordering::SeqCst), 1);
    assert_eq!(amt.challenges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn accepts_the_pinned_certificate() {
    let amt = Amt::start().await;
    let info = amt.client(PASSWORD, Some(amt.fingerprint)).info().await.unwrap();
    assert_eq!(info.trust, Trust::Pinned);
}

#[tokio::test]
async fn refuses_a_changed_certificate() {
    let amt = Amt::start().await;
    let old = Fingerprint::of(b"the certificate before reprovisioning");
    match amt.client(PASSWORD, Some(old)).info().await {
        Err(Error::PinMismatch { expected, actual }) => assert_eq!((expected, actual), (old, amt.fingerprint)),
        other => panic!("expected a pin mismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_wrong_credentials() {
    let amt = Amt::start().await;
    assert!(matches!(
        amt.client("wrong", None).info().await,
        Err(Error::Unauthorized)
    ));
}

#[tokio::test]
async fn answers_a_fresh_challenge_silently() {
    let amt = Amt::start().await;
    let mut client = amt.client(PASSWORD, None);
    client.get("AMT_GeneralSettings").await.unwrap();

    *amt.nonce.lock().unwrap() = "second".into();
    client.get("AMT_GeneralSettings").await.unwrap();
    assert_eq!(amt.challenges.load(Ordering::SeqCst), 2);
}
