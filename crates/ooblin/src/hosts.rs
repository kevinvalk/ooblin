use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use keyring::Entry;
use ooblin_core::{Flavor, Info, Target};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::CommandError;

// NOTE: Passwords only live in the OS credential store, because Digest needs the real thing (or its HA1, which is just
// as good to an attacker). The file and the store are both keyed by the app's identifier, so the verify build never
// sees the real ones. Only `Connections` changes them, one change at a time.
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
    #[serde(alias = "remembered")] // A file saved before we called it `is_remembered` still has the old key.
    is_remembered: bool,
    #[serde(default)] // A device saved before we knew BMCs has no flavor, so it is one of AMT's.
    flavor: Flavor,
}

/// A device as the webview sees it. We keep reaching it at the `host` it was given as, because AMT's own `name` (with
/// its domain) may well not be in DNS. `is_remembered` means we can log in without asking for the password.
#[derive(Serialize, TS)]
#[ts(export_to = "ooblin.ts")]
pub struct Device {
    pub host: String,
    pub name: String,
    pub version: String,
    pub user: String,
    pub is_remembered: bool,
    /// Unknown until we logged in to it.
    pub flavor: Option<Flavor>,
}

impl Hosts {
    pub fn open(path: &Path, service: &str) -> io::Result<Self> {
        let hosts = match std::fs::read(path) {
            Ok(json) => serde_json::from_slice(&json)
                .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };

        Ok(Self { path: path.to_owned(), service: service.to_owned(), hosts })
    }

    /// None for a device we only ever logged in to over IPMI, as that has no certificate.
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
        let (is_remembered, flavor) = (saved.is_remembered, Some(saved.flavor));
        Some(Device { host: host.to_owned(), name, version, user, is_remembered, flavor })
    }

    /// After a successful login we trust the certificate it used. A password we kept before stays kept, even when this
    /// login did not ask for it.
    pub fn connected(&mut self, target: &Target, info: &Info, is_remembered: bool) -> io::Result<()> {
        let saved_before = self.hosts.get(&target.host);
        let is_remembered = is_remembered || saved_before.is_some_and(|saved| saved.is_remembered);

        // An IPMI login has no certificate, so we keep the one pinned before for when we speak to it over TLS again.
        let pin = Some(info.fingerprint.clone()).filter(|pin| !pin.is_empty());
        let pin = pin.or_else(|| Some(saved_before?.pin.clone())).unwrap_or_default();

        let (name, version) = (info.name.clone(), info.version.clone());
        let saved = Saved { pin, name, version, user: target.user.clone(), is_remembered, flavor: info.flavor };
        self.hosts.insert(target.host.clone(), saved);
        self.save()
    }

    pub fn renamed(&mut self, host: &str, name: &str) -> io::Result<()> {
        match self.hosts.get_mut(host) {
            Some(saved) if saved.name != name => {
                saved.name = name.to_owned();
                self.save()
            }
            _ => Ok(()),
        }
    }

    /// IMPORTANT: Only call this after the user compared the fingerprints and chose to trust the new one.
    pub fn trust(&mut self, host: &str, pin: String) -> io::Result<()> {
        if let Some(saved) = self.hosts.get_mut(host) {
            saved.pin = pin;
        }
        self.save()
    }

    /// We only keep the password in the credential store here, `connected` is what saves that we did.
    pub fn remember(&self, target: &Target) -> keyring::Result<()> {
        Entry::new(&self.service, &target.host)?.set_password(&target.password)
    }

    pub fn target(&self, host: &str) -> Result<Target, CommandError> {
        let saved = self.hosts.get(host).filter(|saved| saved.is_remembered);
        let saved = saved.ok_or_else(|| CommandError::failed(format!("No saved password for {host}.")))?;

        let password = Entry::new(&self.service, host)?.get_password()?;
        Ok(Target::new(host, &saved.user, password))
    }

    /// We forget the pin too, so the next connection starts from scratch. Returns whether we kept its password, which
    /// `forget_password` then has to take out of the credential store.
    pub fn forget(&mut self, host: &str) -> io::Result<bool> {
        let is_remembered = self.hosts.remove(host).is_some_and(|saved| saved.is_remembered);
        self.save()?;
        Ok(is_remembered)
    }

    pub fn forget_password(&self, host: &str) -> keyring::Result<()> {
        match Entry::new(&self.service, host)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn save(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.path.parent().expect("the configuration directory has a parent"))?;

        // We write a new file and rename it over the old one, so whoever reads it (or a crash halfway) finds the old
        // file or the new one and never half of one. A rename replaces the file on Windows too.
        let new_path = self.path.with_extension("json.new");
        let mut file = std::fs::File::create(&new_path)?;
        file.write_all(&serde_json::to_vec_pretty(&self.hosts)?)?;
        file.sync_all()?;
        std::fs::rename(&new_path, &self.path)
    }
}
