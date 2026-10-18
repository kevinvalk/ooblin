//! A webview over `amt-core`. The webview never holds a socket or a stored password, it calls these commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hosts;

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use amt_core::{
    AuditRecord, Client, Error, Event, Fingerprint, Hardware, Info, Power, PowerAction, Settings, Status, Target,
};
use clap::Parser;
use hosts::{Device, Hosts};
use serde::Serialize;
use tauri::async_runtime::Mutex;
use tauri::{AppHandle, State, WebviewWindow};
use ts_rs::TS;

/// One session per connected device, each on its own lock: AMT wants its calls one at a time, other devices don't care.
#[derive(Default)]
struct Sessions(std::sync::Mutex<HashMap<String, Arc<Mutex<Client>>>>);

impl Sessions {
    fn get(&self, host: &str) -> Result<Arc<Mutex<Client>>, CommandError> {
        let session = self.0.lock().unwrap().get(host).cloned();
        session.ok_or_else(|| CommandError::failed(format!("Not connected to {host}.")))
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

#[tauri::command]
async fn connect(
    app: AppHandle,
    sessions: State<'_, Sessions>,
    host: String,
    user: String,
    password: String,
    remember: bool,
) -> Result<Info, CommandError> {
    let target = Target::new(host, user, password);
    let info = open(&app, &sessions, &target).await?;
    if remember {
        Hosts::open(&app)?.remember(&target)?;
    }
    Ok(info)
}

/// Connects with the password we already have, from the command line or the OS credential store.
#[tauri::command]
async fn reconnect(
    app: AppHandle,
    sessions: State<'_, Sessions>,
    args: State<'_, Args>,
    host: String,
) -> Result<Info, CommandError> {
    let target = match &args.password {
        Some(password) if args.host.as_ref() == Some(&host) => Target::new(host, &args.user, password),
        _ => Hosts::open(&app)?.target(&host)?,
    };
    open(&app, &sessions, &target).await
}

async fn open(app: &AppHandle, sessions: &Sessions, target: &Target) -> Result<Info, CommandError> {
    let mut hosts = Hosts::open(app)?;
    let mut client = Client::new(target, hosts.pin(&target.host))?;
    let info = client.info().await?;
    hosts.connected(target, info.fingerprint)?;
    sessions.0.lock().unwrap().insert(target.host.clone(), Arc::new(Mutex::new(client)));
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
fn trust(app: AppHandle, host: &str, fingerprint: Fingerprint) -> Result<(), CommandError> {
    Ok(Hosts::open(&app)?.trust(host, fingerprint)?)
}

#[tauri::command]
async fn power(sessions: State<'_, Sessions>, host: &str) -> Result<Power, CommandError> {
    Ok(sessions.get(host)?.lock().await.power().await?)
}

#[tauri::command]
async fn set_power(sessions: State<'_, Sessions>, host: &str, action: PowerAction) -> Result<(), CommandError> {
    Ok(sessions.get(host)?.lock().await.set_power(action).await?)
}

#[tauri::command]
async fn status(sessions: State<'_, Sessions>, host: &str) -> Result<Status, CommandError> {
    Ok(sessions.get(host)?.lock().await.status().await?)
}

#[tauri::command]
async fn hardware(sessions: State<'_, Sessions>, host: &str) -> Result<Hardware, CommandError> {
    Ok(sessions.get(host)?.lock().await.hardware().await?)
}

#[tauri::command]
async fn settings(sessions: State<'_, Sessions>, host: &str) -> Result<Settings, CommandError> {
    Ok(sessions.get(host)?.lock().await.settings().await?)
}

#[tauri::command]
async fn events(sessions: State<'_, Sessions>, host: &str) -> Result<Vec<Event>, CommandError> {
    Ok(sessions.get(host)?.lock().await.event_log().await?)
}

#[tauri::command]
async fn audit(sessions: State<'_, Sessions>, host: &str) -> Result<Vec<AuditRecord>, CommandError> {
    Ok(sessions.get(host)?.lock().await.audit_log().await?)
}

/// The window is titled after the session on screen.
#[tauri::command]
fn title(window: WebviewWindow, title: &str) -> Result<(), CommandError> {
    window.set_title(title).map_err(CommandError::failed)
}

/// What the webview gets when a command fails: always a message, and both fingerprints when the certificate changed.
/// `Unreachable` means the device stopped answering, even to a second try.
#[derive(Serialize, TS)]
#[ts(export, export_to = "console.ts")]
#[serde(tag = "kind", rename_all = "camelCase")]
enum CommandError {
    PinMismatch { message: String, expected: Fingerprint, actual: Fingerprint },
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
        match error {
            Error::PinMismatch { expected, actual } => {
                Self::PinMismatch { message: error.to_string(), expected, actual }
            }
            Error::Transport(_) | Error::Timeout => Self::Unreachable { message: error.to_string() },
            error => Self::failed(error),
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
            launch, devices, connect, reconnect, disconnect, forget, trust, power, set_power, status, hardware,
            settings, events, audit, title
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime starts");
}
