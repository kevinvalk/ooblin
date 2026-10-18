use serde::Serialize;

use crate::{Client, Fingerprint, Result, Trust};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub hostname: String,
    pub domain: String,
    pub version: String,
    pub build: Option<String>,
    pub control_mode: ControlMode,
    pub digest_realm: String,
    pub fingerprint: Fingerprint,
    pub trust: Trust,
}

/// Only admin control mode lets us redirect without asking the user at the device for consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
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

        Ok(Info {
            hostname: settings.text("HostName"),
            domain: settings.text("DomainName"),
            version: firmware("AMT").unwrap_or_default(),
            build: firmware("Build Number"),
            control_mode: match setup.get("ProvisioningMode") {
                Some("1") => ControlMode::Admin,
                Some("4") => ControlMode::Client,
                _ => ControlMode::Unknown,
            },
            digest_realm: settings.text("DigestRealm"),
            fingerprint: self.fingerprint().expect("a successful call has pinned a certificate"),
            trust: self.trust(),
        })
    }
}
