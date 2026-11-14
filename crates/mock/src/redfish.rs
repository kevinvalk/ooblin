//! The BMC's Redfish, from the fixture's `redfish/` laid out as DMTF's mockups are (`redfish/v1/Systems/1/index.json`),
//! and recording one.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use ooblin_core::supermicro::{Client, Connection, probe};
use ooblin_core::{Error, Target};
use serde_json::Value;

use crate::{Bmc, PASSWORD, scrub};

/// `USER:PASSWORD` in Base64.
const BASIC: &str = "Basic YWRtaW46UEBzc3cwcmQ=";

/// The Redfish half's own: the device we forward to, when recording, and the settings changed since we started.
#[derive(Default)]
pub(crate) struct State {
    device: Option<Connection>,
    changed: std::sync::Mutex<HashMap<String, Value>>,
}

/// The settings a PATCH changes, as the X10's own pages do.
const SETTINGS: [&str; 4] = [
    "redfish/v1/Managers/1/FanMode",
    "redfish/v1/Managers/1/MouseMode",
    "redfish/v1/Managers/1/NTP",
    "redfish/v1/Managers/1/Syslog",
];

/// As the X10: the root without a login, the rest with Basic (a 401 without a challenge), a 404 as a web page. Of
/// writes the two our power changes make, which change nothing here, and the settings' (`SETTINGS`), which do.
pub(crate) async fn serve(bmc: &Arc<Bmc>, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let (method, path) = (request.method().clone(), request.uri().path().to_owned());
    let login = request.headers().get(AUTHORIZATION).map(|header| header == BASIC);
    let body = request.into_body().collect().await.map(|body| body.to_bytes()).unwrap_or_default();
    let mut line = format!("{method} {path}");
    if login.is_none() {
        line += " (no login)";
    }
    if !body.is_empty() {
        line = format!("{line} {}", String::from_utf8_lossy(&body));
    }
    bmc.requests.lock().unwrap().push(line);

    let resource = path.trim_matches('/');
    if login != Some(true) && resource != "redfish/v1" {
        let error = r#"{"error":{"code":"Base.v1_4_0.GeneralError","@Message.ExtendedInfo":[{"MessageId":"Base.v1_4_0.ResourceAtUriUnauthorized"}]}}"#;
        return answer(StatusCode::UNAUTHORIZED, error);
    }
    let file = bmc.fixture.join(resource).join("index.json");
    let read = || -> Option<Value> {
        let changed = bmc.redfish.changed.lock().unwrap().get(resource).cloned();
        changed.or_else(|| serde_json::from_slice(&std::fs::read(&file).ok()?).ok())
    };
    match (method, resource) {
        (Method::GET, _) if resource.split('/').any(|segment| segment == "..") => not_found(),
        (Method::GET, _) => match &bmc.redfish.device {
            Some(device) => forward(device, &path, &file).await,
            None => read().map_or_else(not_found, |json| answer(StatusCode::OK, json.to_string())),
        },
        (Method::PATCH, setting) if SETTINGS.contains(&setting) => {
            let (Some(mut current), Ok(Value::Object(change))) = (read(), serde_json::from_slice::<Value>(&body))
            else {
                return answer(StatusCode::BAD_REQUEST, "");
            };
            let allowed = current.get("Mode@Redfish.AllowableValues").and_then(Value::as_array).cloned();
            let known = change.iter().all(|(field, value)| {
                current.get(field).is_some() && allowed.as_ref().is_none_or(|allowed| allowed.contains(value))
            });
            if !known {
                return answer(StatusCode::BAD_REQUEST, "");
            }
            current.as_object_mut().unwrap().extend(change);
            bmc.redfish.changed.lock().unwrap().insert(setting.to_owned(), current);
            answer(StatusCode::NO_CONTENT, "")
        }
        (Method::POST, "redfish/v1/Systems/1/Actions/ComputerSystem.Reset") if resets(&bmc.fixture, &body) => {
            answer(StatusCode::NO_CONTENT, "")
        }
        (Method::PATCH, "redfish/v1/Systems/1") => answer(StatusCode::NO_CONTENT, ""),
        _ => answer(StatusCode::METHOD_NOT_ALLOWED, ""),
    }
}

/// Whether the system allows the `ResetType` asked for.
fn resets(fixture: &Path, body: &[u8]) -> bool {
    let system = std::fs::read(fixture.join("redfish/v1/Systems/1/index.json")).unwrap_or_default();
    let (Ok(system), Ok(body)) = (serde_json::from_slice::<Value>(&system), serde_json::from_slice::<Value>(body))
    else {
        return false;
    };
    let allowed = system.pointer("/Actions/#ComputerSystem.Reset/ResetType@Redfish.AllowableValues");
    allowed.and_then(Value::as_array).is_some_and(|allowed| allowed.contains(&body["ResetType"]))
}

/// Recording: the device answers, and we keep what it said, scrubbed, where the replay will look for it. We answer
/// what we keep, so the fixture holds exactly what we read.
async fn forward(device: &Connection, path: &str, file: &Path) -> Response<Full<Bytes>> {
    match device.get::<Value>(path).await {
        Ok(mut json) => {
            scrub::json(&mut json);
            let mut json = serde_json::to_vec_pretty(&json).unwrap();
            json.push(b'\n');
            std::fs::create_dir_all(file.parent().unwrap()).and_then(|()| std::fs::write(file, &json)).unwrap();
            answer(StatusCode::OK, json)
        }
        Err(Error::Http(status)) => answer(StatusCode::from_u16(status).unwrap(), ""),
        Err(error) => answer(StatusCode::BAD_GATEWAY, error.to_string()),
    }
}

fn answer(status: StatusCode, json: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder().status(status).header(CONTENT_TYPE, "application/json").body(Full::new(json.into())).unwrap()
}

fn not_found() -> Response<Full<Bytes>> {
    let page = Response::builder().status(StatusCode::NOT_FOUND).header(CONTENT_TYPE, "text/html");
    page.body(Full::new("<html><body>Not Found</body></html>".into())).unwrap()
}

impl Bmc {
    /// Changes the Redfish resource at `resource` as `change` says, what it reads from then.
    pub(crate) fn change_redfish(&self, resource: &str, change: impl FnOnce(&mut Value)) {
        let mut changed = self.redfish.changed.lock().unwrap();
        let file = self.fixture.join(resource).join("index.json");
        let mut json = changed
            .get(resource)
            .cloned()
            .unwrap_or_else(|| serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap());
        change(&mut json);
        changed.insert(resource.to_owned(), json);
    }
}

/// Records `target`'s Redfish into `fixture`: what our client reads of it, scrubbed.
pub async fn record(target: &Target, fixture: &Path) -> anyhow::Result<()> {
    let device = Some(Connection::new(target, None)?);
    let proxy = Bmc::serve("127.0.0.1:0", fixture, State { device, ..State::default() }).await;
    let proxied = proxy.target(PASSWORD);
    eprint!("Redfish  recording…");
    anyhow::ensure!(probe(&proxied.host).await, "{} doesn't answer Redfish", target.host);
    let mut client = Client::new(&proxied, None)?;
    client.status().await?;
    client.panel().await.ok();
    client.power().await?;
    client.hardware().await?;
    client.event_log().await?;
    client.bmc_settings().await?;
    eprintln!("\rRedfish  {} requests", proxy.requests.lock().unwrap().len());
    Ok(())
}
