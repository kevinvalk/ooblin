//! A webview over `amt-core`. The webview never holds a socket, it calls these commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use amt_core::{Client, Error, Fingerprint, Info, Target, Trust};
use serde::Serialize;
use tauri::{AppHandle, Manager};

#[tauri::command]
async fn connect(app: AppHandle, target: Target) -> Result<Info, CommandError> {
    let mut pins = Pins::open(&app)?;
    let info = Client::new(&target, pins.get(&target.host))?.info().await?;
    if info.trust == Trust::FirstUse {
        pins.set(&target.host, info.fingerprint)?;
    }
    Ok(info)
}

/// Only called after the user compared both fingerprints and chose to trust the new one.
#[tauri::command]
fn trust(app: AppHandle, host: &str, fingerprint: Fingerprint) -> Result<(), CommandError> {
    Ok(Pins::open(&app)?.set(host, fingerprint)?)
}

/// Certificate pins per host, in `pins.json` in the app's config directory.
struct Pins {
    path: PathBuf,
    pins: BTreeMap<String, Fingerprint>,
}

impl Pins {
    fn open(app: &AppHandle) -> io::Result<Self> {
        let path = app.path().app_config_dir().map_err(io::Error::other)?.join("pins.json");
        let pins = match std::fs::read(&path) {
            Ok(json) => serde_json::from_slice(&json)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        Ok(Self { path, pins })
    }

    fn get(&self, host: &str) -> Option<Fingerprint> {
        self.pins.get(host).copied()
    }

    fn set(&mut self, host: &str, fingerprint: Fingerprint) -> io::Result<()> {
        self.pins.insert(host.to_owned(), fingerprint);
        std::fs::create_dir_all(self.path.parent().expect("the config dir has a parent"))?;
        std::fs::write(&self.path, serde_json::to_vec_pretty(&self.pins)?)
    }
}

/// What the webview gets when a command fails: always a message, and both fingerprints when the certificate changed.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum CommandError {
    PinMismatch {
        message: String,
        expected: Fingerprint,
        actual: Fingerprint,
    },
    Failed {
        message: String,
    },
}

impl From<Error> for CommandError {
    fn from(error: Error) -> Self {
        match error {
            Error::PinMismatch { expected, actual } => Self::PinMismatch {
                message: error.to_string(),
                expected,
                actual,
            },
            error => Self::Failed {
                message: error.to_string(),
            },
        }
    }
}

impl From<io::Error> for CommandError {
    fn from(error: io::Error) -> Self {
        Self::Failed {
            message: error.to_string(),
        }
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![connect, trust])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime starts");
}
