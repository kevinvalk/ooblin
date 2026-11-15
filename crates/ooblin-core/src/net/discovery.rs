//! Finding devices: an ASF presence ping (RMCP) to UDP 623 of every address on a network, which AMT and BMCs answer
//! without a login, AMT even with its TCP 623 turned off. No answer means we didn't find one, not that there's none.

use std::collections::{BTreeSet, HashSet};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use ts_rs::TS;

use crate::{Error, Flavor, Result};

pub const RMCP_PORT: u16 = 623;

/// RMCP version 6, its sequence number and class ASF, then ASF's enterprise number (4542), a presence ping (0x80) and
/// its tag, 0.
const PING: [u8; 12] = [0x06, 0, 0, 0x06, 0, 0, 0x11, 0xBE, 0x80, 0, 0, 0];

/// What a device says about itself before we log in: neither its name nor anything else that would need one. A BMC
/// says no version.
#[derive(Debug, PartialEq, Serialize, TS)]
#[ts(export, export_to = "core.ts")]
pub struct Found {
    pub host: String,
    pub flavor: Flavor,
    pub version: String,
    /// Whether it was set up to be managed, rather than not yet or not completely: only then can we log in.
    pub provisioned: bool,
}

/// The network we look on unless told otherwise: our own /24, empty when we have no IPv4 address.
pub fn nearby() -> String {
    // Connecting a UDP socket sends nothing, it just picks the address we'd reach the world from.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0));
    let ours = socket.and_then(|socket| socket.connect("192.0.2.1:9").and_then(|()| socket.local_addr()));
    match ours.map(|address| address.ip()) {
        Ok(std::net::IpAddr::V4(ours)) => format!("{}/24", Ipv4Addr::from(u32::from(ours) & !0xFF)),
        _ => String::new(),
    }
}

/// Pings every address in `networks` (see `addresses`) on `port` and says each device on `found` as it answers. We
/// pace the pings, a burst would overflow the buffers on the way and lose answers, and ping the silent ones once more.
pub async fn discover(networks: &str, port: u16, mut found: impl FnMut(Found)) -> Result<()> {
    let addresses = addresses(networks)?;
    let socket = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let answered = Mutex::new(HashSet::new());

    let ping = async {
        // As MeshCommander does, then as ipmitool does, sequence number 255: OpenBMC drops any other.
        for sequence in [0, 0xFF] {
            let mut ping = PING;
            ping[2] = sequence;
            let mut pace = tokio::time::interval(Duration::from_millis(2));
            for address in addresses.iter().filter(|address| !answered.lock().unwrap().contains(*address)) {
                pace.tick().await;
                // Some addresses can't be sent to (no route, a broadcast), that's no reason to stop.
                let _ = socket.send_to(&ping, (*address, port)).await;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    let listen = async {
        let mut datagram = [0; 64];
        loop {
            let (length, from) = super::ipmi::receive(&socket, &mut datagram).await?;
            let SocketAddr::V4(from) = from else { continue };
            if let Some(device) = pong(&datagram[..length], from.ip().to_string())
                && answered.lock().unwrap().insert(*from.ip())
            {
                found(device);
            }
        }
    };
    tokio::select! {
        () = ping => Ok(()),
        error = listen => error,
    }
}

/// Every address in `networks`, in order: addresses, ranges (`192.168.88.10-192.168.88.50`) and networks
/// (`192.168.88.0/22`, without its network and broadcast addresses), separated by commas or spaces. A /16 at most, at
/// our pace that's minutes already.
fn addresses(networks: &str) -> Result<BTreeSet<Ipv4Addr>> {
    let mut addresses = BTreeSet::new();
    for network in networks.split([',', ' ']).filter(|network| !network.is_empty()) {
        let invalid = || Error::Invalid(format!("{network} isn't an IPv4 address, range or network."));
        let address = |text: &str| text.trim().parse::<Ipv4Addr>().map(u32::from).map_err(|_| invalid());
        let (first, last) = match network.split_once(['-', '/']) {
            Some((first, last)) if network.contains('-') => (address(first)?, address(last)?),
            Some((base, prefix)) => {
                let prefix: u32 = prefix.parse().ok().filter(|prefix| *prefix <= 32).ok_or_else(invalid)?;
                let host = u32::MAX.checked_shr(prefix).unwrap_or(0);
                let base = address(base)? & !host;
                if prefix < 31 { (base + 1, (base | host) - 1) } else { (base, base | host) }
            }
            None => (address(network)?, address(network)?),
        };
        if first > last {
            return Err(invalid());
        }
        if addresses.len() + (last - first) as usize >= 1 << 16 {
            return Err(Error::Invalid("That's too many addresses, a /16 at most.".into()));
        }
        addresses.extend((first..=last).map(Ipv4Addr::from));
    }
    Ok(addresses)
}

/// The device at `host` from its presence pong: Intel's say their AMT version and whether it's provisioned, a server's
/// BMC only that it speaks IPMI, naming no vendor: the login tells whether it's a Supermicro. The acknowledgement AMT
/// sends before is too short to be a pong.
fn pong(datagram: &[u8], host: String) -> Option<Found> {
    let pong: &[u8; 24] = datagram.get(..24)?.try_into().ok()?;
    if pong[3] != 0x06 || pong[8] != 0x40 || pong[9] != PING[9] {
        return None;
    }
    if pong[12..16] != 343u32.to_be_bytes() {
        let ipmi = pong[20] & 0x80 != 0;
        return ipmi.then(|| Found { host, flavor: Flavor::Ipmi, version: String::new(), provisioned: true });
    }
    // Newer AMT leaves the packed version zero, 19 doesn't fit in a nibble.
    let (major, minor) = match pong[18] {
        0 => (pong[22], pong[23]),
        packed => (packed >> 4, packed & 0x0F),
    };
    Some(Found { host, flavor: Flavor::Amt, version: format!("{major}.{minor}"), provisioned: pong[19] & 0x03 == 2 })
}
