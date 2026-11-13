//! IPMI against the X10 mock, which replays what the X10 said over IPMI (`fixtures/x10/ipmi.txt`): discovery, RAKP
//! logins, the serial console, and the IPMI flavor, a BMC we know over IPMI alone.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use mock::{Bmc, PASSWORD, USER};
use ooblin_core::ipmi::{self, Change, Client, Network, PowerRestore};
use ooblin_core::{
    Configuration, Error, Flavor, Found, PowerAction, PowerState, SensorKind, Severity, Target, discover,
};

async fn x10() -> Arc<Bmc> {
    Bmc::start("127.0.0.1:0", &mock::fixture("x10")).await
}

/// The mock at its web port and its IPMI port.
fn target(bmc: &Bmc, user: &str, password: &str) -> Target {
    Target { ipmi_port: bmc.ipmi_port, ..Target::new(format!("127.0.0.1:{}", bmc.port), user, password) }
}

/// What the mock was asked since the last call over IPMI, oldest first.
fn asked(bmc: &Bmc) -> Vec<String> {
    let requests = std::mem::take(&mut *bmc.requests.lock().unwrap());
    requests.into_iter().filter(|request| request.starts_with("IPMI") || request.starts_with("SOL")).collect()
}

/// The X10's BMC answers a presence ping with its pong alone, which names no vendor and no version but says it speaks
/// IPMI.
#[tokio::test]
async fn finds_the_x10_by_its_presence_pong() -> Result<()> {
    let bmc = x10().await;
    let mut found = Vec::new();
    discover("127.0.0.1", bmc.ipmi_port, |device| found.push(device)).await?;
    let x10 = Found { host: "127.0.0.1".into(), flavor: Flavor::Ipmi, version: String::new(), provisioned: true };
    assert_eq!(found, [x10]);
    Ok(())
}

/// Each login is a session of its own, closed again: a BMC has only a few. A wrong user or password is refused, as is a
/// session sealed with keys from the wrong one.
#[tokio::test]
async fn logs_in_with_rakp_and_closes_the_session() -> Result<()> {
    let bmc = x10().await;
    let chassis = ipmi::with(&target(&bmc, USER, PASSWORD), ipmi::chassis).await?;
    assert_eq!((chassis.identify, chassis.restore), (Some(false), Some(PowerRestore::Previous)));
    assert_eq!(asked(&bmc), ["IPMI 00 01 []", "IPMI 06 3c [00, 01, 00, 00]"]);

    for (user, password) in [("root", PASSWORD), (USER, "wrong")] {
        let refused = ipmi::with(&target(&bmc, user, password), ipmi::chassis).await;
        assert!(matches!(refused, Err(Error::Unauthorized)), "{user}: {refused:?}");
    }
    assert_eq!(asked(&bmc), Vec::<String>::new());
    Ok(())
}

/// The serial console: what the server writes comes, what we type goes and is acknowledged, and closing deactivates
/// SOL before the session ends.
#[tokio::test]
async fn opens_the_serial_console() -> Result<()> {
    let bmc = x10().await;
    let sol = ipmi::Sol::open(&target(&bmc, USER, PASSWORD)).await?;
    let read = async |until: &str| -> Result<String> {
        let mut text = String::new();
        for _ in 0..5 {
            text += &String::from_utf8_lossy(&sol.read().await?);
            if text.contains(until) {
                return Ok(text);
            }
        }
        bail!("no {until:?} in {text:?}")
    };
    read("mock login: ").await?;
    sol.write(b"root\r").await?;
    assert_eq!(read("\r\n").await?, "root\r\n");
    // Typing waits for the BMC to take what's typed before, which `read` hears.
    tokio::time::timeout(Duration::from_secs(5), async {
        let typing = sol.write(b"ls\r");
        let reading = read("ls\r\n");
        let (typed, echoed) = tokio::join!(typing, reading);
        typed.and(echoed.map(drop).map_err(|error| Error::Protocol(error.to_string())))
    })
    .await??;
    sol.close().await;
    let asked = asked(&bmc);
    assert_eq!(asked[0], "IPMI 06 48 [01, 01, c4, 00, 00, 00]");
    assert_eq!(asked[1..3], [r#"SOL "root\r""#, r#"SOL "ls\r""#]);
    assert_eq!(asked[3..], ["IPMI 06 49 [01, 01, 00, 00, 00, 00]", "IPMI 06 3c [00, 01, 00, 00]"]);
    Ok(())
}

/// Logging in as IPMI says what Get Device ID does: the X10's firmware, and no certificate.
#[tokio::test]
async fn logs_in_as_ipmi() -> Result<()> {
    let bmc = x10().await;
    let (client, info) = ooblin_core::Client::login(&target(&bmc, USER, PASSWORD), None, Some(Flavor::Ipmi)).await?;
    assert!(matches!(client, ooblin_core::Client::Ipmi(_)));
    assert_eq!((info.flavor, &*info.version, &*info.fingerprint), (Flavor::Ipmi, "4.07", ""));
    assert_eq!((info.remote_access.kvm, info.remote_access.sol), (None, true));
    Ok(())
}

/// What IPMI says of the X10: Supermicro's board 0x086D, IPMI 2.0, its port as its LAN parameters say, its sensors as
/// the SDR and readings do, and its event log as Redfish words it too (`fixtures/x10/redfish`).
#[tokio::test]
async fn reads_what_the_x10_says_over_ipmi() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&target(&bmc, USER, PASSWORD));
    let status = client.status().await?;
    assert_eq!((&*status.manufacturer, status.product_id, &*status.ipmi_version), ("Supermicro", 0x086D, "2.0"));
    assert_eq!(status.power, PowerState::On);
    let [port] = &*status.ports else { bail!("{} ports", status.ports.len()) };
    assert_eq!((&*port.mac, port.ipv4.configuration), ("5C:A1:1F:A0:CB:D5", Configuration::Dhcp));
    assert_eq!((&*port.ipv4.address, &*port.ipv4.gateway), ("192.0.2.224", "192.0.2.241"));
    // The X10's SLAAC address, scrubbed to the documentation prefix.
    assert_eq!(
        port.ipv6.as_ref().map(|ipv6| ipv6.addresses.clone()),
        Some(vec!["2001:db8::5ea1:1fff:fea0:cbd5".into()])
    );

    let sensors = client.sensors().await?;
    assert_eq!(sensors.len(), 20);
    let cpu = sensors.iter().find(|sensor| sensor.name == "CPU Temp").unwrap();
    assert_eq!((cpu.kind, &*cpu.unit, cpu.high), (SensorKind::Temperature, "°C", Some(96.0)));
    // A sensor of states with none asserted.
    let intrusion = sensors.iter().find(|sensor| sensor.name == "Chassis Intru").unwrap();
    assert_eq!((intrusion.reading, &*intrusion.state), (None, "OK"));

    let events = client.event_log().await?;
    assert_eq!(events.len(), 31);
    let hot = events.iter().find(|event| event.description == "Upper Critical going high, 96 °C ≥ 96 °C");
    assert_eq!(hot.map(|event| (&*event.source, event.severity)), Some(("CPU Temp", Severity::Critical)));

    let settings = client.settings().await?;
    let admin = settings.users.iter().find(|user| user.name == "ADMIN").unwrap();
    assert_eq!((&*admin.privilege, admin.enabled), ("Administrator", Some(true)));
    assert_eq!(settings.power_restore, Some(PowerRestore::Previous));
    Ok(())
}

/// The FRU as the X10's own pages show it, and changed: its product area grows into the room after it, its board area
/// stays, and an area it has none of can't be set.
#[tokio::test]
async fn changes_the_fru() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&target(&bmc, USER, PASSWORD));
    let mut fru = client.fru().await?;
    let board = fru.board.clone().unwrap();
    assert_eq!((&*board.manufacturer, &*board.serial_number, fru.chassis.is_none()), ("Supermicro", "", true));

    let product = fru.product.as_mut().unwrap();
    (product.name, product.asset_tag) = ("Homelab".into(), "rack-2".into());
    client.change(Change::Fru(fru.clone())).await?;
    assert_eq!(client.fru().await?, fru);

    fru.chassis = Some(Default::default());
    match client.change(Change::Fru(fru)).await {
        Err(Error::Invalid(message)) => assert_eq!(message, "this FRU has no chassis area"),
        other => bail!("expected a refusal, got {other:?}"),
    }
    Ok(())
}

/// Booting once is a boot override, ipmitool's way, then the power change.
#[tokio::test]
async fn boots_once_into_pxe() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&target(&bmc, USER, PASSWORD));
    assert!(client.power().await?.actions.contains(&PowerAction::ResetToPxe));
    asked(&bmc);
    client.set_power(PowerAction::ResetToPxe).await?;
    let asked = asked(&bmc);
    let sent = ["IPMI 00 08 [05, 80, 04, 00, 00, 00]", "IPMI 00 02 [03]"];
    let at = |request| asked.iter().position(|asked| asked == request).unwrap();
    assert!(at(sent[0]) < at(sent[1]), "{asked:?}");
    Ok(())
}

/// The network as IPMI's LAN parameters say it, changed the same way: no host name or DNS, which IPMI has none of.
#[tokio::test]
async fn changes_the_network() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&target(&bmc, USER, PASSWORD));
    let network = client.settings().await?.network.unwrap();
    assert_eq!((network.dhcp, &*network.address, network.vlan, &network.hostname), (true, "192.0.2.224", None, &None));

    let mut network = Network { dhcp: false, address: "192.0.2.10".into(), vlan: Some(42), ..network };
    client.change(Change::Network(network.clone())).await?;
    assert_eq!(client.settings().await?.network, Some(network.clone()));

    network.vlan = Some(4095);
    assert!(matches!(client.change(Change::Network(network.clone())).await, Err(Error::Invalid(_))));
    (network.vlan, network.gateway) = (None, "the router".into());
    assert!(matches!(client.change(Change::Network(network)).await, Err(Error::Invalid(_))));
    Ok(())
}
