use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use ooblin_core::Target;
use tokio::net::{TcpListener, ToSocketAddrs};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::version::TLS12;

use crate::{USER, identity, ikvm, ipmi, redfish};

pub struct Bmc {
    pub port: u16,
    /// What was asked of us, oldest first: `GET /redfish/v1/Systems/1`, `WS /`, `ikvm <hex>` and the like.
    pub requests: Mutex<Vec<String>>,
    pub(crate) fixture: PathBuf,
    pub(crate) redfish: redfish::State,
    pub(crate) ikvm: ikvm::State,
    pub(crate) ipmi: Mutex<ipmi::Replay>,
    /// A fixture without an `ipmi.txt` is of a BMC with IPMI off, which answers none unless we are recording it.
    pub(crate) is_ipmi_off: bool,
    /// The BMC we ask what the replay cannot answer, when we are recording its IPMI.
    pub(crate) ipmi_device: OnceLock<ooblin_core::ipmi::Session>,
    /// Where it answers IPMI on 127.0.0.1, `serve_ipmi` answers on more.
    pub ipmi_port: u16,
}

impl Bmc {
    pub async fn start(address: impl ToSocketAddrs, fixture: &Path) -> Arc<Self> {
        Self::serve(address, fixture, redfish::State::default()).await
    }

    /// Like `start`, but with the Redfish half in the state `redfish`: one that forwards to a device, say.
    pub(crate) async fn serve(address: impl ToSocketAddrs, fixture: &Path, redfish: redfish::State) -> Arc<Self> {
        let listener = TcpListener::bind(address).await.unwrap();
        let ipmi_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recording = std::fs::read_to_string(fixture.join("ipmi.txt"));
        let bmc = Arc::new(Self {
            port: listener.local_addr().unwrap().port(),
            requests: Mutex::default(),
            fixture: fixture.to_owned(),
            redfish,
            ikvm: ikvm::State::default(),
            is_ipmi_off: recording.is_err(),
            ipmi: Mutex::new(ipmi::Replay::read(&recording.unwrap_or_default())),
            ipmi_device: OnceLock::new(),
            ipmi_port: ipmi_socket.local_addr().unwrap().port(),
        });
        bmc.answer_ipmi(ipmi_socket);

        // NOTE: Redfish (under `/redfish/`), the web session and the WebSocket of the KVM (everywhere else) share this
        // one port, over TLS 1.2 only as on the X10.
        let (certificate, key) = identity();
        let tls = ServerConfig::builder_with_protocol_versions(&[&TLS12]);
        let tls = tls.with_no_client_auth().with_single_cert(vec![certificate], key).unwrap();
        let tls = TlsAcceptor::from(Arc::new(tls));
        let serving = bmc.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (tls, bmc) = (tls.clone(), serving.clone());
                tokio::spawn(async move {
                    let Ok(stream) = tls.accept(tcp).await else { return };
                    let service = service_fn(move |request| route(bmc.clone(), request));
                    let connection = http1::Builder::new().serve_connection(TokioIo::new(stream), service);
                    let _ = connection.with_upgrades().await;
                });
            }
        });

        bmc
    }

    pub fn target(&self, password: &str) -> Target {
        Target { ipmi_port: self.ipmi_port, ..Target::new(format!("127.0.0.1:{}", self.port), USER, password) }
    }

    /// What we were asked since the last call, oldest first.
    pub fn asked(&self) -> Vec<String> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }
}

async fn route(bmc: Arc<Bmc>, request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(match request.uri().path().starts_with("/redfish/") {
        true => redfish::serve(&bmc, request).await,
        false => ikvm::serve(&bmc, request).await,
    })
}
