//! A webview over `amt-core`. The webview never holds a socket or a stored password, it calls these commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod connections;
mod hosts;

use std::fmt::Display;
use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use amt_core::{
    AuditRecord, Error, Event, Found, Hardware, Ider, Input, Power, PowerAction, RMCP_PORT, Screens, Settings, Status,
    Target,
};
use clap::Parser;
use connections::{Change, Changed, Connected, Connections, Task};
use hosts::Device;
use serde::Serialize;
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_log::RotationStrategy;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_window_state::StateFlags;
use ts_rs::TS;

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

/// What we were started with last, which a second start with a host replaces (see `main`).
struct Launch(std::sync::Mutex<Args>);

impl Launch {
    /// The login the command line gives `host`, if it gives a password.
    fn login(&self, host: &str) -> Option<Target> {
        let args = self.0.lock().unwrap();
        let password = args.password.clone().filter(|_| args.host.as_deref() == Some(host))?;
        Some(Target::new(host, &args.user, password))
    }
}

/// The device the command line names, if any. The webview opens it like a click in the sidebar would, on start and when
/// we're started again.
#[tauri::command]
fn launch(connections: State<'_, Connections>, launch: State<'_, Launch>) -> Result<Option<Device>, CommandError> {
    let args = launch.0.lock().unwrap();
    let Some(host) = args.host.clone() else { return Ok(None) };
    let saved = connections.hosts()?.device(&host).filter(|_| args.password.is_none());
    let (user, remembered) = (args.user.clone(), args.password.is_some());
    Ok(Some(saved.unwrap_or(Device { host, name: String::new(), version: String::new(), user, remembered })))
}

#[tauri::command]
fn devices(connections: State<'_, Connections>) -> Result<Vec<Device>, CommandError> {
    Ok(connections.hosts()?.devices())
}

/// Without a `password`, with the one we have: from the command line or the OS credential store. `remember` keeps
/// a typed one there.
#[tauri::command]
async fn connect(
    connections: State<'_, Connections>,
    launch: State<'_, Launch>,
    host: String,
    user: String,
    password: Option<String>,
    remember: bool,
) -> Result<Connected, CommandError> {
    let remember = remember && password.is_some();
    let target = match (password, launch.login(&host)) {
        (Some(password), _) => Target::new(host, user, password),
        (None, Some(target)) => target,
        (None, None) => connections.hosts()?.target(&host)?,
    };
    connections.connect(target, remember).await
}

#[tauri::command]
async fn disconnect(connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    connections.close(host).await;
    Ok(())
}

#[tauri::command]
async fn forget(connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    connections.forget(host).await
}

#[tauri::command]
fn trust(connections: State<'_, Connections>, host: &str, fingerprint: String) -> Result<(), CommandError> {
    Ok(connections.edit(|hosts| hosts.trust(host, fingerprint))?)
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
async fn set_power(connections: State<'_, Connections>, host: &str, action: PowerAction) -> Result<(), CommandError> {
    let connection = connections.get(host)?;
    if matches!(action, PowerAction::OnToImage | PowerAction::ResetToImage) && !connection.image.serving().await {
        return Err(CommandError::failed("No disk image is mounted, mount one again."));
    }
    log::info!("{action:?} on {host}");
    Ok(connection.client.lock().await.set_power(action).await?)
}

/// What the UI reads from a device: a command per `Client` read, named after it.
macro_rules! reads {
    ($($name:ident: $type:ty),*) => {$(
        #[tauri::command]
        async fn $name(connections: State<'_, Connections>, host: &str) -> Result<$type, CommandError> {
            Ok(connections.get(host)?.client.lock().await.$name().await?)
        }
    )*};
}

reads!(power: Power, status: Status, hardware: Hardware, settings: Settings, event_log: Vec<Event>, audit_log: Vec<AuditRecord>);

/// Changes a setting and says how it went, see `Connections::change`. The webview closes the screen and serial console
/// once they're off.
#[tauri::command]
async fn change(connections: State<'_, Connections>, host: &str, change: Change) -> Result<Changed, CommandError> {
    connections.change(host, change).await
}

/// Writes the certificate as PEM to `path`.
#[tauri::command]
async fn save_certificate(
    connections: State<'_, Connections>,
    host: &str,
    id: &str,
    path: PathBuf,
) -> Result<(), CommandError> {
    let pem = connections.get(host)?.client.lock().await.certificate(id).await?;
    Ok(std::fs::write(path, pem)?)
}

/// Opens AMT's own web pages in the browser.
#[tauri::command]
async fn web_pages(app: AppHandle, connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    let url = connections.get(host)?.client.lock().await.web_pages();
    app.opener().open_url(url, None::<&str>).map_err(CommandError::failed)
}

/// Has the device show a code for someone at it to read out, see `Client::start_consent`.
#[tauri::command]
async fn consent_start(connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.start_consent().await?)
}

#[tauri::command]
async fn consent_send(connections: State<'_, Connections>, host: &str, code: u32) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.send_consent(code).await?)
}

#[tauri::command]
async fn consent_cancel(connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.cancel_consent().await?)
}

/// Opens the device's screen and says its id, which the other `kvm_` commands take. See `Connections::open`.
#[tauri::command]
async fn kvm_open(connections: State<'_, Connections>, host: &str) -> Result<u64, CommandError> {
    connections.open(host, |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new)).await
}

/// See `Client::screens`.
#[tauri::command]
async fn screens(connections: State<'_, Connections>, host: &str) -> Result<Screens, CommandError> {
    Ok(connections.get(host)?.client.lock().await.screens().await?)
}

#[tauri::command]
async fn show_screen(connections: State<'_, Connections>, host: &str, screen: u8) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.show_screen(screen).await?)
}

/// The next change on screen, as bytes rather than JSON (see `Kvm::frame`).
#[tauri::command]
async fn kvm_frame(connections: State<'_, Connections>, host: &str, id: u64) -> Result<Response, CommandError> {
    Ok(Response::new(connections.get(host)?.kvm.get(id).await?.frame().await?))
}

#[tauri::command]
async fn kvm_input(
    connections: State<'_, Connections>,
    host: &str,
    id: u64,
    inputs: Vec<Input>,
) -> Result<(), CommandError> {
    Ok(connections.get(host)?.kvm.get(id).await?.input(inputs).await?)
}

/// Closes the screen `id` names, unless another one opened since or the device was disconnected, which closed it.
#[tauri::command]
async fn kvm_close(connections: State<'_, Connections>, host: &str, id: u64) -> Result<(), CommandError> {
    if let Ok(connection) = connections.get(host) {
        connection.kvm.close(Some(id)).await;
    }
    Ok(())
}

/// Opens the device's serial console and says its id, which the other `sol_` commands take, like `kvm_open`.
#[tauri::command]
async fn sol_open(connections: State<'_, Connections>, host: &str) -> Result<u64, CommandError> {
    connections.open(host, |connection| &connection.sol, async |client| client.sol().await.map(Arc::new)).await
}

/// What the device wrote to its serial port since, see `Sol::read`, as bytes rather than JSON.
#[tauri::command]
async fn sol_read(connections: State<'_, Connections>, host: &str, id: u64) -> Result<Response, CommandError> {
    Ok(Response::new(connections.get(host)?.sol.get(id).await?.read().await?))
}

#[tauri::command]
async fn sol_write(connections: State<'_, Connections>, host: &str, id: u64, data: String) -> Result<(), CommandError> {
    Ok(connections.get(host)?.sol.get(id).await?.write(data.as_bytes()).await?)
}

/// Like `kvm_close`.
#[tauri::command]
async fn sol_close(connections: State<'_, Connections>, host: &str, id: u64) -> Result<(), CommandError> {
    if let Ok(connection) = connections.get(host) {
        connection.sol.close(Some(id)).await;
    }
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

/// A disk image in a device's drive: the id `ider_eject` takes, its file's name and its size.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
struct Mounted {
    id: u64,
    name: String,
    size: u64,
}

/// What the device does with the disk image in its drive: reads part of it, in bytes, or ends the session, a reset say,
/// saying why if it broke off. Taking it out says neither, the webview knows.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
#[serde(tag = "kind")]
enum Drive {
    Read(Range<u64>),
    Ended { error: Option<CommandError> },
}

/// Puts the image at `path` in the device's drive (see `Client::ider`), taking out the one in it, then says on `drive`
/// what the device does with it.
#[tauri::command]
async fn ider_mount(
    connections: State<'_, Connections>,
    host: &str,
    path: PathBuf,
    drive: Channel<Drive>,
) -> Result<Mounted, CommandError> {
    let file = std::fs::File::open(&path)?;
    let size = file.metadata()?.len();
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let image = format!("{name} in {host}'s drive");
    let id = connections
        .open(
            host,
            |connection| &connection.image,
            async |client| Ok(Task(tauri::async_runtime::spawn(serve(client.ider(file).await?, drive, image)))),
        )
        .await?;
    Ok(Mounted { id, name, size })
}

/// Serves the disk `image` (see `Ider::serve`), saying on `drive` what the device reads of it and how serving it ended.
async fn serve(ider: Ider, drive: Channel<Drive>, image: String) {
    let ended = ider.serve(|read| drive.send(Drive::Read(read)).unwrap_or_default()).await;
    match &ended {
        Ok(()) => log::info!("The device closed {image}"),
        Err(error) => log::warn!("Serving {image} broke off: {error}"),
    }
    drive.send(Drive::Ended { error: ended.err().map(Into::into) }).unwrap_or_default();
}

/// Takes the disk image `id` names out of the drive, like `kvm_close`.
#[tauri::command]
async fn ider_eject(connections: State<'_, Connections>, host: &str, id: u64) -> Result<(), CommandError> {
    if let Ok(connection) = connections.get(host) {
        connection.image.close(Some(id)).await;
    }
    Ok(())
}

/// The window is titled after the device on screen.
#[tauri::command]
fn title(window: WebviewWindow, title: &str) -> Result<(), CommandError> {
    window.set_title(title).map_err(CommandError::failed)
}

/// The window fills the screen, for the KVM's own full screen.
#[tauri::command]
fn fullscreen(window: WebviewWindow, on: bool) -> Result<(), CommandError> {
    window.set_fullscreen(on).map_err(CommandError::failed)
}

/// Writes the log (see `main`) to `path`, the older file first.
#[tauri::command]
fn save_log(app: AppHandle, path: PathBuf) -> Result<(), CommandError> {
    let logs = app.path().app_log_dir().map_err(CommandError::failed)?;
    let mut files = std::fs::read_dir(logs)?.collect::<io::Result<Vec<_>>>()?;
    files.retain(|file| file.path().extension().is_some_and(|extension| extension == "log"));
    files.sort_by_key(|file| file.metadata().and_then(|metadata| metadata.modified()).ok());
    let log = files.iter().map(|file| std::fs::read(file.path())).collect::<io::Result<Vec<_>>>()?;
    Ok(std::fs::write(path, log.concat())?)
}

/// What the webview gets when a command fails: always a message, and both fingerprints when the certificate changed.
/// `Unreachable` means the device stopped answering, even to a second try, `Consent` that someone at it has to read out
/// a code first (see `consent_start`).
#[derive(Debug, Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
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

impl Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::PinMismatch { message, .. }
        | Self::Unreachable { message }
        | Self::Consent { message }
        | Self::Failed { message }) = self;
        formatter.write_str(message)
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

impl From<io::Error> for CommandError {
    fn from(error: io::Error) -> Self {
        Self::failed(error)
    }
}

impl From<keyring::Error> for CommandError {
    fn from(error: keyring::Error) -> Self {
        Self::failed(format_args!("Keychain: {error}"))
    }
}

/// Writes our types the webview sees to ui/src/bindings/ooblin.ts (see .cargo/config.toml), without the amt-core types
/// they hold: amt-core's tests write those, and ts-rs writes a file anew the first time a test run writes to it.
#[test]
fn bindings() -> Result<(), ts_rs::ExportError> {
    let config = ts_rs::Config::from_env();
    Change::export(&config)?;
    Changed::export(&config)?;
    CommandError::export(&config)?;
    Connected::export(&config)?;
    Device::export(&config)?;
    Drive::export(&config)?;
    Mounted::export(&config)
}

fn main() {
    let args = Args::parse();
    // First, so a second start hands its command line to us and quits before anything else of it starts: we open its
    // host, if it names one, as on start.
    let builder = tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, args, _| {
        if let Ok(args) = Args::try_parse_from(args)
            && args.host.is_some()
        {
            *app.state::<Launch>().0.lock().unwrap() = args;
            app.emit("launch", ()).unwrap_or_default();
        }
        if let Some(window) = app.get_webview_window("main") {
            window.unminimize().unwrap_or_default();
            window.set_focus().unwrap_or_default();
        }
    }));
    // Debug builds let agents drive the app through a localhost WebSocket (mcp-server-tauri), release builds don't.
    #[cfg(debug_assertions)]
    let builder = builder.plugin(tauri_plugin_mcp_bridge::Builder::new().bind_address("127.0.0.1").build());

    builder
        // What happened, from Rust and the webview, in the OS's log folder: a megabyte at most and the one before, which
        // `save_log` saves.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(1_000_000)
                .rotation_strategy(RotationStrategy::KeepSome(1))
                .build(),
        )
        .setup(|app| {
            let hosts = app.path().app_config_dir()?.join("hosts.json");
            app.manage(Connections::new(hosts, app.config().identifier.clone()));
            #[cfg(debug_assertions)]
            app.add_capability(
                tauri::ipc::CapabilityBuilder::new("mcp-bridge").window("main").permission("mcp-bridge:default"),
            )?;
            log::info!("Ooblin {} on {}", app.package_info().version, std::env::consts::OS);
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        // The window opens where and as large as it was, but neither hidden (the verify build's) nor full screen.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
                .build(),
        )
        .manage(Launch(args.into()))
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
            fullscreen,
            save_log
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime starts");
}
