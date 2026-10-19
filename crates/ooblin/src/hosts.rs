//! Every device we logged in to, in `hosts.json`: its pinned certificate, name and AMT version, the user that worked and
//! whether we remember its password. Passwords only live in the OS credential store, Digest needs the real thing (or its
//! HA1, which is just as good to an attacker). Both are keyed by the app's identifier, so the verify build never sees the real ones.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use amt_core::{Info, Target};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use ts_rs::TS;

use crate::CommandError;

pub struct Hosts {
    path: PathBuf,
    service: String,
    hosts: BTreeMap<String, Saved>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    pin: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    user: String,
    remembered: bool,
}

/// A device as the webview sees it. We keep reaching it at the address it was given as, as AMT's own `name` (with its
/// domain) may well not be in DNS. `remembered` means we can log in without asking for the password.
#[derive(Serialize, TS)]
#[ts(export, export_to = "ooblin.ts")]
pub struct Device {
    pub host: String,
    pub name: String,
    pub version: String,
    pub user: String,
    pub remembered: bool,
}

impl Hosts {
    pub fn open(app: &AppHandle) -> io::Result<Self> {
        let path = app.path().app_config_dir().map_err(io::Error::other)?.join("hosts.json");
        let hosts = match std::fs::read(&path) {
            Ok(json) => serde_json::from_slice(&json)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        Ok(Self { path, service: app.config().identifier.clone(), hosts })
    }

    pub fn pin(&self, host: &str) -> Option<String> {
        Some(self.hosts.get(host)?.pin.clone())
    }

    pub fn devices(&self) -> Vec<Device> {
        self.hosts.keys().filter_map(|host| self.device(host)).collect()
    }

    pub fn device(&self, host: &str) -> Option<Device> {
        let saved = self.hosts.get(host)?;
        let (name, version, user) = (saved.name.clone(), saved.version.clone(), saved.user.clone());
        Some(Device { host: host.to_owned(), name, version, user, remembered: saved.remembered })
    }

    /// After a successful login: we trust the certificate it used and remember its name, version and the user that
    /// worked.
    pub fn connected(&mut self, target: &Target, info: &Info) -> io::Result<()> {
        let remembered = self.hosts.get(&target.host).is_some_and(|saved| saved.remembered);
        let (pin, name, version) = (info.fingerprint.clone(), info.name.clone(), info.version.clone());
        self.hosts.insert(target.host.clone(), Saved { pin, name, version, user: target.user.clone(), remembered });
        self.save()
    }

    /// The device goes by another name now.
    pub fn renamed(&mut self, host: &str, name: &str) -> io::Result<()> {
        match self.hosts.get_mut(host) {
            Some(saved) if saved.name != name => {
                saved.name = name.to_owned();
                self.save()
            }
            _ => Ok(()),
        }
    }

    /// Only called after the user compared fingerprints and chose to trust the new one.
    pub fn trust(&mut self, host: &str, pin: String) -> io::Result<()> {
        if let Some(saved) = self.hosts.get_mut(host) {
            saved.pin = pin;
        }
        self.save()
    }

    pub fn remember(&mut self, target: &Target) -> Result<(), CommandError> {
        Entry::new(&self.service, &target.host)?.set_password(&target.password)?;
        if let Some(saved) = self.hosts.get_mut(&target.host) {
            saved.remembered = true;
        }
        Ok(self.save()?)
    }

    /// The remembered login for `host`.
    pub fn target(&self, host: &str) -> Result<Target, CommandError> {
        let saved = self.hosts.get(host).filter(|saved| saved.remembered);
        let saved = saved.ok_or_else(|| CommandError::failed(format!("No saved password for {host}.")))?;
        let password = Entry::new(&self.service, host)?.get_password()?;
        Ok(Target::new(host, &saved.user, password))
    }

    /// Forgets the pin too, the next connection starts from scratch.
    pub fn forget(&mut self, host: &str) -> Result<(), CommandError> {
        if self.hosts.remove(host).is_some_and(|saved| saved.remembered) {
            match Entry::new(&self.service, host)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(self.save()?)
    }

    fn save(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.path.parent().expect("the config dir has a parent"))?;
        std::fs::write(&self.path, serde_json::to_vec_pretty(&self.hosts)?)
    }
}
