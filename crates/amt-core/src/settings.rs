use instant_xml::ToXml;
use serde::Serialize;
use ts_rs::TS;

use crate::decode::yes;
use crate::logs::REALMS;
use crate::wsman::Method;
use crate::{Client, Result};

/// How the device is set up: power policy, ping, name and accounts. What it allows remotely is in its status.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub power_policy: String,
    pub respond_to_ping: String,
    pub host_name: String,
    pub domain_name: String,
    /// Whether the OS's name is AMT's too.
    pub shared_with_os: String,
    pub accounts: Vec<Account>,
}

/// The admin comes first and may do anything, the others what their realms allow, from where they may.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub name: String,
    /// "Administrator", or "Redirection, Hardware Asset, … (network only)".
    pub role: String,
}

impl Client {
    pub async fn settings(&mut self) -> Result<Settings> {
        let general = self.get("AMT_GeneralSettings").await?;
        let schemes = self.enumerate("AMT_SystemPowerScheme").await?;
        let element_settings = self.enumerate("CIM_ElementSettingData").await?;

        // The active power scheme is the current setting that points at one, its GUID being the reference's last line.
        let active = element_settings.iter().find_map(|setting| {
            let reference = setting.get("SettingData").filter(|reference| reference.contains("AMT_SystemPowerScheme"));
            reference.filter(|_| setting.get("IsCurrent") == Some("1")).and_then(|reference| reference.lines().last())
        });
        let scheme = schemes.iter().find(|scheme| scheme.get("SchemeGUID") == active);
        Ok(Settings {
            power_policy: scheme.map(|scheme| scheme.text("Description")).unwrap_or_default(),
            respond_to_ping: yes(general.get("PingResponseEnabled")),
            host_name: general.text("HostName"),
            domain_name: general.text("DomainName"),
            shared_with_os: yes(general.get("SharedFQDN")),
            accounts: self.accounts().await?,
        })
    }

    /// The admin and every other account, hidden ones (named `$$…`, AMT's own) left out.
    async fn accounts(&mut self) -> Result<Vec<Account>> {
        let admin = self.query(&GetAdminAclEntry).await?;
        let mut accounts = vec![Account { name: admin.text("Username"), role: "Administrator".to_owned() }];
        let handles = self.query(&EnumerateUserAclEntries { StartIndex: 1 }).await?;
        for handle in handles.all("Handles").map(str::to_owned).collect::<Vec<_>>() {
            let entry = self.query(&GetUserAclEntryEx { Handle: &handle }).await?;
            let name = entry.get("DigestUsername").unwrap_or("Kerberos user").to_owned();
            if name.starts_with("$$") {
                continue;
            }
            let realms = entry.all("Realms").filter_map(|realm| REALMS.get(realm.parse::<usize>().ok()?));
            let realms: Vec<_> = realms.filter(|name| !name.is_empty()).copied().collect();
            let access = match entry.get("AccessPermission") {
                Some("0") => "local only",
                Some("1") => "network only",
                _ => "local and network",
            };
            accounts.push(Account { name, role: format!("{} ({access})", realms.join(", ")) });
        }
        Ok(accounts)
    }
}

const AUTHORIZATION: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_AuthorizationService";

#[derive(ToXml)]
#[xml(rename = "GetAdminAclEntry_INPUT", ns(AUTHORIZATION))]
struct GetAdminAclEntry;

impl Method for GetAdminAclEntry {
    const URI: &'static str = AUTHORIZATION;
    const NAME: &'static str = "GetAdminAclEntry";
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(rename = "EnumerateUserAclEntries_INPUT", ns(AUTHORIZATION))]
struct EnumerateUserAclEntries {
    StartIndex: u32,
}

impl Method for EnumerateUserAclEntries {
    const URI: &'static str = AUTHORIZATION;
    const NAME: &'static str = "EnumerateUserAclEntries";
}

#[allow(non_snake_case)]
#[derive(ToXml)]
#[xml(rename = "GetUserAclEntryEx_INPUT", ns(AUTHORIZATION))]
struct GetUserAclEntryEx<'a> {
    Handle: &'a str,
}

impl Method for GetUserAclEntryEx<'_> {
    const URI: &'static str = AUTHORIZATION;
    const NAME: &'static str = "GetUserAclEntryEx";
}
