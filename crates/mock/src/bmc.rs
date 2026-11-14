//! A stand-in Supermicro BMC on one port, TLS 1.2 only as the X10's: Redfish under `/redfish/` (see `redfish`), its web
//! session and the KVM's WebSocket everywhere else (see `ikvm`); IPMI on UDP (see `ipmi`).

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
    /// The BMC we ask what the replay can't answer, when recording its IPMI.
    pub(crate) ipmi_device: OnceLock<ooblin_core::ipmi::Session>,
    /// Where it answers IPMI, on 127.0.0.1: `serve_ipmi` answers on more.
    pub ipmi_port: u16,
}

impl Bmc {
    /// Serves `fixture` on `address`.
    pub async fn start(address: impl ToSocketAddrs, fixture: &Path) -> Arc<Self> {
        Self::serve(address, fixture, redfish::State::default()).await
    }

    /// Like `start`, with the Redfish half in `redfish`'s state: forwarding to a device, say.
    pub(crate) async fn serve(address: impl ToSocketAddrs, fixture: &Path, redfish: redfish::State) -> Arc<Self> {
        let listener = TcpListener::bind(address).await.unwrap();
        let ipmi = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bmc = Arc::new(Self {
            port: listener.local_addr().unwrap().port(),
            requests: Mutex::default(),
            fixture: fixture.to_owned(),
            redfish,
            ikvm: ikvm::State::default(),
            ipmi: Mutex::new(ipmi::Replay::read(
                &std::fs::read_to_string(fixture.join("ipmi.txt")).unwrap_or_default(),
            )),
            ipmi_device: OnceLock::new(),
            ipmi_port: ipmi.local_addr().unwrap().port(),
        });
        bmc.answer_ipmi(ipmi);
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

    /// Us at our web port and our IPMI port, as `USER` with `password`.
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
