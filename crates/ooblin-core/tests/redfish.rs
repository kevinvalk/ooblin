use std::sync::Arc;

use anyhow::{Context, Result};
use mock::{Bmc, PASSWORD};
use ooblin_core::redfish::Client;
use ooblin_core::{Error, Flavor, PowerAction, PowerState, SensorKind, Severity};

// NOTE: We replay two BMCs that are not each other's. OpenBMC as we recorded it says what a Reset allows in a resource
// of its own and names its identify light the new way. DMTF's sample rack server is the standard's own example and has
// the hardware, sensors and log an emulated OpenBMC lacks.
async fn bmc(fixture: &str) -> Arc<Bmc> {
    Bmc::start("127.0.0.1:0", &mock::fixture(fixture)).await
}

/// Returns the requests that GET each of `paths`, which are space-separated and under `/redfish/v1/`, in order.
fn gets(paths: &str) -> Vec<String> {
    paths.split_whitespace().map(|path| format!("GET /redfish/v1/{path}")).collect()
}

#[tokio::test]
async fn finds_what_the_service_lists_and_tries_a_wrong_password_once() -> Result<()> {
    let bmc = bmc("openbmc").await;
    let client = Client::new(&bmc.target(PASSWORD), None)?;

    let info = client.info().await?;
    assert_eq!((info.flavor, info.remote_access.is_kvm, info.remote_access.is_sol), (Flavor::Redfish, None, false));
    let found = "Managers Systems Chassis Managers/bmc Managers/bmc/EthernetInterfaces";
    let mut asked = bmc.asked();
    asked[1..3].sort_by(|a, b| b.cmp(a));
    assert_eq!(asked, gets(&format!("{found} Managers/bmc/EthernetInterfaces/eth0")));

    // What we found once we never ask for again.
    client.power().await?;
    assert_eq!(bmc.asked(), gets("Systems/system Systems/system/ResetActionInfo"));

    let wrong = Client::new(&bmc.target("wrong"), None)?;
    assert!(matches!(wrong.info().await, Err(Error::Unauthorized)));
    assert!(matches!(wrong.power().await, Err(Error::Unauthorized)));
    assert_eq!(bmc.asked(), gets("Managers"));

    Ok(())
}

#[tokio::test]
async fn reads_and_changes_what_openbmc_says() -> Result<()> {
    let bmc = bmc("openbmc").await;
    let client = Client::new(&bmc.target(PASSWORD), None)?;

    let status = client.status().await?;
    assert_eq!((&*status.redfish, status.power), ("1.17.0", PowerState::Off));
    let port = &status.ports[0];
    assert!(port.is_link_up && port.mbps == Some(100) && port.ipv4.address.starts_with("192.0.2."), "{port:?}");

    // A gateway of all zeros is no gateway.
    assert_eq!(port.ipv6.as_ref().map(|ipv6| &*ipv6.gateway), Some(""));

    // We offer what a Reset allows, which its action's own resource says, of what a server that is off can do. A disk
    // image is nowhere to boot from and sleep is nothing Redfish knows.
    use PowerAction::*;
    assert_eq!(client.power().await?.actions, [On, OnToBios, OnToPxe]);
    assert!(matches!(client.set_power(Sleep).await, Err(Error::Unsupported)));
    bmc.asked();

    client.set_power(OnToPxe).await?;
    client.set_power(Cycle).await?;
    let boot = r#"PATCH /redfish/v1/Systems/system {"Boot":{"BootSourceOverrideEnabled":"Once","BootSourceOverrideTarget":"Pxe"}}"#;
    let reset = "POST /redfish/v1/Systems/system/Actions/ComputerSystem.Reset";
    let asked = [boot.to_owned(), format!(r#"{reset} {{"ResetType":"On"}}"#)];
    assert_eq!(bmc.asked(), [&asked[..], &[format!(r#"{reset} {{"ResetType":"PowerCycle"}}"#)]].concat());

    // We change the identify light by the name this server has for it.
    assert_eq!(client.panel().await?.is_identifying, Some(true));
    client.identify(false).await?;
    assert_eq!(client.panel().await?.is_identifying, Some(false));
    assert!(bmc.asked().contains(&r#"PATCH /redfish/v1/Systems/system {"LocationIndicatorActive":false}"#.to_owned()));

    // An emulated board has no server behind it to list.
    let hardware = client.hardware().await?;
    assert!(hardware.processors.is_empty() && hardware.memory.is_empty() && hardware.disks.is_empty());
    assert!(client.sensors().await?.is_empty() && client.event_log().await?.is_empty());

    Ok(())
}

#[tokio::test]
async fn reads_what_dmtfs_sample_server_says() -> Result<()> {
    let bmc = bmc("dmtf-rackmount1").await;
    let client = Client::new(&bmc.target(PASSWORD), None)?;

    let status = client.status().await?;
    assert_eq!((&*status.info.name, &*status.info.version), ("web483-bmc.dmtf.org", "1.45.455b66-rev4"));
    assert_eq!((&*status.bios, status.power, status.lans.len()), ("P79 v1.45 (12/06/2017)", PowerState::On, 4));
    assert_eq!(status.ports[0].mac, "23:11:8A:33:CF:EA");

    let hardware = client.hardware().await?;
    assert_eq!((&*hardware.system.model, &*hardware.system.serial_number), ("3500", "437XR1138R2"));
    assert_eq!((&*hardware.motherboard.product, &*hardware.motherboard.manufacturer), ("3500RX", "Contoso"));

    // Its FPGA is no processor of the server's, nor is its empty second socket, and an empty bay is no disk.
    let sockets: Vec<_> = hardware.processors.iter().map(|processor| &*processor.socket).collect();
    assert_eq!(sockets, ["CPU 1"]);
    assert_eq!(hardware.processors[0].max_mhz, Some(3700));

    let module = &hardware.memory[0];
    assert_eq!((&*module.slot, module.bytes, &*module.kind), ("DIMM Slot 1", Some(32 << 30), "DDR4"));
    let disks: Vec<_> = hardware.disks.iter().map(|disk| (&*disk.model, disk.bytes)).collect();
    assert_eq!(disks, [("3000GT8", Some(8_000_000_000_000)), ("3000GT7", Some(4_000_000_000_000))]);

    // Each sensor is a resource of its own, and of its 41 we show those that are there: its second power supply is not.
    let sensors = client.sensors().await?;
    assert_eq!(sensors.len(), 30);
    let sensor = |name: &str| sensors.iter().find(|sensor| sensor.name == name).context(name.to_owned());
    let cpu = sensor("CPU #1 Temperature")?;
    assert_eq!((cpu.kind, cpu.reading, &*cpu.unit, cpu.high), (SensorKind::Temperature, Some(37.0), "°C", Some(45.0)));
    let supply = sensor("Power Supply #1 Input Power")?;
    assert_eq!((supply.kind, supply.reading, &*supply.unit), (SensorKind::Power, Some(374.0), "W"));
    let fan = sensor("CPU #1 Fan Speed")?;
    assert_eq!((fan.kind, fan.reading, &*fan.unit, fan.low), (SensorKind::Other, Some(80.0), "%", Some(0.0)));

    // Its log names a next page, which a sample has none of. So the mock answers the first again, naming the same next
    // one, which we do not read twice.
    let events = client.event_log().await?;
    assert_eq!(events.len(), 4);
    assert!(events[0].occurred_at > events[3].occurred_at, "newest first");
    let event = &events[0];
    assert_eq!((&*event.source, &*event.description), ("Temperature", "Temperature threshold exceeded"));
    assert_eq!(event.severity, Severity::Critical);

    // Here the server itself says what a Reset allows, and its identify light goes by its older name.
    use PowerAction::*;
    assert_eq!(client.power().await?.actions, [SoftOff, Off, SoftReset, Reset, Nmi, ResetToBios, ResetToPxe]);
    assert_eq!(client.panel().await?.is_identifying, Some(false));
    client.identify(true).await?;
    assert_eq!(client.panel().await?.is_identifying, Some(true));
    assert!(bmc.asked().iter().any(|request| request.ends_with(r#"{"IndicatorLED":"Blinking"}"#)));

    Ok(())
}

/// A BMC with IPMI off is found by the Redfish that answered, after a Supermicro's and IPMI were not it.
#[tokio::test]
async fn is_what_a_login_finds_where_ipmi_is_off() -> Result<()> {
    let bmc = bmc("dmtf-rackmount1").await;
    let (mut client, info) = ooblin_core::Client::login(&bmc.target(PASSWORD), None, None).await?;
    assert_eq!(info.flavor, Flavor::Redfish);
    assert_eq!(client.bmc_status().await?.redfish, "1.15.0");
    assert!(matches!(client.kvm().await, Err(Error::Unsupported)));
    assert!(matches!(client.sol().await, Err(Error::Unsupported)));

    Ok(())
}
