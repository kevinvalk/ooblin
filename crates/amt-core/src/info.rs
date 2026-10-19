use serde::Serialize;
use ts_rs::TS;

use crate::{Client, Result};

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// The host name, with its domain if it has one.
    pub name: String,
    pub version: String,
    pub build: Option<String>,
    /// "Admin" or "Client", only admin control mode redirects without asking someone at the device for consent.
    pub control_mode: String,
    /// The pinned certificate's, see `fingerprint`.
    pub fingerprint: String,
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
        let (host, domain) = (settings.text("HostName"), settings.text("DomainName"));
        Ok(Info {
            name: if domain.is_empty() { host } else { format!("{host}.{domain}") },
            version: firmware("AMT").unwrap_or_default(),
            build: firmware("Build Number"),
            control_mode: match setup.get("ProvisioningMode") {
                Some("1") => "Admin",
                Some("4") => "Client",
                _ => "",
            }
            .to_owned(),
            fingerprint: self.fingerprint().expect("a successful call has pinned a certificate"),
        })
    }
}
