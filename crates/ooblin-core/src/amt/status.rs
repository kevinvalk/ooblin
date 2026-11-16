use serde::Serialize;
use ts_rs::TS;

use super::Client;
use super::wsman::Instance;
use crate::{Configuration, Info, Ipv4, Ipv6, Port, PowerState, Result};

/// What the device's own status page shows: the device, its AMT and its network.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Status {
    pub info: Info,
    pub power: PowerState,
    pub system_id: String,
    pub ports: Vec<Port>,
}

impl Ipv4 {
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

impl Client {
    pub async fn status(&mut self) -> Result<Status> {
        let info = self.info().await?;
        let power = self.power().await?;
        let system_id = self.system_id().await?;
        let settings = self.enumerate("AMT_EthernetPortSettings").await?;
        let ports = self.enumerate("CIM_EthernetPort").await?;
        let ipv6 = self.enumerate("IPS_IPv6PortSettings").await?;

        // NOTE: We rely on the three classes listing the ports in the same order.
        let ports = settings.iter().zip(&ports).enumerate().map(|(i, (settings, port))| {
            let ipv6 = ipv6.get(i);

            // AMT reports an unset IPv6 address as `::`, which we leave out.
            let ipv6_address = |name| {
                ipv6.map(|port| port.text(name).to_lowercase()).filter(|address| address != "::").unwrap_or_default()
            };

            // Each is `FE80::751:CC63:480F:CF6F,0,3`: the address, its kind and its state. We write them lowercase
            // because that is how RFC 5952 has it.
            let addresses: Vec<_> = ipv6
                .into_iter()
                .flat_map(|port| port.all("CurrentAddressInfo"))
                .filter_map(|address_info| address_info.split(',').next())
                .map(str::to_lowercase)
                .collect();

            Port {
                is_wireless: port.text("Description").starts_with("Wireless"),
                // AMT writes `12-07-14-5f-37-f1`, we write every flavor's the same way: `12:07:14:5F:37:F1`.
                mac: crate::mac(&settings.text("MACAddress")),
                is_link_up: settings.parse("LinkIsUp").unwrap_or_default(),
                mbps: port.parse::<u64>("Speed").filter(|&bits| bits > 0).map(|bits| bits / 1_000_000),
                ipv4: Ipv4::read(settings),
                ipv6: (!addresses.is_empty()).then(|| Ipv6 {
                    addresses,
                    gateway: ipv6_address("CurrentDefaultRouter"),
                    primary_dns: ipv6_address("CurrentPrimaryDNS"),
                    secondary_dns: ipv6_address("CurrentSecondaryDNS"),
                }),
            }
        });

        Ok(Status { info, power: power.state, system_id, ports: ports.collect() })
    }
}
