use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use ooblin_core::redfish::{Connection, probe};
use ooblin_core::{Client, Error, Flavor, Target};
use serde_json::{Value, json};

use crate::{Bmc, PASSWORD, scrub};

const BASIC: &str = "Basic YWRtaW46UEBzc3cwcmQ="; // `USER:PASSWORD` in Base64

/// What the Redfish half keeps for itself: the device we forward to when recording, and the settings changed since we
/// started.
#[derive(Default)]
pub(crate) struct State {
    device: Option<Connection>,
    changed: std::sync::Mutex<HashMap<String, Value>>,
}

/// The settings a PATCH changes, as the pages of the X10 itself do.
const SETTINGS: [&str; 4] = [
    "redfish/v1/Managers/1/FanMode",
    "redfish/v1/Managers/1/MouseMode",
    "redfish/v1/Managers/1/NTP",
    "redfish/v1/Managers/1/Syslog",
];

/// Answers as the X10 does: the root without a login, the rest with Basic (a 401 without a challenge), a 404 as a web
/// page. Of writes we take the two our power changes make to a server, of which only its identify light changes here,
/// and those of the settings (`SETTINGS`), which do change.
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

    // NOTE: The `redfish/` of a fixture is laid out as the mockups of DMTF are (`redfish/v1/Systems/1/index.json`).
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
            let is_known = change.iter().all(|(field, value)| {
                current.get(field).is_some() && allowed.as_ref().is_none_or(|allowed| allowed.contains(value))
            });
            // We refuse as the A2SDi does, which says why only in its extended messages.
            if !is_known {
                let error = json!({ "error": {
                    "code": "Base.v1_4_0.GeneralError",
                    "Message": "A general error has occurred. See ExtendedInfo for more information.",
                    "@Message.ExtendedInfo": [{
                        "MessageId": "Base.v1_4_0.PropertyValueNotInList",
                        "Message": "The value for the property is not in the list of acceptable values.",
                    }],
                }});
                return answer(StatusCode::BAD_REQUEST, error.to_string());
            }

            current.as_object_mut().unwrap().extend(change);
            bmc.redfish.changed.lock().unwrap().insert(setting.to_owned(), current);
            answer(StatusCode::NO_CONTENT, "")
        }
        (Method::POST, reset) if is_reset_allowed(&bmc.fixture, reset, &body) => answer(StatusCode::NO_CONTENT, ""),
        (Method::PATCH, system) if system.starts_with("redfish/v1/Systems/") => {
            let (Some(mut current), Ok(Value::Object(change))) = (read(), serde_json::from_slice::<Value>(&body))
            else {
                return not_found();
            };

            let lights = change.into_iter().filter(|(field, _)| field.contains("Indicator"));
            current.as_object_mut().unwrap().extend(lights);
            bmc.redfish.changed.lock().unwrap().insert(system.to_owned(), current);
            answer(StatusCode::NO_CONTENT, "")
        }
        _ => answer(StatusCode::METHOD_NOT_ALLOWED, ""),
    }
}

/// Is `reset` the Reset of a server, and does the server allow the `ResetType` asked for? It says which it allows in
/// itself, or in a resource of their own.
fn is_reset_allowed(fixture: &Path, reset: &str, body: &[u8]) -> bool {
    let read = |resource: &str| -> Option<Value> {
        serde_json::from_slice(&std::fs::read(fixture.join(resource.trim_matches('/')).join("index.json")).ok()?).ok()
    };
    let allowed = || -> Option<Vec<Value>> {
        let system = read(reset.strip_suffix("/Actions/ComputerSystem.Reset")?)?;
        let action = system.pointer("/Actions/#ComputerSystem.Reset")?;
        let allowed = match action.get("@Redfish.ActionInfo").and_then(Value::as_str) {
            Some(info) => read(info)?.pointer("/Parameters/0/AllowableValues")?.clone(),
            None => action.get("ResetType@Redfish.AllowableValues")?.clone(),
        };
        allowed.as_array().cloned()
    };
    let asked = serde_json::from_slice::<Value>(body).unwrap_or_default();

    allowed().is_some_and(|allowed| allowed.contains(&asked["ResetType"]))
}

/// When recording, the device answers and we keep what it said, scrubbed, where the replay will look for it. We answer
/// with what we keep, so that the fixture holds exactly what we read.
async fn forward(device: &Connection, path: &str, file: &Path) -> Response<Full<Bytes>> {
    match device.get::<Value>(path).await {
        Ok(mut json) => {
            scrub::json(&mut json);
            let mut json = serde_json::to_vec_pretty(&json).unwrap();
            json.push(b'\n');
            std::fs::create_dir_all(file.parent().unwrap()).and_then(|()| std::fs::write(file, &json)).unwrap();
            answer(StatusCode::OK, json)
        }
        Err(Error::Http(status) | Error::Redfish { status, .. }) => answer(StatusCode::from_u16(status).unwrap(), ""),
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
    /// Changes the Redfish resource at `resource` as `change` says, which is what it reads as from then on.
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

/// Records the Redfish of `target` into `fixture`: what our client reads of it as a device of `flavor`, scrubbed.
pub async fn record(target: &Target, fixture: &Path, flavor: Flavor) -> anyhow::Result<()> {
    let device = Some(Connection::new(target, None)?);
    let proxy = Bmc::serve("127.0.0.1:0", fixture, State { device, ..State::default() }).await;
    let proxy_target = proxy.target(PASSWORD);

    eprint!("Redfish  recording…");
    anyhow::ensure!(probe(&proxy_target.host).await, "{} doesn't answer Redfish", target.host);

    let (mut client, _) = Client::login(&proxy_target, None, Some(flavor)).await?;
    client.bmc_status().await?;
    client.panel().await.ok();
    client.power().await?;
    client.hardware().await?;
    client.event_log().await?;
    match &mut client {
        // We read the sensors of a Supermicro over IPMI, and keep what its Redfish says of them too.
        Client::Supermicro(client) => {
            client.bmc_settings().await?;
            client.redfish().sensors().await?;
        }
        client => drop(client.sensors().await?),
    }

    eprintln!("\rRedfish  {} requests", proxy.requests.lock().unwrap().len());
    Ok(())
}
