//! A webview over `amt-core`. The webview never holds a socket or a stored password, it calls these commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hosts;

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use amt_core::{
    AuditRecord, Client, Error, Event, Hardware, Info, Input, Kvm, Power, PowerAction, Settings, Status, Target,
};
use clap::Parser;
use hosts::{Device, Hosts};
use serde::Serialize;
use tauri::async_runtime::Mutex;
use tauri::ipc::Response;
use tauri::{AppHandle, State, WebviewWindow};
use ts_rs::TS;

/// One session per connected device: its client, on its own lock as AMT wants its calls one at a time (other devices
/// don't care), and its screen while one is open.
#[derive(Default)]
struct Sessions(std::sync::Mutex<HashMap<String, Session>>);

#[derive(Clone)]
struct Session {
    client: Arc<Mutex<Client>>,
    kvm: Option<Arc<Kvm>>,
}

impl Sessions {
    fn get(&self, host: &str) -> Result<Session, CommandError> {
        let session = self.0.lock().unwrap().get(host).cloned();
        session.ok_or_else(|| CommandError::failed(format!("Not connected to {host}.")))
    }

    fn kvm(&self, host: &str) -> Result<Arc<Kvm>, CommandError> {
        self.get(host)?.kvm.ok_or_else(|| CommandError::failed("The screen isn't open."))
    }
}

/// A desktop console for Intel AMT.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Connect to this host on start, with its saved credentials unless --password is given.
    host: Option<String>,
    #[arg(long, default_value = "admin")]
    user: String,
    #[arg(long)]
    password: Option<String>,
}

/// The device to open on start, if any. The webview connects to it like a click in the sidebar would.
#[tauri::command]
fn launch(app: AppHandle, args: State<'_, Args>) -> Result<Option<Device>, CommandError> {
    let Some(host) = args.host.clone() else { return Ok(None) };
    let saved = Hosts::open(&app)?.device(&host).filter(|_| args.password.is_none());
    Ok(Some(saved.unwrap_or(Device { host, user: args.user.clone(), remembered: args.password.is_some() })))
}

#[tauri::command]
fn devices(app: AppHandle) -> Result<Vec<Device>, CommandError> {
    Ok(Hosts::open(&app)?.devices())
}

/// Without a `password`, with the one we have: from the command line or the OS credential store. `remember` keeps
/// a typed one there.
#[tauri::command]
async fn connect(
    app: AppHandle,
    sessions: State<'_, Sessions>,
    args: State<'_, Args>,
    host: String,
    user: String,
    password: Option<String>,
    remember: bool,
) -> Result<Info, CommandError> {
    let mut hosts = Hosts::open(&app)?;
    let remember = remember && password.is_some();
    let target = match (password, &args.password) {
        (Some(password), _) => Target::new(host, user, password),
        (None, Some(password)) if args.host.as_ref() == Some(&host) => Target::new(host, &args.user, password),
        (None, _) => hosts.target(&host)?,
    };
    let mut client = Client::new(&target, hosts.pin(&target.host))?;
    let info = client.info().await?;
    hosts.connected(&target, info.fingerprint.clone())?;
    if remember {
        hosts.remember(&target)?;
    }
    sessions.0.lock().unwrap().insert(target.host, Session { client: Arc::new(Mutex::new(client)), kvm: None });
    Ok(info)
}

#[tauri::command]
fn disconnect(sessions: State<'_, Sessions>, host: &str) {
    sessions.0.lock().unwrap().remove(host);
}

#[tauri::command]
fn forget(app: AppHandle, sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    disconnect(sessions, host);
    Hosts::open(&app)?.forget(host)
}

#[tauri::command]
fn trust(app: AppHandle, host: &str, fingerprint: String) -> Result<(), CommandError> {
    Ok(Hosts::open(&app)?.trust(host, fingerprint)?)
}

#[tauri::command]
async fn set_power(sessions: State<'_, Sessions>, host: &str, action: PowerAction) -> Result<(), CommandError> {
    Ok(sessions.get(host)?.client.lock().await.set_power(action).await?)
}

/// What the UI reads from a session: a command per `Client` read, named after it.
macro_rules! reads {
    ($($name:ident: $type:ty),*) => {$(
        #[tauri::command]
        async fn $name(sessions: State<'_, Sessions>, host: &str) -> Result<$type, CommandError> {
            Ok(sessions.get(host)?.client.lock().await.$name().await?)
        }
    )*};
}

reads!(power: Power, status: Status, hardware: Hardware, settings: Settings, event_log: Vec<Event>, audit_log: Vec<AuditRecord>);

/// Opens the device's screen, see `Client::kvm`. One at a time per device, a second replaces the first.
#[tauri::command]
async fn kvm_open(sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    let kvm = sessions.get(host)?.client.lock().await.kvm().await?;
    if let Some(session) = sessions.0.lock().unwrap().get_mut(host) {
        session.kvm = Some(Arc::new(kvm));
    }
    Ok(())
}

/// The next change on screen, as bytes rather than JSON (see `Kvm::frame`).
#[tauri::command]
async fn kvm_frame(sessions: State<'_, Sessions>, host: &str) -> Result<Response, CommandError> {
    Ok(Response::new(sessions.kvm(host)?.frame().await?))
}

#[tauri::command]
async fn kvm_input(sessions: State<'_, Sessions>, host: &str, inputs: Vec<Input>) -> Result<(), CommandError> {
    Ok(sessions.kvm(host)?.input(inputs).await?)
}

/// The session ends once a `kvm_frame` still waiting is done with it.
#[tauri::command]
fn kvm_close(sessions: State<'_, Sessions>, host: &str) {
    if let Some(session) = sessions.0.lock().unwrap().get_mut(host) {
        session.kvm = None;
    }
}

/// The window is titled after the session on screen.
#[tauri::command]
fn title(window: WebviewWindow, title: &str) -> Result<(), CommandError> {
    window.set_title(title).map_err(CommandError::failed)
}

/// The window fills the screen, for the KVM's own full screen.
#[tauri::command]
fn fullscreen(window: WebviewWindow, on: bool) -> Result<(), CommandError> {
    window.set_fullscreen(on).map_err(CommandError::failed)
}

/// What the webview gets when a command fails: always a message, and both fingerprints when the certificate changed.
/// `Unreachable` means the device stopped answering, even to a second try.
#[derive(Serialize, TS)]
#[ts(export, export_to = "console.ts")]
#[serde(tag = "kind")]
enum CommandError {
    PinMismatch { message: String, expected: String, actual: String },
    Unreachable { message: String },
    Failed { message: String },
}

impl CommandError {
    fn failed(message: impl Display) -> Self {
        Self::Failed { message: message.to_string() }
    }
}

impl From<Error> for CommandError {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::PinMismatch { expected, actual } => Self::PinMismatch { message, expected, actual },
            Error::Transport(_) | Error::Timeout => Self::Unreachable { message },
            _ => Self::Failed { message },
        }
    }
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        Self::failed(error)
    }
}

impl From<keyring::Error> for CommandError {
    fn from(error: keyring::Error) -> Self {
        Self::failed(format_args!("Keychain: {error}"))
    }
}

fn main() {
    let builder = tauri::Builder::default();
    // Debug builds let agents drive the app through a localhost WebSocket (mcp-server-tauri), release builds don't.
    #[cfg(debug_assertions)]
    let builder =
        builder.plugin(tauri_plugin_mcp_bridge::Builder::new().bind_address("127.0.0.1").build()).setup(|app| {
            let bridge =
                tauri::ipc::CapabilityBuilder::new("mcp-bridge").window("main").permission("mcp-bridge:default");
            Ok(tauri::Manager::add_capability(app, bridge)?)
        });

    builder
        .manage(Args::parse())
        .manage(Sessions::default())
        .invoke_handler(tauri::generate_handler![
            launch, devices, connect, disconnect, forget, trust, power, set_power, status, hardware, settings,
            event_log, audit_log, kvm_open, kvm_frame, kvm_input, kvm_close, title, fullscreen
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime starts");
}
