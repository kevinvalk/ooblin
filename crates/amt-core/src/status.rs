use instant_xml::ToXml;
use serde::Serialize;
use ts_rs::TS;

use crate::hardware::guid;
use crate::wsman::Method;
use crate::{Client, Info, PowerState, Result};

/// What the device's own status page shows, and a bit more: the AMT itself, remote access and the network.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Status {
    pub info: Info,
    pub power: PowerState,
    pub system_id: String,
    /// Unix seconds.
    pub clock: Option<u32>,
    pub redirection: bool,
    pub serial_over_lan: bool,
    pub ide_redirection: bool,
    pub kvm: bool,
    /// When someone at the device has to read out a code first: "Not required", "For KVM" or "For all redirection".
    pub user_consent: String,
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

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Ipv4 {
    pub dhcp: bool,
    pub address: String,
    pub subnet_mask: String,
    pub gateway: String,
    pub primary_dns: String,
    pub secondary_dns: String,
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
        let clock = self.query(&GetLowAccuracyTimeSynch).await?;
        let settings = self.enumerate("AMT_EthernetPortSettings").await?;
        let ports = self.enumerate("CIM_EthernetPort").await?;
        let ipv6 = self.enumerate("IPS_IPv6PortSettings").await?;
        let redirection = self.get("AMT_RedirectionService").await?;
        let kvm = self.enumerate("CIM_KVMRedirectionSAP").await?;
        let opt_in = self.get("IPS_OptInService").await?;

        // The three classes list the ports in the same order.
        let ports = settings.iter().zip(&ports).enumerate().map(|(i, (settings, port))| {
            let ipv6 = ipv6.get(i);
            // AMT reports an unset IPv6 address as `::`.
            let v6 = |name| ipv6.map(|port| port.text(name)).filter(|address| address != "::").unwrap_or_default();
            let addresses: Vec<_> =
                ipv6.into_iter().flat_map(|port| port.all("CurrentAddressInfo")).map(str::to_owned).collect();
            Port {
                wireless: port.text("Description").starts_with("Wireless"),
                mac: settings.text("MACAddress"),
                link_up: settings.parse("LinkIsUp").unwrap_or_default(),
                mbps: port.parse::<u64>("Speed").filter(|&bits| bits > 0).map(|bits| bits / 1_000_000),
                ipv4: Ipv4 {
                    dhcp: settings.parse("DHCPEnabled").unwrap_or_default(),
                    address: settings.text("IPAddress"),
                    subnet_mask: settings.text("SubnetMask"),
                    gateway: settings.text("DefaultGateway"),
                    primary_dns: settings.text("PrimaryDNS"),
                    secondary_dns: settings.text("SecondaryDNS"),
                },
                ipv6: (!addresses.is_empty()).then(|| Ipv6 {
                    addresses,
                    gateway: v6("CurrentDefaultRouter"),
                    primary_dns: v6("CurrentPrimaryDNS"),
                    secondary_dns: v6("CurrentSecondaryDNS"),
                }),
            }
        });

        let state = redirection.parse::<u32>("EnabledState").unwrap_or(32768);
        Ok(Status {
            info,
            power: power.state,
            system_id: package.first().map(|package| guid(&package.text("PlatformGUID"))).unwrap_or_default(),
            clock: clock.parse("Ta0"),
            redirection: redirection.parse("ListenerEnabled").unwrap_or_default(),
            serial_over_lan: state & 2 != 0,
            ide_redirection: state & 1 != 0,
            // KVM is enabled when its access point is enabled (2) or enabled but idle (6).
            kvm: kvm.first().is_some_and(|kvm| matches!(kvm.get("EnabledState"), Some("2" | "6"))),
            user_consent: match opt_in.get("OptInRequired") {
                Some("0") => "Not required",
                Some("1") => "For KVM",
                _ => "For all redirection",
            }
            .to_owned(),
            ports: ports.collect(),
        })
    }
}

const TIME_SYNCHRONIZATION: &str = "http://intel.com/wbem/wscim/1/amt-schema/1/AMT_TimeSynchronizationService";

#[derive(ToXml)]
#[xml(rename = "GetLowAccuracyTimeSynch_INPUT", ns(TIME_SYNCHRONIZATION))]
struct GetLowAccuracyTimeSynch;

impl Method for GetLowAccuracyTimeSynch {
    const URI: &'static str = TIME_SYNCHRONIZATION;
    const NAME: &'static str = "GetLowAccuracyTimeSynch";
}
