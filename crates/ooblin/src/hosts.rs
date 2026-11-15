//! Every device we logged in to, in `hosts.json`: its pinned certificate, name, flavor and firmware version, the user
//! that worked and whether we remember its password. Passwords only live in the OS credential store, Digest needs the
//! real thing (or its HA1, which is just as good to an attacker). Both are keyed by the app's identifier, so the verify
//! build never sees the real ones. Only `Connections` changes them, one change at a time.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use keyring::Entry;
use ooblin_core::{Flavor, Info, Target};
use serde::{Deserialize, Serialize};
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
    /// Saves from before BMCs are AMT's.
    #[serde(default)]
    flavor: Flavor,
}

/// A device as the webview sees it. We keep reaching it at the address it was given as, as AMT's own `name` (with its
/// domain) may well not be in DNS. `remembered` means we can log in without asking for the password.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
pub struct Device {
    pub host: String,
    pub name: String,
    pub version: String,
    pub user: String,
    pub remembered: bool,
    /// Unknown until we logged in to it.
    pub flavor: Option<Flavor>,
}

impl Hosts {
    /// The devices saved at `path`, their passwords under `service` in the credential store.
    pub fn open(path: &Path, service: &str) -> io::Result<Self> {
        let hosts = match std::fs::read(path) {
            Ok(json) => serde_json::from_slice(&json)
                .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        Ok(Self { path: path.to_owned(), service: service.to_owned(), hosts })
    }

    /// None for a device only ever logged in to over IPMI, which has no certificate.
    pub fn pin(&self, host: &str) -> Option<String> {
        Some(self.hosts.get(host)?.pin.clone()).filter(|pin| !pin.is_empty())
    }

    pub fn flavor(&self, host: &str) -> Option<Flavor> {
        Some(self.hosts.get(host)?.flavor)
    }

    pub fn devices(&self) -> Vec<Device> {
        self.hosts.keys().filter_map(|host| self.device(host)).collect()
    }

    pub fn device(&self, host: &str) -> Option<Device> {
        let saved = self.hosts.get(host)?;
        let (name, version, user) = (saved.name.clone(), saved.version.clone(), saved.user.clone());
        let (remembered, flavor) = (saved.remembered, Some(saved.flavor));
        Some(Device { host: host.to_owned(), name, version, user, remembered, flavor })
    }

    /// After a successful login: we trust the certificate it used and remember its name, flavor, version, the user that
    /// worked and whether we keep its password, which we still do if we did.
    pub fn connected(&mut self, target: &Target, info: &Info, remembered: bool) -> io::Result<()> {
        let before = self.hosts.get(&target.host);
        let remembered = remembered || before.is_some_and(|saved| saved.remembered);
        // An IPMI login has no certificate: the one pinned before stays, for when it's spoken to over TLS again.
        let pin = Some(info.fingerprint.clone()).filter(|pin| !pin.is_empty());
        let pin = pin.or_else(|| Some(before?.pin.clone())).unwrap_or_default();
        let (name, version) = (info.name.clone(), info.version.clone());
        let saved = Saved { pin, name, version, user: target.user.clone(), remembered, flavor: info.flavor };
        self.hosts.insert(target.host.clone(), saved);
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

    /// Keeps the password of `target` in the credential store, for `connected` to say so.
    pub fn remember(&self, target: &Target) -> keyring::Result<()> {
        Entry::new(&self.service, &target.host)?.set_password(&target.password)
    }

    /// The remembered login for `host`.
    pub fn target(&self, host: &str) -> Result<Target, CommandError> {
        let saved = self.hosts.get(host).filter(|saved| saved.remembered);
        let saved = saved.ok_or_else(|| CommandError::failed(format!("No saved password for {host}.")))?;
        let password = Entry::new(&self.service, host)?.get_password()?;
        Ok(Target::new(host, &saved.user, password))
    }

    /// Forgets the pin too, the next connection starts from scratch. Says whether we kept its password, for
    /// `forget_password`.
    pub fn forget(&mut self, host: &str) -> io::Result<bool> {
        let remembered = self.hosts.remove(host).is_some_and(|saved| saved.remembered);
        self.save()?;
        Ok(remembered)
    }

    pub fn forget_password(&self, host: &str) -> keyring::Result<()> {
        match Entry::new(&self.service, host)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Whole or not at all: whoever reads it, or a crash halfway, finds the old file or the new one. A rename replaces
    /// the file on Windows too.
    fn save(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.path.parent().expect("the config dir has a parent"))?;
        let new = self.path.with_extension("json.new");
        let mut file = std::fs::File::create(&new)?;
        file.write_all(&serde_json::to_vec_pretty(&self.hosts)?)?;
        file.sync_all()?;
        std::fs::rename(&new, &self.path)
    }
}
