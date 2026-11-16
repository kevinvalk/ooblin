#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod connections;
mod hosts;

use std::collections::HashSet;
use std::fmt::Display;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use connections::{Changed, Connected, Connections, Serving, Task};
use hosts::Device;
use ooblin_core::amt::{AuditRecord, Change, Screens, Settings, Status};
use ooblin_core::ipmi::{Change as IpmiChange, Fru, IpmiSettings, IpmiStatus};
use ooblin_core::supermicro::{BmcChange, BmcSettings, BmcStatus};
use ooblin_core::{
    Client, Error, Event, Flavor, Found, Hardware, Input, Media, Panel, Power, PowerAction, RMCP_PORT, Sensor, Target,
};
use serde::Serialize;
use tauri::async_runtime::Receiver;
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WebviewWindow};
use tauri_plugin_log::RotationStrategy;
use tauri_plugin_window_state::StateFlags;
use ts_rs::TS;

/// A desktop console for Intel AMT, Supermicro BMCs and any BMC over IPMI.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Connect to this host on start, with its saved credentials unless --password is given. A BMC whose web port
    /// isn't 443 takes it here, `host:port`.
    host: Option<String>,
    #[arg(long, default_value = "admin")]
    user: String,
    #[arg(long)]
    password: Option<String>,
}

/// What we were started with last, which a second start with a host replaces (see `main`).
struct Launch(std::sync::Mutex<Args>);

impl Launch {
    /// The login the command line gives `host`, if it gives a password. We only hand it out ONCE, because tried on
    /// every click a wrong one would lock a BMC's account, and it would stand in for the one typed and remembered
    /// since.
    fn login(&self, host: &str) -> Option<Target> {
        let args = &mut *self.0.lock().unwrap();
        let password = args.password.take_if(|_| args.host.as_deref() == Some(host))?;
        Some(Target::new(host, &args.user, password))
    }
}

/// The device the command line names, if any. The webview opens it like a click in the sidebar would, on start and when
/// we are started again.
#[tauri::command]
fn launch(connections: State<'_, Connections>, launch: State<'_, Launch>) -> Result<Option<Device>, CommandError> {
    let args = launch.0.lock().unwrap();
    let Some(host) = args.host.clone() else { return Ok(None) };

    let saved = connections.hosts()?.device(&host).filter(|_| args.password.is_none());
    let (user, is_remembered) = (args.user.clone(), args.password.is_some());
    let unsaved = Device { host, name: String::new(), version: String::new(), user, is_remembered, flavor: None };
    Ok(Some(saved.unwrap_or(unsaved)))
}

#[tauri::command]
fn devices(connections: State<'_, Connections>) -> Result<Vec<Device>, CommandError> {
    Ok(connections.hosts()?.devices())
}

/// Without a `password` we log in with the one we have, from the command line (once) or the OS credential store.
/// `is_remember` keeps a typed one there, and so does a device we remember already, as the password that last worked
/// replaces the one before.
#[tauri::command]
async fn connect(
    connections: State<'_, Connections>,
    launch: State<'_, Launch>,
    host: String,
    user: String,
    password: Option<String>,
    is_remember: bool,
    flavor: Option<Flavor>,
) -> Result<Connected, CommandError> {
    let hosts = connections.hosts()?;
    let is_remembered = hosts.device(&host).is_some_and(|device| device.is_remembered);
    let is_remember = password.is_some() && (is_remember || is_remembered);

    let target = match (password, launch.login(&host)) {
        (Some(password), _) => Target::new(host, user, password),
        (None, Some(target)) => target,
        (None, None) => hosts.target(&host)?,
    };

    connections.connect(target, flavor, is_remember).await
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

const IS_VERIFY_BUILD: bool = cfg!(feature = "verify"); // It keeps to the mock and off the user's screen (AGENTS.md).

/// The network to look for devices on unless told otherwise, see `ooblin_core::nearby`. The verify build looks on this
/// machine only, where the mock is, and never at the user's devices.
#[tauri::command]
fn networks() -> String {
    if IS_VERIFY_BUILD { "127.0.0.1/32".into() } else { ooblin_core::nearby() }
}

#[derive(Default)]
struct Discovery(std::sync::Mutex<Option<Task>>);

/// Looks for devices on `networks` and sends each on `found` as it answers, see `ooblin_core::discover`.
#[tauri::command]
async fn discover(
    discovery: State<'_, Discovery>,
    networks: String,
    found: Channel<Found>,
) -> Result<(), CommandError> {
    let (done, mut ended) = tauri::async_runtime::channel(1);
    let search = async move {
        let outcome =
            ooblin_core::discover(&networks, RMCP_PORT, |device| found.send(device).unwrap_or_default()).await;
        done.send(outcome).await.unwrap_or_default();
    };

    // Replacing the task drops the one before, which ends that search, as `stop_discovery` does.
    *discovery.0.lock().unwrap() = Some(Task(tauri::async_runtime::spawn(search)));

    // A stopped search never says how it went, so we call that fine.
    Ok(ended.recv().await.unwrap_or(Ok(()))?)
}

#[tauri::command]
fn stop_discovery(discovery: State<'_, Discovery>) {
    discovery.0.lock().unwrap().take();
}

#[tauri::command]
async fn set_power(connections: State<'_, Connections>, host: &str, action: PowerAction) -> Result<(), CommandError> {
    let connection = connections.get(host)?;
    if matches!(action, PowerAction::OnToImage | PowerAction::ResetToImage) && !connection.image.is_serving().await {
        return Err(CommandError::failed("No disk image is mounted, mount one again."));
    }

    log::info!("{action:?} on {host}");
    Ok(connection.client.lock().await.set_power(action).await?)
}

// What the UI reads from a device is a command per `Client` read, named after it. A read that only one flavor makes
// names it as `= flavor.method`, the method of that flavor's client.
macro_rules! reads {
    ($($name:ident $(= $flavor:ident.$method:ident)?: $type:ty);* $(;)?) => {$(
        #[tauri::command]
        async fn $name(connections: State<'_, Connections>, host: &str) -> Result<$type, CommandError> {
            let connection = connections.get(host)?;
            let mut client = connection.client.lock().await;
            Ok(reads!(@call client $name $($flavor $method)?).await?)
        }
    )*};
    (@call $client:ident $name:ident) => { $client.$name() };
    (@call $client:ident $name:ident $flavor:ident $method:ident) => { $client.$flavor()?.$method() };
}

reads!(power: Power; hardware: Hardware; event_log: Vec<Event>; sensors: Vec<Sensor>; panel: Panel;
       status = amt.status: Status; settings = amt.settings: Settings; audit_log = amt.audit_log: Vec<AuditRecord>;
       bmc_status: BmcStatus; bmc_settings = supermicro.bmc_settings: BmcSettings;
       ipmi_status = ipmi.status: IpmiStatus; fru = ipmi.fru: Fru; ipmi_settings = ipmi.settings: IpmiSettings);

#[tauri::command]
async fn identify(connections: State<'_, Connections>, host: &str, is_on: bool) -> Result<(), CommandError> {
    log::info!("Identify {} on {host}", if is_on { "on" } else { "off" });
    Ok(connections.get(host)?.client.lock().await.identify(is_on).await?)
}

#[tauri::command]
async fn change(
    connections: State<'_, Connections>,
    picked: State<'_, Picked>,
    host: &str,
    change: Change,
) -> Result<Changed<Settings>, CommandError> {
    if let Change::AddCertificate { path, .. } = &change {
        picked.check(path, false)?;
    }

    let make = async |client: &mut Client, change| client.amt()?.change(change).await;
    let changed = connections.change(host, change, make, async |client| client.amt()?.settings().await).await?;

    if let Ok(settings) = &changed.settings {
        let name = &settings.general.name;
        if let Err(error) = connections.edit(|hosts| hosts.renamed(host, name)) {
            log::warn!("Saving {host}'s name {name} failed: {error}");
        }

        // A drive that is turned off cannot hold a disk image, so we take it out. The screen and serial console are
        // closed by the webview once they are off.
        if !settings.remote_access.is_ider
            && let Ok(connection) = connections.get(host)
        {
            connection.image.close(None).await;
        }
    }

    Ok(changed)
}

#[tauri::command]
async fn ipmi_change(
    connections: State<'_, Connections>,
    host: &str,
    change: IpmiChange,
) -> Result<Changed<IpmiSettings>, CommandError> {
    let make = async |client: &mut Client, change| client.ipmi()?.change(change).await;
    connections.change(host, change, make, async |client| client.ipmi()?.settings().await).await
}

#[tauri::command]
async fn bmc_change(
    connections: State<'_, Connections>,
    host: &str,
    change: BmcChange,
) -> Result<Changed<BmcSettings>, CommandError> {
    let make = async |client: &mut Client, change| client.supermicro()?.change(change).await;
    connections.change(host, change, make, async |client| client.supermicro()?.bmc_settings().await).await
}

#[tauri::command]
async fn save_certificate(
    connections: State<'_, Connections>,
    picked: State<'_, Picked>,
    host: &str,
    id: &str,
    path: PathBuf,
) -> Result<(), CommandError> {
    picked.check(&path, true)?;

    let pem = connections.get(host)?.client.lock().await.amt()?.certificate(id).await?;
    Ok(std::fs::write(path, pem)?)
}

#[tauri::command]
async fn web_pages(connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    let url = connections.get(host)?.client.lock().await.web_pages();
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(CommandError::failed)
}

/// The device shows a code for someone at it to read out, see `amt::Client::start_consent`.
#[tauri::command]
async fn consent_start(connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.amt()?.start_consent().await?)
}

#[tauri::command]
async fn consent_send(connections: State<'_, Connections>, host: &str, code: u32) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.amt()?.send_consent(code).await?)
}

#[tauri::command]
async fn consent_cancel(connections: State<'_, Connections>, host: &str) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.amt()?.cancel_consent().await?)
}

/// Returns the id of the screen, which the other `kvm_` commands take.
#[tauri::command]
async fn kvm_open(connections: State<'_, Connections>, host: &str) -> Result<u64, CommandError> {
    connections.open(host, |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new)).await
}

#[tauri::command]
async fn screens(connections: State<'_, Connections>, host: &str) -> Result<Screens, CommandError> {
    Ok(connections.get(host)?.client.lock().await.amt()?.screens().await?)
}

#[tauri::command]
async fn show_screen(connections: State<'_, Connections>, host: &str, screen: u8) -> Result<(), CommandError> {
    Ok(connections.get(host)?.client.lock().await.amt()?.show_screen(screen).await?)
}

/// The next change on screen, which we answer as bytes instead of JSON (see `Kvm::frame`).
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

/// Another screen opening since, or disconnecting the device, closed this one already. So that is fine too.
#[tauri::command]
async fn kvm_close(connections: State<'_, Connections>, host: &str, id: u64) -> Result<(), CommandError> {
    if let Ok(connection) = connections.get(host) {
        connection.kvm.close(Some(id)).await;
    }
    Ok(())
}

/// Returns the id of the serial console, which the other `sol_` commands take.
#[tauri::command]
async fn sol_open(connections: State<'_, Connections>, host: &str) -> Result<u64, CommandError> {
    connections.open(host, |connection| &connection.sol, async |client| client.sol().await.map(Arc::new)).await
}

/// What the device wrote to its serial port since, which we answer as bytes instead of JSON (see `Sol::read`).
#[tauri::command]
async fn sol_read(connections: State<'_, Connections>, host: &str, id: u64) -> Result<Response, CommandError> {
    Ok(Response::new(connections.get(host)?.sol.get(id).await?.read().await?))
}

#[tauri::command]
async fn sol_write(connections: State<'_, Connections>, host: &str, id: u64, text: String) -> Result<(), CommandError> {
    Ok(connections.get(host)?.sol.get(id).await?.write(text.as_bytes()).await?)
}

/// Another serial console opening since, or disconnecting the device, closed this one already. So that is fine too.
#[tauri::command]
async fn sol_close(connections: State<'_, Connections>, host: &str, id: u64) -> Result<(), CommandError> {
    if let Ok(connection) = connections.get(host) {
        connection.sol.close(Some(id)).await;
    }
    Ok(())
}

/// The files the user picked in a dialog, to save to or to open. A command only takes these from the webview, because
/// the webview could name any other file if a device ever got a script into it.
#[derive(Default)]
struct Picked(std::sync::Mutex<HashSet<(PathBuf, bool)>>);

impl Picked {
    fn keep(&self, path: Option<PathBuf>, is_save: bool) -> Option<PathBuf> {
        self.0.lock().unwrap().extend(path.clone().map(|path| (path, is_save)));
        path
    }

    fn check(&self, path: &Path, is_save: bool) -> Result<(), CommandError> {
        let is_picked = self.0.lock().unwrap().contains(&(path.to_owned(), is_save));
        is_picked.then_some(()).ok_or_else(|| CommandError::failed("That file wasn't picked here."))
    }
}

/// Asks for a file to open, of a `kind` ("Disk image") and its `extensions`. The verify build never shows a dialog, it
/// picks the file `OOBLIN_PICK` names.
///
/// NOTE: Native dialogs want the main thread, which is where a command that is not async runs.
#[tauri::command]
fn pick(window: WebviewWindow, picked: State<'_, Picked>, kind: &str, extensions: Vec<String>) -> Option<PathBuf> {
    let dialog = || rfd::FileDialog::new().set_parent(&window).add_filter(kind, &extensions).pick_file();
    let path = if IS_VERIFY_BUILD { std::env::var_os("OOBLIN_PICK").map(PathBuf::from) } else { dialog() };
    picked.keep(path, false)
}

/// Asks where to save a file, suggesting `name`, see `pick`. The verify build saves to `OOBLIN_SAVE`, so never over
/// the file it picks.
#[tauri::command]
fn pick_save(window: WebviewWindow, picked: State<'_, Picked>, name: &str) -> Option<PathBuf> {
    let dialog = || rfd::FileDialog::new().set_parent(&window).set_file_name(name).save_file();
    let path = if IS_VERIFY_BUILD { std::env::var_os("OOBLIN_SAVE").map(PathBuf::from) } else { dialog() };
    picked.keep(path, true)
}

/// A disk image in a device's drive: the id `eject` takes, its file's name and its size.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
struct Mounted {
    id: u64,
    name: String,
    size_bytes: u64,
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

/// Puts the image at `path` in the device's drive (see `Client::mount`), taking out the one in it, then sends on
/// `drive` what the device does with it.
#[tauri::command]
async fn mount(
    connections: State<'_, Connections>,
    picked: State<'_, Picked>,
    host: &str,
    path: PathBuf,
    drive: Channel<Drive>,
) -> Result<Mounted, CommandError> {
    picked.check(&path, false)?;

    let file = std::fs::File::open(&path)?;
    let size_bytes = file.metadata()?.len();
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let image = format!("{name} in {host}'s drive");

    let open = async |client: &mut Client| {
        let media = client.mount(file).await?;
        let (eject, ejected) = tauri::async_runtime::channel(1);
        let task = Task(tauri::async_runtime::spawn(serve(media, drive, image, ejected)));
        Ok(Serving { task, eject })
    };
    let id = connections.open(host, |connection| &connection.image, open).await?;
    Ok(Mounted { id, name, size_bytes })
}

/// Serves the disk `image` until `ejected` (see `Media::serve`), sending on `drive` what the device reads of it and how
/// serving it ended.
async fn serve(media: Media, drive: Channel<Drive>, image: String, mut ejected: Receiver<()>) {
    let read = |read| drive.send(Drive::Read(read)).unwrap_or_default();
    let ended = media.serve(read, async move { ejected.recv().await.unwrap_or_default() }).await;

    match &ended {
        Ok(()) => log::info!("{image} is out"),
        Err(error) => log::warn!("Serving {image} broke off: {error}"),
    }

    drive.send(Drive::Ended { error: ended.err().map(Into::into) }).unwrap_or_default();
}

/// Another disk image mounted since, or disconnecting the device, took this one out already. So that is fine too.
#[tauri::command]
async fn eject(connections: State<'_, Connections>, host: &str, id: u64) -> Result<(), CommandError> {
    if let Ok(connection) = connections.get(host) {
        connection.image.close(Some(id)).await;
    }
    Ok(())
}

#[tauri::command]
fn title(window: WebviewWindow, title: &str) -> Result<(), CommandError> {
    window.set_title(title).map_err(CommandError::failed)
}

#[tauri::command]
fn fullscreen(window: WebviewWindow, is_on: bool) -> Result<(), CommandError> {
    window.set_fullscreen(is_on).map_err(CommandError::failed)
}

/// Grows or shrinks the window by `width` and `height` in points, for the KVM to show the device's screen at its own
/// size.
#[tauri::command]
fn grow(window: WebviewWindow, width: f64, height: f64) -> Result<(), CommandError> {
    window.unmaximize().map_err(CommandError::failed)?; // A size does not apply to a maximized window.

    let scale_factor = window.scale_factor().map_err(CommandError::failed)?;
    let size = window.inner_size().map_err(CommandError::failed)?.to_logical::<f64>(scale_factor);
    let size = tauri::LogicalSize::new(size.width + width, size.height + height);
    window.set_size(size).map_err(CommandError::failed)
}

#[tauri::command]
fn save_log(app: AppHandle, picked: State<'_, Picked>, path: PathBuf) -> Result<(), CommandError> {
    picked.check(&path, true)?;

    // The log is the current file and the one before it (see `main`), which we write as one with the older file first.
    let log_directory = app.path().app_log_dir().map_err(CommandError::failed)?;
    let mut files = std::fs::read_dir(log_directory)?.collect::<io::Result<Vec<_>>>()?;
    files.retain(|file| file.path().extension().is_some_and(|extension| extension == "log"));
    files.sort_by_key(|file| file.metadata().and_then(|metadata| metadata.modified()).ok());
    let logs = files.iter().map(|file| std::fs::read(file.path())).collect::<io::Result<Vec<_>>>()?;

    Ok(std::fs::write(path, logs.concat())?)
}

/// What the webview gets when a command fails: always a message, and both fingerprints when the certificate changed.
/// `Unreachable` means the device stopped answering, even to a second try, `Consent` that someone at it has to read out
/// a code first (see `consent_start`), `Ended` that it ended a session on purpose, which we do not open again
/// ourselves.
#[derive(Debug, Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
#[serde(tag = "kind")]
enum CommandError {
    PinMismatch { message: String, expected: String, actual: String },
    Unreachable { message: String },
    Consent { message: String },
    Ended { message: String },
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
        | Self::Ended { message }
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
            Error::Ended(_) => Self::Ended { message },
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

/// Writes our types the webview sees to ui/src/bindings/ooblin.ts (see .cargo/config.toml). We leave out the
/// ooblin-core types they hold, because the tests of ooblin-core write those and ts-rs writes a file anew on a test
/// run's first write to it.
#[test]
fn bindings() -> Result<(), ts_rs::ExportError> {
    let config = ts_rs::Config::from_env();
    Changed::<()>::export(&config)?;
    CommandError::export(&config)?;
    Connected::export(&config)?;
    Device::export(&config)?;
    Drive::export(&config)?;
    Mounted::export(&config)
}

#[test]
fn the_command_lines_password_is_used_once() {
    let launch = Launch(Args::parse_from(["ooblin", "bmc", "--user", "root", "--password", "secret"]).into());
    assert!(launch.login("another").is_none());

    let login = launch.login("bmc").unwrap();
    assert_eq!((login.user.as_str(), login.password.as_str()), ("root", "secret"));
    assert!(launch.login("bmc").is_none());
}

#[test]
fn only_a_file_picked_for_it_is_taken() {
    let picked = Picked::default();
    picked.keep(Some("disc.iso".into()), false);
    picked.keep(None, true);

    assert!(picked.check(Path::new("disc.iso"), false).is_ok());
    assert!(picked.check(Path::new("disc.iso"), true).is_err());
    assert!(picked.check(Path::new("hosts.json"), false).is_err());
}

fn main() {
    let args = Args::try_parse().unwrap_or_else(|error| {
        // A Windows release has no console (see `windows_subsystem`), so we say a typo, --help or --version in a
        // dialog.
        #[cfg(all(windows, not(debug_assertions)))]
        rfd::MessageDialog::new().set_title("Ooblin").set_description(error.to_string()).show();
        error.exit()
    });

    // IMPORTANT: This plugin has to be the first, so a second start hands its command line to us and quits before
    // anything else of it starts. We open its host, if it names one, as on start.
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

    // Only the verify build lets agents drive the app through a localhost WebSocket (mcp-server-tauri).
    #[cfg(feature = "verify")]
    let builder = builder.plugin(tauri_plugin_mcp_bridge::Builder::new().bind_address("127.0.0.1").build());

    builder
        // We log what happened in Rust and the webview to the OS's log folder. That is a megabyte at most plus the
        // file before it, which `save_log` saves.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(1_000_000)
                .rotation_strategy(RotationStrategy::KeepSome(1))
                .build(),
        )
        .setup(|app| {
            let hosts_path = app.path().app_config_dir()?.join("hosts.json");
            app.manage(Connections::new(hosts_path, app.config().identifier.clone()));

            #[cfg(feature = "verify")]
            app.add_capability(
                tauri::ipc::CapabilityBuilder::new("mcp-bridge")
                    .window("main")
                    .permission("mcp-bridge:default")
                    .permission("core:event:default"), // The bridge's script emits events, which the window may not.
            )?;

            log::info!("Ooblin {} on {}", app.package_info().version, std::env::consts::OS);
            Ok(())
        })
        // The window opens where and as large as it was. We do not restore hidden (the verify build's window is) nor
        // full screen.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
                .build(),
        )
        .manage(Launch(args.into()))
        .manage(Discovery::default())
        .manage(Picked::default())
        // NOTE: The webview never holds a socket or a stored password, it calls these commands instead.
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
            bmc_status,
            panel,
            identify,
            sensors,
            bmc_settings,
            bmc_change,
            ipmi_status,
            fru,
            ipmi_settings,
            ipmi_change,
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
            mount,
            eject,
            title,
            fullscreen,
            grow,
            save_log
        ])
        .build(tauri::generate_context!())
        .expect("the Tauri runtime starts")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                tauri::async_runtime::block_on(app.state::<Connections>().close_all());
            }
        });
}
