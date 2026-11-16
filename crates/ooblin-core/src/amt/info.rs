use super::Client;
use super::settings::name;
use crate::{Flavor, Info, Result};

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
            flavor: Flavor::Amt,
            name: name(&settings),
            version: firmware("AMT").unwrap_or_default(),
            build: firmware("Build Number").unwrap_or_default(),
            control_mode: match setup.get("ProvisioningMode") {
                Some("1") => "Admin",
                Some("4") => "Client",
                _ => "",
            }
            .to_owned(),
            fingerprint: self
                .fingerprint()
                .expect("No pinned certificate after a successful call, did we skip the TLS handshake?!"),
            remote_access: self.remote_access().await?,
        })
    }
}
