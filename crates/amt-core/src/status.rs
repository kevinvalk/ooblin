use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::hardware::guid;
use crate::wsman::Instance;
use crate::{Client, Info, PowerState, Result};

/// What the device's own status page shows: the device, its AMT and its network.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Status {
    pub info: Info,
    pub power: PowerState,
    pub system_id: String,
    pub ports: Vec<Port>,
}

/// AMT has one wired port and maybe a wireless one.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Port {
    pub wireless: bool,
    pub mac: String,
    pub link_up: bool,
    pub mbps: Option<u64>,
    pub ipv4: Ipv4,
    /// `None` when IPv6 is off.
    pub ipv6: Option<Ipv6>,
}

/// The addresses are what AMT uses, which it gets from DHCP, the OS or what was set (`Manual`).
#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Ipv4 {
    pub configuration: Configuration,
    pub address: String,
    pub subnet_mask: String,
    pub gateway: String,
    pub primary_dns: String,
    pub secondary_dns: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "core.ts")]
pub enum Configuration {
    Dhcp,
    /// A static address shared with the OS, which AMT takes from it.
    Os,
    Manual,
}

impl Ipv4 {
    /// From a port's `AMT_EthernetPortSettings`.
    pub(crate) fn read(settings: &Instance) -> Self {
        Self {
            configuration: match (settings.get("DHCPEnabled"), settings.get("IpSyncEnabled")) {
                (Some("true"), _) => Configuration::Dhcp,
                (_, Some("true")) => Configuration::Os,
                _ => Configuration::Manual,
            },
            address: settings.text("IPAddress"),
            subnet_mask: settings.text("SubnetMask"),
            gateway: settings.text("DefaultGateway"),
            primary_dns: settings.text("PrimaryDNS"),
            secondary_dns: settings.text("SecondaryDNS"),
        }
    }
}

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Ipv6 {
    pub addresses: Vec<String>,
    pub gateway: String,
    pub primary_dns: String,
    pub secondary_dns: String,
}

impl Client {
    pub async fn status(&mut self) -> Result<Status> {
        let info = self.info().await?;
        let power = self.power().await?;
        let package = self.enumerate("CIM_ComputerSystemPackage").await?;
        let settings = self.enumerate("AMT_EthernetPortSettings").await?;
        let ports = self.enumerate("CIM_EthernetPort").await?;
        let ipv6 = self.enumerate("IPS_IPv6PortSettings").await?;

        // The three classes list the ports in the same order.
        let ports = settings.iter().zip(&ports).enumerate().map(|(i, (settings, port))| {
            let ipv6 = ipv6.get(i);
            // AMT reports an unset IPv6 address as `::`.
            let v6 = |name| {
                ipv6.map(|port| port.text(name).to_lowercase()).filter(|address| address != "::").unwrap_or_default()
            };
            // Each is `FE80::751:CC63:480F:CF6F,0,3`: the address, its kind and its state. We write them lowercase, as
            // RFC 5952 has it.
            let addresses: Vec<_> = ipv6
                .into_iter()
                .flat_map(|port| port.all("CurrentAddressInfo"))
                .filter_map(|info| info.split(',').next())
                .map(str::to_lowercase)
                .collect();
            Port {
                wireless: port.text("Description").starts_with("Wireless"),
                // AMT writes `12-07-14-5f-37-f1`, we write them all the same: `12:07:14:5F:37:F1`.
                mac: settings.text("MACAddress").to_uppercase().replace('-', ":"),
                link_up: settings.parse("LinkIsUp").unwrap_or_default(),
                mbps: port.parse::<u64>("Speed").filter(|&bits| bits > 0).map(|bits| bits / 1_000_000),
                ipv4: Ipv4::read(settings),
                ipv6: (!addresses.is_empty()).then(|| Ipv6 {
                    addresses,
                    gateway: v6("CurrentDefaultRouter"),
                    primary_dns: v6("CurrentPrimaryDNS"),
                    secondary_dns: v6("CurrentSecondaryDNS"),
                }),
            }
        });

        Ok(Status {
            info,
            power: power.state,
            system_id: package.first().map(|package| guid(&package.text("PlatformGUID"))).unwrap_or_default(),
            ports: ports.collect(),
        })
    }
}
