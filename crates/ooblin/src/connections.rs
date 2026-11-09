//! The devices we're connected to, one connection each, and what's open on them. AMT wants its calls one at a time, so
//! each connection's client is on its own lock (other devices don't care), and takes one screen, serial console and
//! disk image at a time, so those open and close in turn. Each login, connection and what opens on one gets an id, so
//! work that outlives them (a login overtaken or cancelled, a screen opening as the device is connected again, closing
//! a screen since opened anew) changes nothing that isn't its own. The saved devices change under the same lock, one
//! change at a time from the file as it is: two logins at once both stick, and a device forgotten during a login stays
//! forgotten. Nothing awaits under it, nor waits on the credential store. Locks are taken in the order screen, serial
//! console or drive, then client, then this one.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use amt_core::{
    Alarm, Availability, Client, General, Info, Ipv4, Ipv6Settings, Kvm, Recovery, RemoteAccess, Settings, Sol, Target,
};
use serde::{Deserialize, Serialize};
use tauri::async_runtime::{JoinHandle, Mutex};
use ts_rs::TS;

use crate::CommandError;
use crate::hosts::Hosts;

pub struct Connections {
    state: std::sync::Mutex<State>,
    /// Where the saved devices live, and their passwords in the credential store, see `Hosts`.
    hosts: PathBuf,
    service: String,
}

#[derive(Default)]
struct State {
    connections: HashMap<String, Arc<Connection>>,
    /// The latest login underway to each host: a disconnect, forget or newer login cancels it.
    logins: HashMap<String, u64>,
    /// The last id given out.
    id: u64,
}

impl State {
    fn next(&mut self) -> u64 {
        self.id += 1;
        self.id
    }

    /// Lets go of `host`'s connection, for the caller to close, and cancels a login to it underway.
    fn remove(&mut self, host: &str) -> Option<Arc<Connection>> {
        self.logins.remove(host);
        self.connections.remove(host)
    }
}

/// A device we're logged in to: its client, and its screen, serial console and the disk image in its drive while
/// they're open.
pub struct Connection {
    pub id: u64,
    pub client: Mutex<Client>,
    pub kvm: Slot<Arc<Kvm>>,
    pub sol: Slot<Arc<Sol>>,
    pub image: Slot<Task>,
}

/// A redirection session while it's open, by the id its commands name.
pub struct Slot<T>(Mutex<Option<(u64, T)>>);

/// What's open in a `Slot`, which ends with a goodbye: a screen or serial console, or the task serving a disk image.
pub trait Redirection {
    /// What it is, in the log.
    const NAME: &str;
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

impl Redirection for Task {
    const NAME: &str = "disk image";
    /// Stops it, and waits until it has: the device's drive is free then.
    async fn close(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

/// A task that stops with the last copy of it: one serving a disk image, or looking for devices.
pub struct Task(pub JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A login that worked, and why its password wasn't remembered, if it was to be.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
pub struct Connected {
    pub info: Info,
    pub unremembered: Option<String>,
}

/// A change to the settings, each made by the `Client` method it's named after.
#[derive(Debug, Deserialize, TS)]
#[ts(export_to = "ooblin.ts")]
pub enum Change {
    RemoteAccess(RemoteAccess),
    General(General),
    PowerPolicy(String),
    Ipv4(Ipv4),
    Ipv6(Ipv6Settings),
    Availability(Availability),
    Recovery(Recovery),
    Clock,
    AddAlarm(Alarm),
    DeleteAlarm(String),
    /// The certificate in the file at `path`, see `Client::add_certificate`.
    AddCertificate {
        path: PathBuf,
        trusted_root: bool,
    },
    DeleteCertificate(String),
}

impl Change {
    async fn make(self, client: &mut Client) -> Result<(), CommandError> {
        match self {
            Change::RemoteAccess(access) => client.set_remote_access(&access).await,
            Change::General(general) => client.set_general(&general).await,
            Change::PowerPolicy(id) => client.set_power_policy(&id).await,
            Change::Ipv4(ipv4) => client.set_ipv4(&ipv4).await,
            Change::Ipv6(ipv6) => client.set_ipv6(&ipv6).await,
            Change::Availability(available) => client.set_availability(&available).await,
            Change::Recovery(recovery) => client.set_recovery(&recovery).await,
            Change::Clock => client.set_clock().await,
            Change::AddAlarm(alarm) => client.add_alarm(&alarm).await,
            Change::DeleteAlarm(name) => client.delete_alarm(&name).await,
            Change::AddCertificate { path, trusted_root } => {
                client.add_certificate(&std::fs::read(path)?, trusted_root).await
            }
            Change::DeleteCertificate(id) => client.delete_certificate(&id).await,
        }?;
        Ok(())
    }
}

/// How a change went: why it failed, if it did, and the settings read after it either way, as a change that failed may
/// have made some of it, or why they couldn't be read.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
pub struct Changed {
    pub error: Option<CommandError>,
    pub settings: Result<Settings, CommandError>,
}

impl Connections {
    pub fn new(hosts: PathBuf, service: String) -> Self {
        Self { state: Default::default(), hosts, service }
    }

    /// The saved devices as they are now, to read.
    pub fn hosts(&self) -> io::Result<Hosts> {
        Hosts::open(&self.hosts, &self.service)
    }

    /// Changes the saved devices.
    pub fn edit<T>(&self, change: impl FnOnce(&mut Hosts) -> io::Result<T>) -> io::Result<T> {
        let _state = self.state.lock().unwrap();
        change(&mut self.hosts()?)
    }

    /// Logs in to `target`'s device and saves it, with its password if we `remember` it and the credential store takes
    /// it. A login cancelled before it's in leaves neither a connection, nor the device saved, nor its password.
    pub async fn connect(&self, target: Target, remember: bool) -> Result<Connected, CommandError> {
        let login = {
            let mut state = self.state.lock().unwrap();
            let id = state.next();
            state.logins.insert(target.host.clone(), id);
            id
        };
        let hosts = self.hosts()?;
        let mut client = Client::new(&target, hosts.pin(&target.host))?;
        let info = client.info().await?;
        let unremembered = if remember { hosts.remember(&target).err() } else { None };
        let remembered = remember && unremembered.is_none();
        let accepted = self.accept(login, &target, &info, client, remembered);
        if accepted.is_err() && remembered {
            hosts.forget_password(&target.host).unwrap_or_default();
        }
        accepted?;
        log::info!("Connected to {} (AMT {}), connection {login}", target.host, info.version);
        Ok(Connected { info, unremembered: unremembered.map(|error| error.to_string()) })
    }

    fn accept(
        &self,
        login: u64,
        target: &Target,
        info: &Info,
        client: Client,
        remembered: bool,
    ) -> Result<(), CommandError> {
        let mut state = self.state.lock().unwrap();
        if state.logins.get(&target.host) != Some(&login) {
            return Err(CommandError::failed(format!("Connecting to {} was cancelled.", target.host)));
        }
        state.logins.remove(&target.host);
        self.hosts()?.connected(target, info, remembered)?;
        let (client, kvm, sol, image) = (Mutex::new(client), Slot::default(), Slot::default(), Slot::default());
        let connection = Connection { id: login, client, kvm, sol, image };
        // A connection replaced lets go of what's open on it, which the webview never does.
        state.connections.insert(target.host.clone(), Arc::new(connection));
        Ok(())
    }

    /// Lets go of `host`'s connection, closing what's open on it, and cancels a login to it underway.
    pub async fn close(&self, host: &str) {
        let connection = self.state.lock().unwrap().remove(host);
        if let Some(connection) = connection {
            connection.close().await;
            log::info!("Disconnected from {host}, connection {}", connection.id);
        }
    }

    /// Forgets `host`, see `Hosts::forget`, letting go of its connection and cancelling a login to it underway.
    pub async fn forget(&self, host: &str) -> Result<(), CommandError> {
        let (connection, remembered) = {
            let mut state = self.state.lock().unwrap();
            (state.remove(host), self.hosts().and_then(|mut hosts| hosts.forget(host)))
        };
        if let Some(connection) = connection {
            connection.close().await;
        }
        if remembered? {
            self.hosts()?.forget_password(host)?;
        }
        log::info!("Forgot {host}");
        Ok(())
    }

    pub fn get(&self, host: &str) -> Result<Arc<Connection>, CommandError> {
        let connection = self.state.lock().unwrap().connections.get(host).cloned();
        connection.ok_or_else(|| not_connected(host))
    }

    /// An id for what opened on `connection`, unless it's no longer `host`'s: closing it closed what was open then,
    /// which this wasn't yet.
    fn opened(&self, host: &str, connection: &Connection) -> Result<u64, CommandError> {
        let mut state = self.state.lock().unwrap();
        match state.connections.get(host) {
            Some(current) if current.id == connection.id => Ok(state.next()),
            _ => Err(not_connected(host)),
        }
    }

    /// Opens a redirection session of `host`'s, as `open` does, into the slot `slot` picks, closing the one before:
    /// AMT takes one of each at a time. Says its id.
    pub async fn open<T: Redirection>(
        &self,
        host: &str,
        slot: impl FnOnce(&Connection) -> &Slot<T>,
        open: impl AsyncFnOnce(&mut Client) -> amt_core::Result<T>,
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

    /// Makes `change` to `host`'s settings, then reads them again: the name goes in the saved devices, and a disk
    /// image comes out of a drive that's off.
    pub async fn change(&self, host: &str, change: Change) -> Result<Changed, CommandError> {
        let connection = self.get(host)?;
        log::info!("Changing {host}'s settings: {change:?}");
        let (error, settings) = {
            let mut client = connection.client.lock().await;
            (change.make(&mut client).await.err(), client.settings().await.map_err(CommandError::from))
        };
        if let Some(error) = &error {
            log::warn!("Changing {host}'s settings failed: {error}");
        }
        match &settings {
            Ok(settings) => {
                let name = &settings.general.name;
                if let Err(error) = self.edit(|hosts| hosts.renamed(host, name)) {
                    log::warn!("Saving {host}'s name {name} failed: {error}");
                }
                if !settings.remote_access.ider {
                    connection.image.close(None).await;
                }
            }
            Err(error) => log::warn!("Reading {host}'s settings after a change failed: {error}"),
        }
        Ok(Changed { error, settings })
    }
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<T: Redirection + Clone> Slot<T> {
    /// The one `id` names, while it's open.
    pub async fn get(&self, id: u64) -> Result<T, CommandError> {
        let slot = self.0.lock().await;
        let open = slot.as_ref().filter(|(open, _)| *open == id).map(|(_, open)| open.clone());
        open.ok_or_else(|| CommandError::failed("It's closed."))
    }
}

impl<T: Redirection> Slot<T> {
    /// Closes the one `id` names, or the one that's open.
    pub async fn close(&self, id: Option<u64>) {
        let mut slot = self.0.lock().await;
        if let Some(open) = slot.take_if(|(open, _)| id.is_none_or(|id| id == *open)) {
            close(open).await;
        }
    }
}

/// Closes what was open in a slot, by its id.
async fn close<T: Redirection>((id, open): (u64, T)) {
    open.close().await;
    log::info!("Closed {} {id}", T::NAME);
}

impl Slot<Task> {
    /// Whether the device can read a disk image: one is in the drive, and still served.
    pub async fn serving(&self) -> bool {
        self.0.lock().await.as_ref().is_some_and(|(_, task)| !task.0.inner().is_finished())
    }
}

impl Connection {
    /// Closes what's open on it.
    async fn close(&self) {
        self.kvm.close(None).await;
        self.sol.close(None).await;
        self.image.close(None).await;
    }
}

fn not_connected(host: &str) -> CommandError {
    CommandError::failed(format!("Not connected to {host}."))
}
