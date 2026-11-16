use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{Pin, http, ok, transport};
use crate::{Error, Result, Target};

pub struct Connection {
    pub(super) http: reqwest::Client,
    pub(super) url: String, // `https://host[:port]`
    pub(super) user: String,
    pub(super) password: String,
    pub(super) pin: Arc<Pin>,

    // Set by a 401, after which we send nothing more: a wrong password that is retried locks the account!
    pub(super) is_refused: AtomicBool,

    // Set by a refused web login, which we do not try again either. Redfish goes on, because it took the password, so
    // the pages refuse us for a reason of their own.
    pub(super) is_web_refused: AtomicBool,
}

impl Connection {
    /// Connects lazily and pins as `amt::Connection` does.
    pub fn new(target: &Target, pin: Option<String>) -> Result<Self> {
        let pin = Pin::new(pin);

        Ok(Self {
            http: http(&pin)?,
            url: format!("https://{}", target.authority()),
            user: target.user.clone(),
            password: target.password.clone(),
            pin,
            is_refused: AtomicBool::new(false),
            is_web_refused: AtomicBool::new(false),
        })
    }

    /// The pinned certificate, which we only know once the first connection was made.
    pub fn fingerprint(&self) -> Option<String> {
        self.pin.pinned.get().cloned()
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Reads the resource at `path` (`/redfish/v1/Systems/1`). We try twice if the connection broke, as AMT's reads do.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = match self.send(Method::GET, path, None).await {
            Err(Error::Transport(_)) => self.send(Method::GET, path, None).await,
            response => response,
        }?;
        let body = response.bytes().await.map_err(transport)?;

        serde_json::from_slice(&body).map_err(|error| Error::Protocol(format!("{path}: {error}")))
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<()> {
        self.send(Method::POST, path, Some(body)).await.map(drop)
    }

    pub async fn patch(&self, path: &str, body: &Value) -> Result<()> {
        self.send(Method::PATCH, path, Some(body)).await.map(drop)
    }

    async fn send(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Response> {
        if self.is_refused.load(Relaxed) {
            return Err(Error::Unauthorized);
        }

        // The X10 answers a 401 without a challenge, so we send Basic up front with every request.
        let mut request =
            self.http.request(method, format!("{}{path}", self.url)).basic_auth(&self.user, Some(&self.password));
        if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json").body(body.to_string());
        }

        let response = request.send().await.map_err(|error| self.pin.mismatch().unwrap_or_else(|| transport(error)))?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.is_refused.store(true, Relaxed);
            return Err(Error::Unauthorized);
        }

        ok(response)
    }
}

/// Whether `host` answers Redfish without a login, as its root must (DSP0266). It is one request, which carries no
/// password.
pub async fn probe(host: &str) -> bool {
    let Ok(http) = http(&Pin::new(None)) else { return false };

    let root =
        async { http.get(format!("https://{host}/redfish/v1/")).send().await?.error_for_status()?.bytes().await };
    let Ok(Ok(root)) = tokio::time::timeout(Duration::from_secs(3), root).await else { return false };

    serde_json::from_slice::<Value>(&root).is_ok_and(|root| root.get("RedfishVersion").is_some())
}
