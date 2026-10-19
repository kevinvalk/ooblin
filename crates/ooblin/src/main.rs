//! A webview over `amt-core`. The webview never holds a socket or a stored password, it calls these commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hosts;

use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use amt_core::{
    AuditRecord, Change, Client, Error, Event, Found, Hardware, Info, Input, Kvm, Power, PowerAction, RMCP_PORT,
    Screens, Settings, Sol, Status, Target,
};
use clap::Parser;
use hosts::{Device, Hosts};
use serde::Serialize;
use tauri::async_runtime::{JoinHandle, Mutex};
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, State, WebviewWindow};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_window_state::StateFlags;
use ts_rs::TS;

/// One session per connected device: its client, on its own lock as AMT wants its calls one at a time (other devices
/// don't care), its screen and serial console while they're open and the disk image in its drive.
#[derive(Default)]
struct Sessions(std::sync::Mutex<HashMap<String, Session>>);

#[derive(Clone)]
struct Session {
    client: Arc<Mutex<Client>>,
    kvm: Option<Arc<Kvm>>,
    sol: Option<Arc<Sol>>,
    image: Option<Arc<Task>>,
}

/// A task that stops with the last copy of it: one serving a disk image, or looking for devices.
struct Task(JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Sessions {
    fn get(&self, host: &str) -> Result<Session, CommandError> {
        let session = self.0.lock().unwrap().get(host).cloned();
        session.ok_or_else(|| CommandError::failed(format!("Not connected to {host}.")))
    }

    fn kvm(&self, host: &str) -> Result<Arc<Kvm>, CommandError> {
        self.get(host)?.kvm.ok_or_else(|| CommandError::failed("The screen isn't open."))
    }

    fn sol(&self, host: &str) -> Result<Arc<Sol>, CommandError> {
        self.get(host)?.sol.ok_or_else(|| CommandError::failed("The serial console isn't open."))
    }

    fn update<T>(&self, host: &str, change: impl FnOnce(&mut Session) -> T) -> Option<T> {
        self.0.lock().unwrap().get_mut(host).map(change)
    }

    async fn close_kvm(&self, host: &str) {
        if let Some(kvm) = self.update(host, |session| session.kvm.take()).flatten() {
            kvm.close().await;
        }
    }

    async fn close_sol(&self, host: &str) {
        if let Some(sol) = self.update(host, |session| session.sol.take()).flatten() {
            sol.close().await;
        }
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
    let (user, remembered) = (args.user.clone(), args.password.is_some());
    Ok(Some(saved.unwrap_or(Device { host, name: String::new(), version: String::new(), user, remembered })))
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
    hosts.connected(&target, &info)?;
    if remember {
        hosts.remember(&target)?;
    }
    sessions
        .0
        .lock()
        .unwrap()
        .insert(target.host, Session { client: Arc::new(Mutex::new(client)), kvm: None, sol: None, image: None });
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

/// Whether this is the verify build (AGENTS.md), which keeps to the mock and off the user's screen.
fn verify(app: &AppHandle) -> bool {
    app.config().identifier.ends_with(".verify")
}

/// The network to look for devices on unless told otherwise, see `amt_core::nearby`. The verify build looks on this
/// machine only, where the mock is, never at the user's devices.
#[tauri::command]
fn networks(app: AppHandle) -> String {
    if verify(&app) { "127.0.0.1/32".into() } else { amt_core::nearby() }
}

/// The search for devices underway, see `discover`.
#[derive(Default)]
struct Discovery(std::sync::Mutex<Option<Task>>);

/// Looks for devices on `networks` and says each on `found` as it answers, see `amt_core::discover`. It ends the
/// search before, as does `stop_discovery`.
#[tauri::command]
async fn discover(
    discovery: State<'_, Discovery>,
    networks: String,
    found: Channel<Found>,
) -> Result<(), CommandError> {
    let (done, mut ended) = tauri::async_runtime::channel(1);
    let search = async move {
        let result = amt_core::discover(&networks, RMCP_PORT, |device| found.send(device).unwrap_or_default()).await;
        done.send(result).await.unwrap_or_default();
    };
    *discovery.0.lock().unwrap() = Some(Task(tauri::async_runtime::spawn(search)));
    // A stopped search never says how it went.
    Ok(ended.recv().await.unwrap_or(Ok(()))?)
}

#[tauri::command]
fn stop_discovery(discovery: State<'_, Discovery>) {
    discovery.0.lock().unwrap().take();
}

#[tauri::command]
async fn set_power(sessions: State<'_, Sessions>, host: &str, action: PowerAction) -> Result<(), CommandError> {
    let session = sessions.get(host)?;
    let serving = session.image.is_some_and(|image| !image.0.inner().is_finished());
    if matches!(action, PowerAction::OnToImage | PowerAction::ResetToImage) && !serving {
        return Err(CommandError::failed("No disk image is mounted, mount one again."));
    }
    Ok(session.client.lock().await.set_power(action).await?)
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

/// Changes a setting and says them all after, see `Client::change`. A disk image goes when they're turned off, the
/// webview closes the screen and serial console. A new name goes in the device list.
#[tauri::command]
async fn change(
    app: AppHandle,
    sessions: State<'_, Sessions>,
    host: &str,
    change: Change,
) -> Result<Settings, CommandError> {
    let client = sessions.get(host)?.client;
    let mut client = client.lock().await;
    client.change(&change).await?;
    if let Change::RemoteAccess(access) = change
        && !access.ider
    {
        sessions.update(host, |session| session.image = None);
    }
    let settings = client.settings().await?;
    Hosts::open(&app)?.renamed(host, &settings.general.name)?;
    Ok(settings)
}

/// Writes the certificate as PEM to `path`.
#[tauri::command]
async fn save_certificate(
    sessions: State<'_, Sessions>,
    host: &str,
    id: &str,
    path: PathBuf,
) -> Result<(), CommandError> {
    let pem = sessions.get(host)?.client.lock().await.certificate(id).await?;
    Ok(std::fs::write(path, pem)?)
}

/// Opens AMT's own web pages in the browser.
#[tauri::command]
async fn web_pages(app: AppHandle, sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    let url = sessions.get(host)?.client.lock().await.web_pages();
    app.opener().open_url(url, None::<&str>).map_err(CommandError::failed)
}

/// Has the device show a code for someone at it to read out, see `Client::start_consent`.
#[tauri::command]
async fn consent_start(sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    Ok(sessions.get(host)?.client.lock().await.start_consent().await?)
}

#[tauri::command]
async fn consent_send(sessions: State<'_, Sessions>, host: &str, code: u32) -> Result<(), CommandError> {
    Ok(sessions.get(host)?.client.lock().await.send_consent(code).await?)
}

#[tauri::command]
async fn consent_cancel(sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    Ok(sessions.get(host)?.client.lock().await.cancel_consent().await?)
}

/// Opens the device's screen, see `Client::kvm`, closing the one that may be open: AMT takes one at a time.
#[tauri::command]
async fn kvm_open(sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    sessions.close_kvm(host).await;
    let kvm = Arc::new(sessions.get(host)?.client.lock().await.kvm().await?);
    sessions.update(host, |session| session.kvm = Some(kvm));
    Ok(())
}

/// See `Client::screens`.
#[tauri::command]
async fn screens(sessions: State<'_, Sessions>, host: &str) -> Result<Screens, CommandError> {
    Ok(sessions.get(host)?.client.lock().await.screens().await?)
}

#[tauri::command]
async fn show_screen(sessions: State<'_, Sessions>, host: &str, screen: u8) -> Result<(), CommandError> {
    Ok(sessions.get(host)?.client.lock().await.show_screen(screen).await?)
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

#[tauri::command]
async fn kvm_close(sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    sessions.close_kvm(host).await;
    Ok(())
}

/// Opens the device's serial console, see `Client::sol`, closing the one that may be open.
#[tauri::command]
async fn sol_open(sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    sessions.close_sol(host).await;
    let sol = Arc::new(sessions.get(host)?.client.lock().await.sol().await?);
    sessions.update(host, |session| session.sol = Some(sol));
    Ok(())
}

/// What the device wrote to its serial port since, see `Sol::read`, as bytes rather than JSON.
#[tauri::command]
async fn sol_read(sessions: State<'_, Sessions>, host: &str) -> Result<Response, CommandError> {
    Ok(Response::new(sessions.sol(host)?.read().await?))
}

#[tauri::command]
async fn sol_write(sessions: State<'_, Sessions>, host: &str, data: String) -> Result<(), CommandError> {
    Ok(sessions.sol(host)?.write(data.as_bytes()).await?)
}

#[tauri::command]
async fn sol_close(sessions: State<'_, Sessions>, host: &str) -> Result<(), CommandError> {
    sessions.close_sol(host).await;
    Ok(())
}

/// Asks for a file to open, of a `kind` ("Disk image") and its `extensions`. Native dialogs want the main thread, where
/// a command that isn't async runs. The verify build never shows one, it picks the file `OOBLIN_PICK` names.
#[tauri::command]
fn pick(app: AppHandle, window: WebviewWindow, kind: &str, extensions: Vec<String>) -> Option<PathBuf> {
    if verify(&app) {
        return std::env::var_os("OOBLIN_PICK").map(PathBuf::from);
    }
    rfd::FileDialog::new().set_parent(&window).add_filter(kind, &extensions).pick_file()
}

/// Asks where to save a file, suggesting `name`, see `pick`.
#[tauri::command]
fn pick_save(app: AppHandle, window: WebviewWindow, name: &str) -> Option<PathBuf> {
    if verify(&app) {
        return std::env::var_os("OOBLIN_PICK").map(PathBuf::from);
    }
    rfd::FileDialog::new().set_parent(&window).set_file_name(name).save_file()
}

/// Puts the image at `path` in the device's drive (see `Client::ider`) and says its name and size, then what of it the
/// device reads on `reads` as it goes, and nothing once the device closed the drive. AMT has one drive, so the image in
/// it comes out first. Ejecting it says nothing, the webview knows.
#[tauri::command]
async fn ider_mount(
    sessions: State<'_, Sessions>,
    host: &str,
    path: PathBuf,
    reads: Channel<Option<Range<u64>>>,
) -> Result<(String, u64), CommandError> {
    let file = std::fs::File::open(&path)?;
    let size = file.metadata()?.len();
    sessions.update(host, |session| session.image = None);
    let ider = sessions.get(host)?.client.lock().await.ider(file).await?;
    let ended = reads.clone();
    let serve = ider.serve(move |read| reads.send(Some(read)).unwrap_or_default());
    let image = Arc::new(Task(tauri::async_runtime::spawn(async move {
        // How it ended doesn't matter, that it did does.
        serve.await.unwrap_or_default();
        ended.send(None).unwrap_or_default();
    })));
    sessions.update(host, |session| session.image = Some(image));
    Ok((path.file_name().unwrap_or_default().to_string_lossy().into_owned(), size))
}

#[tauri::command]
fn ider_eject(sessions: State<'_, Sessions>, host: &str) {
    sessions.update(host, |session| session.image = None);
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
/// `Unreachable` means the device stopped answering, even to a second try, `Consent` that someone at it has to read out
/// a code first (see `consent_start`).
#[derive(Serialize, TS)]
#[ts(export, export_to = "ooblin.ts")]
#[serde(tag = "kind")]
enum CommandError {
    PinMismatch { message: String, expected: String, actual: String },
    Unreachable { message: String },
    Consent { message: String },
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
            Error::Consent => Self::Consent { message },
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
        .plugin(tauri_plugin_opener::init())
        // The window opens where and as large as it was, but neither hidden (the verify build's) nor full screen.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
                .build(),
        )
        .manage(Args::parse())
        .manage(Sessions::default())
        .manage(Discovery::default())
        .invoke_handler(tauri::generate_handler![
            launch,
            devices,
            connect,
            disconnect,
            forget,
            trust,
            networks,
            discover,
            stop_discovery,
            power,
            set_power,
            status,
            hardware,
            settings,
            change,
            consent_start,
            consent_send,
            consent_cancel,
            save_certificate,
            web_pages,
            event_log,
            audit_log,
            kvm_open,
            screens,
            show_screen,
            kvm_frame,
            kvm_input,
            kvm_close,
            sol_open,
            sol_read,
            sol_write,
            sol_close,
            pick,
            pick_save,
            ider_mount,
            ider_eject,
            title,
            fullscreen
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime starts");
}
