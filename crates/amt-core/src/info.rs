use serde::Serialize;
use ts_rs::TS;

use crate::{Client, Fingerprint, Result, Trust};

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// The host name, with its domain if it has one.
    pub name: String,
    /// "19.0.5 (build 1992)"
    pub version: String,
    pub control_mode: ControlMode,
    pub fingerprint: Fingerprint,
    pub trust: Trust,
}

/// Only admin control mode lets us redirect without asking the user at the device for consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum ControlMode {
    Admin,
    Client,
    Unknown,
}

impl Client {
    pub async fn info(&mut self) -> Result<Info> {
        let settings = self.get("AMT_GeneralSettings").await?;
        let setup = self.get("AMT_SetupAndConfigurationService").await?;
        let software = self.enumerate("CIM_SoftwareIdentity").await?;

        let firmware = |id| {
            let component = software.iter().find(|component| component.get("InstanceID") == Some(id))?;
            component.get("VersionString").map(str::to_owned)
        };

        let (host, domain, version) = (settings.text("HostName"), settings.text("DomainName"), firmware("AMT"));
        Ok(Info {
            name: if domain.is_empty() { host } else { format!("{host}.{domain}") },
            version: match firmware("Build Number") {
                Some(build) => format!("{} (build {build})", version.unwrap_or_default()),
                None => version.unwrap_or_default(),
            },
            control_mode: match setup.get("ProvisioningMode") {
                Some("1") => ControlMode::Admin,
                Some("4") => ControlMode::Client,
                _ => ControlMode::Unknown,
            },
            fingerprint: self.fingerprint().expect("a successful call has pinned a certificate"),
            trust: self.trust(),
        })
    }
}
