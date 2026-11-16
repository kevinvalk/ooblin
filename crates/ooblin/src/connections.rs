#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use ooblin_core::{Client, Flavor, Info, Kvm, Sol, Target};
use serde::Serialize;
use tauri::async_runtime::{JoinHandle, Mutex, Sender};
use ts_rs::TS;

use crate::CommandError;
use crate::hosts::Hosts;

// NOTE: The saved devices change under the lock of `state` too, one change at a time from the file as it is. That way
// two logins at once both stick, and a device forgotten during a login stays forgotten.
//
// IMPORTANT: We take locks in the order screen, serial console or drive, then client, then `state`. Nothing may await
// under `state`, nor wait on the credential store!
pub struct Connections {
    state: std::sync::Mutex<State>,
    hosts_path: PathBuf,
    service: String,
}

// NOTE: Each login, each connection and whatever opens on one gets an id. Work that outlives them (a login overtaken or
// cancelled, a screen opening as the device is connected again, closing a screen since opened anew) then changes
// nothing that is not its own.
#[derive(Default)]
struct State {
    connections: HashMap<String, Arc<Connection>>,
    // The latest login per host that is not in yet, so underway or failed. A disconnect, forget or newer login cancels
    // it.
    logins: HashMap<String, u64>,
    last_id: u64,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// The connection is handed to the caller to close, as we cannot await under the lock. A login to `host` that is
    /// underway is cancelled too.
    fn remove(&mut self, host: &str) -> Option<Arc<Connection>> {
        self.logins.remove(host);
        self.connections.remove(host)
    }
}

// NOTE: AMT wants its calls one at a time, so each connection's client is on its own lock (other devices do not care).
// It also takes only one screen, serial console and disk image at a time, so those open and close in turn.
pub struct Connection {
    pub id: u64,
    pub client: Mutex<Client>,
    pub kvm: Slot<Arc<Kvm>>,
    pub sol: Slot<Arc<Sol>>,
    pub image: Slot<Serving>,
}

/// A redirection session while it is open, with the id its commands name.
pub struct Slot<T>(Mutex<Option<(u64, T)>>);

pub trait Redirection {
    const NAME: &str; // What we call it in the log.
    async fn close(self);
}

impl Redirection for Arc<Kvm> {
    const NAME: &str = "screen";

    async fn close(self) {
        Kvm::close(&self).await
    }
}

impl Redirection for Arc<Sol> {
    const NAME: &str = "serial console";

    async fn close(self) {
        Sol::close(&self).await
    }
}

impl Redirection for Serving {
    const NAME: &str = "disk image";

    async fn close(mut self) {
        // Dropping `eject` takes the disc out. We wait until it is out, because only then the device's drive is free.
        drop(self.eject);
        let _ = (&mut self.task.0).await;
    }
}

pub struct Serving {
    pub task: Task,
    pub eject: Sender<()>, // Dropping it takes the disc out, see `ooblin_core::Media::serve`.
}

/// A task that stops when dropped.
pub struct Task(pub JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A login that worked, and why its password was not remembered, if it was to be.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
pub struct Connected {
    pub info: Info,
    pub remember_error: Option<String>,
}

/// How a change went: why it failed, if it did, and the settings read after it either way, as a change that failed may
/// have made some of it, or why they could not be read. AMT's settings or a BMC's.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
pub struct Changed<T> {
    pub error: Option<CommandError>,
    pub settings: Result<T, CommandError>,
}

impl Connections {
    pub fn new(hosts_path: PathBuf, service: String) -> Self {
        Self { state: Default::default(), hosts_path, service }
    }

    /// The saved devices as the file has them now. Only read from these, changes go through `edit`.
    pub fn hosts(&self) -> io::Result<Hosts> {
        Hosts::open(&self.hosts_path, &self.service)
    }

    pub fn edit<T>(&self, change: impl FnOnce(&mut Hosts) -> io::Result<T>) -> io::Result<T> {
        let _state = self.state.lock().unwrap();
        change(&mut self.hosts()?)
    }

    /// Logs in to the device of `target` and saves it, with its password if `is_remember` and the credential store
    /// takes it. A login cancelled before it is in leaves neither a connection, nor the device saved, nor its password.
    pub async fn connect(
        &self,
        target: Target,
        flavor: Option<Flavor>,
        is_remember: bool,
    ) -> Result<Connected, CommandError> {
        let login = {
            let mut state = self.state.lock().unwrap();
            let login = state.next_id();
            state.logins.insert(target.host.clone(), login);
            login
        };

        let hosts = self.hosts()?;
        let flavor = flavor.or(hosts.flavor(&target.host));
        let (client, info) = Client::login(&target, hosts.pin(&target.host), flavor).await?;

        let remember_error = if is_remember { hosts.remember(&target).err() } else { None };
        let is_remembered = is_remember && remember_error.is_none();
        let accepted = self.accept(login, &target, &info, client, is_remembered);

        // We go by what the saved devices say now. A device remembered all along keeps the password, which just worked,
        // but one forgotten meanwhile does not.
        let is_kept =
            || self.hosts().ok().and_then(|hosts| hosts.device(&target.host)).is_some_and(|saved| saved.is_remembered);
        if accepted.is_err() && is_remembered && !is_kept() {
            hosts.forget_password(&target.host).unwrap_or_default();
        }

        // We close the connection we replaced before we say we are in, as that frees the device's serial console and
        // drive.
        if let Some(replaced) = accepted? {
            replaced.close(&target.host).await;
        }

        log::info!("Connected to {} ({:?} {}), connection {login}", target.host, info.flavor, info.version);
        Ok(Connected { info, remember_error: remember_error.map(|error| error.to_string()) })
    }

    fn accept(
        &self,
        login: u64,
        target: &Target,
        info: &Info,
        client: Client,
        is_remembered: bool,
    ) -> Result<Option<Arc<Connection>>, CommandError> {
        let mut state = self.state.lock().unwrap();
        if state.logins.get(&target.host) != Some(&login) {
            return Err(CommandError::failed(format!("Connecting to {} was cancelled.", target.host)));
        }

        state.logins.remove(&target.host);
        self.hosts()?.connected(target, info, is_remembered)?;

        let (client, kvm, sol, image) = (Mutex::new(client), Slot::default(), Slot::default(), Slot::default());
        let connection = Connection { id: login, client, kvm, sol, image };
        Ok(state.connections.insert(target.host.clone(), Arc::new(connection)))
    }

    /// Also cancels a login to `host` that is underway.
    pub async fn close(&self, host: &str) {
        let connection = self.state.lock().unwrap().remove(host);
        if let Some(connection) = connection {
            connection.close(host).await;
        }
    }

    /// We close every connection at once, so quitting waits for the slowest device instead of all of them in turn. The
    /// logins underway are cancelled too.
    pub async fn close_all(&self) {
        let State { connections, .. } = std::mem::take(&mut *self.state.lock().unwrap());
        let closing = join_all(connections.iter().map(|(host, connection)| connection.close(host)));

        // We do not wait for long, because a session opening on a device that stopped answering holds its slot for as
        // long as that takes. What is still open when we quit hangs up with us.
        let _ = tokio::time::timeout(Duration::from_secs(5), closing).await;
    }

    /// Also lets go of the connection of `host` and cancels a login to it that is underway, see `Hosts::forget`.
    pub async fn forget(&self, host: &str) -> Result<(), CommandError> {
        let (connection, is_remembered) = {
            let mut state = self.state.lock().unwrap();
            (state.remove(host), self.hosts().and_then(|mut hosts| hosts.forget(host)))
        };

        if let Some(connection) = connection {
            connection.close(host).await;
        }

        if is_remembered? {
            self.hosts()?.forget_password(host)?;
        }

        log::info!("Forgot {host}");
        Ok(())
    }

    pub fn get(&self, host: &str) -> Result<Arc<Connection>, CommandError> {
        let connection = self.state.lock().unwrap().connections.get(host).cloned();
        connection.ok_or_else(|| not_connected(host))
    }

    /// An id for what opened on `connection`, unless that is no longer the connection of `host`. Closing it closed what
    /// was open then, which this was not yet, so the caller has to close it.
    fn opened(&self, host: &str, connection: &Connection) -> Result<u64, CommandError> {
        let mut state = self.state.lock().unwrap();
        match state.connections.get(host) {
            Some(current) if current.id == connection.id => Ok(state.next_id()),
            _ => Err(not_connected(host)),
        }
    }

    /// Opens a redirection session with `open` into the slot that `slot` picks and returns its id. We close the one
    /// before first, because AMT takes one of each at a time.
    pub async fn open<T: Redirection>(
        &self,
        host: &str,
        slot: impl FnOnce(&Connection) -> &Slot<T>,
        open: impl AsyncFnOnce(&mut Client) -> ooblin_core::Result<T>,
    ) -> Result<u64, CommandError> {
        let connection = self.get(host)?;
        let mut slot = slot(&connection).0.lock().await;
        if let Some(before) = slot.take() {
            close(before).await;
        }

        let opened = open(&mut *connection.client.lock().await).await;
        let opened = opened.inspect_err(|error| log::warn!("Opening the {} on {host} failed: {error}", T::NAME))?;

        match self.opened(host, &connection) {
            Ok(id) => {
                log::info!("Opened {} {id} on {host}", T::NAME);
                Ok(slot.insert((id, opened)).0)
            }
            Err(error) => {
                opened.close().await;
                Err(error)
            }
        }
    }

    /// Makes `change` with `make`, then reads the settings again with `read` whether it went through or not, because a
    /// change that failed may have made some of it.
    pub async fn change<C: std::fmt::Debug, T>(
        &self,
        host: &str,
        change: C,
        make: impl AsyncFnOnce(&mut Client, C) -> ooblin_core::Result<()>,
        read: impl AsyncFnOnce(&mut Client) -> ooblin_core::Result<T>,
    ) -> Result<Changed<T>, CommandError> {
        let connection = self.get(host)?;
        log::info!("Changing {host}'s settings: {change:?}");
        let mut client = connection.client.lock().await;

        let error = make(&mut client, change).await.err().map(CommandError::from);
        if let Some(error) = &error {
            log::warn!("Changing {host}'s settings failed: {error}");
        }

        let settings = read(&mut client).await.map_err(CommandError::from);
        if let Err(error) = &settings {
            log::warn!("Reading {host}'s settings after a change failed: {error}");
        }

        Ok(Changed { error, settings })
    }
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<T: Clone> Slot<T> {
    pub async fn get(&self, id: u64) -> Result<T, CommandError> {
        let slot = self.0.lock().await;
        let redirection =
            slot.as_ref().filter(|(open_id, _)| *open_id == id).map(|(_, redirection)| redirection.clone());
        redirection.ok_or_else(|| CommandError::failed("It's closed."))
    }
}

impl<T: Redirection> Slot<T> {
    /// Without an `id` we close whichever one is open.
    pub async fn close(&self, id: Option<u64>) {
        let mut slot = self.0.lock().await;
        if let Some(open) = slot.take_if(|(open_id, _)| id.is_none_or(|id| id == *open_id)) {
            close(open).await;
        }
    }
}

async fn close<T: Redirection>((id, redirection): (u64, T)) {
    redirection.close().await;
    log::info!("Closed {} {id}", T::NAME);
}

impl Slot<Serving> {
    /// Whether the device can read a disk image, so one is in the drive and we still serve it.
    pub async fn is_serving(&self) -> bool {
        self.0.lock().await.as_ref().is_some_and(|(_, serving)| !serving.task.0.inner().is_finished())
    }
}

impl Connection {
    async fn close(&self, host: &str) {
        self.kvm.close(None).await;
        self.sol.close(None).await;
        self.image.close(None).await;
        log::info!("Disconnected from {host}, connection {}", self.id);
    }
}

fn not_connected(host: &str) -> CommandError {
    CommandError::failed(format!("Not connected to {host}."))
}
