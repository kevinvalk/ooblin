use super::Client;
use super::hardware::guid;
use super::wsman::Instance;
use crate::{Configuration, Ipv4, Ipv6, Port, Result, Status};

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
                mac: crate::mac(&settings.text("MACAddress")),
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
