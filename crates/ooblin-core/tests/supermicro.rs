//! A Supermicro BMC against our mock, replaying the X10 we recorded: Redfish, its web pages, the chassis commands, and
//! the settings it changes over Redfish and IPMI alike (IPMI itself in `tests/ipmi.rs`).

use std::sync::Arc;

use anyhow::{Result, bail};
use mock::{Amt, Bmc, PASSWORD};
use ooblin_core::ipmi::{Network, PowerRestore};
use ooblin_core::supermicro::{Change, Client, Ntp, Service, Syslog, probe};
use ooblin_core::{Configuration, Error, Flavor, PowerAction, PowerState, SensorKind, Severity, Target, fingerprint};

async fn x10() -> Arc<Bmc> {
    Bmc::start("127.0.0.1:0", &mock::fixture("x10")).await
}

fn client(bmc: &Bmc, password: &str, pin: Option<String>) -> Result<Client> {
    Ok(Client::new(&bmc.target(password), pin)?)
}

/// The requests that GET each of `paths`, space-separated and under `/redfish/v1/`, in order.
fn gets(paths: &str) -> Vec<String> {
    paths.split_whitespace().map(|path| format!("GET /redfish/v1/{path}")).collect()
}

#[tokio::test]
async fn pins_the_certificate_on_first_use_and_refuses_another() -> Result<()> {
    let bmc = x10().await;
    let pin = fingerprint(&Amt::certificate());

    assert_eq!(client(&bmc, PASSWORD, None)?.info().await?.fingerprint, pin);
    assert_eq!(client(&bmc, PASSWORD, Some(pin.clone()))?.info().await?.fingerprint, pin);

    let old = fingerprint(b"the certificate before the BMC was reflashed");
    match client(&bmc, PASSWORD, Some(old.clone()))?.info().await {
        Err(Error::PinMismatch { expected, actual }) => assert_eq!((expected, actual), (old, pin)),
        other => bail!("expected a pin mismatch, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn logs_in_with_basic_up_front_and_never_again_once_refused() -> Result<()> {
    let bmc = x10().await;
    client(&bmc, PASSWORD, None)?.power().await?;
    assert_eq!(bmc.asked(), gets("Systems/1"), "one request a resource, with the password");

    // The X10 locks the account after five, which our polls would get to in a minute.
    let mut wrong = client(&bmc, "wrong", None)?;
    assert!(matches!(wrong.info().await, Err(Error::Unauthorized)));
    assert!(matches!(wrong.power().await, Err(Error::Unauthorized)));
    assert!(matches!(wrong.set_power(PowerAction::Reset).await, Err(Error::Unauthorized)));
    assert_eq!(bmc.asked(), gets("Managers/1"));

    assert!(matches!(client(&bmc, "wrong", None)?.hardware().await, Err(Error::Unauthorized)));
    assert_eq!(bmc.asked(), gets("Systems/1"));
    Ok(())
}

#[tokio::test]
async fn probes_without_a_password() -> Result<()> {
    let bmc = x10().await;
    assert!(probe(&format!("127.0.0.1:{}", bmc.port)).await);
    assert_eq!(bmc.asked(), ["GET /redfish/v1/ (no login)"]);

    let not_redfish = Bmc::start("127.0.0.1:0", &mock::fixture("ms-02")).await;
    assert!(!probe(&format!("127.0.0.1:{}", not_redfish.port)).await, "a web server without a Redfish root");
    // Port 1, which no test of ours is handed, as it could be one we bound and let go of.
    assert!(!probe("127.0.0.1:1").await, "nothing listening");
    Ok(())
}

/// Not told what a host is, we give the password to a Supermicro's Redfish only: a web server that isn't a BMC gets
/// none, IPMI's login never sends it, and one a Supermicro refuses isn't tried again over IPMI.
#[tokio::test]
async fn a_web_server_that_isnt_a_bmc_never_gets_the_password() -> Result<()> {
    let web = Bmc::start("127.0.0.1:0", &mock::fixture("ms-02")).await;
    assert!(ooblin_core::Client::login(&web.target(PASSWORD), None, None).await.is_err());
    let asked = web.asked();
    assert_eq!(asked[0], "GET /redfish/v1/ (no login)");
    assert!(asked[1..].iter().all(|request| request.starts_with("IPMI ")), "{asked:#?}");

    let bmc = x10().await;
    let refused = ooblin_core::Client::login(&bmc.target("wrong"), None, None).await;
    assert!(matches!(refused, Err(Error::Unauthorized)));
    assert_eq!(bmc.asked(), ["GET /redfish/v1/ (no login)", "GET /redfish/v1/Managers/1"]);
    Ok(())
}

/// Another vendor's Redfish, which has no manager `1` (OpenBMC's is `bmc`), isn't a Supermicro's: we know it over
/// IPMI.
#[tokio::test]
async fn another_vendors_redfish_is_a_bmc_over_ipmi() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("redfish/v1"))?;
    std::fs::write(dir.path().join("redfish/v1/index.json"), r#"{"RedfishVersion": "1.17.0"}"#)?;
    std::fs::copy(mock::fixture("openbmc").join("ipmi.txt"), dir.path().join("ipmi.txt"))?;
    let bmc = Bmc::start("127.0.0.1:0", dir.path()).await;

    let (_, info) = ooblin_core::Client::login(&bmc.target(PASSWORD), None, None).await?;
    assert_eq!(info.flavor, Flavor::Ipmi);
    assert_eq!(bmc.asked()[..2], ["GET /redfish/v1/ (no login)", "GET /redfish/v1/Managers/1"]);
    Ok(())
}

/// A BMC with IPMI turned off, here one that answers none of what we ask over it: we go without, and don't wait for
/// its firmware's version again.
#[tokio::test]
async fn goes_without_ipmi() -> Result<()> {
    let nothing = tempfile::tempdir()?;
    std::fs::write(nothing.path().join("ipmi.txt"), "")?;
    let (bmc, silent) = (x10().await, Bmc::start("127.0.0.1:0", nothing.path()).await);
    let mut client = Client::new(&Target { ipmi_port: silent.ipmi_port, ..bmc.target(PASSWORD) }, None)?;

    assert_eq!(client.info().await?.version, "4.7", "as Redfish says it");
    assert!(!silent.asked().is_empty());
    client.info().await?;
    assert!(silent.asked().is_empty(), "asked once");

    let panel = client.panel().await?;
    assert_eq!((panel.identify, panel.post_code), (None, Some(0)));
    Ok(())
}

/// Pages that refuse a login Redfish takes (firmware that wants it another way, say) cost us what only they say, not
/// Redfish, and aren't tried again.
#[tokio::test]
async fn goes_without_its_web_pages() -> Result<()> {
    let bmc = x10().await;
    bmc.refuse_web();
    let mut client = client(&bmc, PASSWORD, None)?;

    let status = client.status().await?;
    assert_eq!((&*status.firmware_built, &*status.bios, status.lans.len()), ("", "2.6a", 2));
    assert_eq!(client.power().await?.state, PowerState::On);
    bmc.asked();

    let services = Change::Services(vec![Service { name: "SSH".into(), enabled: false, port: None }]);
    assert!(matches!(client.change(services).await, Err(Error::Unauthorized)));
    assert!(bmc.asked().is_empty(), "the login isn't tried again");
    Ok(())
}

/// As the X10's own web pages show it.
#[tokio::test]
async fn reads_what_the_x10_says() -> Result<()> {
    let bmc = x10().await;
    let mut client = client(&bmc, PASSWORD, None)?;

    // Its version as IPMI says it, as an IPMI login does.
    let info = client.info().await?;
    let redfish: Vec<_> = bmc.asked().into_iter().filter(|request| request.contains("/redfish/")).collect();
    assert_eq!(redfish, gets("Managers/1 Managers/1/EthernetInterfaces/1 Managers/1/NetworkProtocol"));
    let access = &info.remote_access;
    assert_eq!((&*info.version, &*info.name, access.kvm, access.ider), ("4.07", "", Some(true), true));

    let power = client.power().await?;
    assert_eq!(bmc.asked(), gets("Systems/1"));
    assert_eq!(power.state, PowerState::On);
    for action in [PowerAction::Reset, PowerAction::ResetToBios, PowerAction::ResetToImage, PowerAction::Nmi] {
        assert!(power.actions.contains(&action), "{action:?}");
    }
    assert!(!power.actions.contains(&PowerAction::On) && !power.actions.contains(&PowerAction::Cycle));

    let hardware = client.hardware().await?;
    let paths = "Chassis/1 Systems/1 Systems/1/Memory Systems/1/Memory/1 Systems/1/Memory/2 Systems/1/Memory/3 \
        Systems/1/Memory/4 Systems/1/Processors Systems/1/Processors/1 Systems/1/SimpleStorage";
    let mut asked = bmc.asked();
    assert_eq!(asked[0], gets("Systems/1")[0], "the system alone first, so a wrong password is tried once");
    asked.sort();
    assert_eq!(asked, gets(paths), "one request a member, the X10's $expand is broken");
    assert_eq!(hardware.bios.version, "2.6a");
    assert_eq!(hardware.processors[0].model, "Intel(R) Xeon(R) CPU D-1541 @ 2.10GHz");
    assert_eq!(hardware.memory.len(), 4);
    for module in &hardware.memory {
        assert_eq!((&*module.kind, module.mhz, module.bytes), ("DDR4", Some(2400), Some(16 << 30)));
    }
    assert!(hardware.disks.is_empty() && hardware.system.serial_number.is_empty());

    let events = client.event_log().await?;
    assert_eq!(bmc.asked(), gets("Systems/1/LogServices/Log1/Entries"));
    assert!(events.len() == 31 && events.is_sorted_by(|newer, older| newer.time >= older.time));
    let hot: Vec<_> =
        events.iter().filter(|event| event.description == "[ CPU Temp ] Upper Critical going high").collect();
    assert_eq!(
        (hot[0].severity, hot[1].severity),
        (Severity::Warning, Severity::Critical),
        "the deassert, then the assert, as the IPMI flavor rates them"
    );
    assert_eq!(hot[0].source, "Temperature");
    let oem = events.iter().find(|event| event.source == "ACPowerOn").map(|event| event.severity);
    assert_eq!(oem, Some(Severity::Info), "an OEM event we don't know");

    // Over IPMI, as `tests/ipmi.rs` reads them.
    let sensors = client.sensors().await?;
    let count = |kind| sensors.iter().filter(|sensor| sensor.kind == kind).count();
    assert_eq!((count(SensorKind::Temperature), count(SensorKind::Voltage), count(SensorKind::Fan)), (7, 8, 4));
    let fan3 = sensors.iter().find(|fan| fan.name == "FAN3");
    assert_eq!(fan3.map(|fan| fan.reading), Some(None), "absent");
    bmc.asked();

    let status = client.status().await?;
    let [port] = &*status.ports else { bail!("{} ports", status.ports.len()) };
    assert_eq!((port.ipv4.configuration, port.mbps, port.link_up), (Configuration::Dhcp, Some(1000), true));
    assert_eq!(port.ipv6.as_ref().map(|ipv6| &*ipv6.gateway), Some(""), "the X10 names itself its gateway");
    let lans: Vec<_> = status.lans.iter().map(|lan| (&*lan.name, &*lan.mac)).collect();
    assert_eq!(lans, [("OnBoard LAN 1", "5C:A1:1F:A0:CB:E2"), ("OnBoard LAN 2", "5C:A1:1F:A0:CB:E3")]);
    // What only the BMC's own pages say, in a web session of its own that's logged out again.
    assert_eq!((&*status.info.version, &*status.firmware_built), ("4.07", "2026-05-29"));
    assert_eq!((&*status.bios, &*status.bios_released, &*status.redfish), ("2.6a", "2025-10-01", "1.0.1"));
    let web: Vec<_> = bmc
        .asked()
        .into_iter()
        .filter(|request| {
            !request.contains("/redfish/") && !request.starts_with("IPMI") && !request.ends_with("ipmi.cgi")
        })
        .collect();
    let dashboard = "GET /cgi/url_redirect.cgi?url_name=sys_info";
    let reads = ["CGI GENERIC_INFO.XML", "CGI SMBIOS_INFO.XML"];
    assert_eq!(web, [&["POST /cgi/login.cgi", dashboard][..], &reads, &["GET /cgi/logout.cgi"]].concat());

    let settings = client.bmc_settings().await?;
    // Asked all at once, in no particular order.
    let mut redfish: Vec<_> = bmc.asked().into_iter().filter(|request| request.contains("/redfish/")).collect();
    redfish.sort();
    assert_eq!(
        redfish,
        gets(
            "Managers/1 Managers/1/EthernetInterfaces/1 Managers/1/FanMode Managers/1/MouseMode Managers/1/NTP Managers/1/NetworkProtocol Managers/1/Syslog"
        )
    );
    let port = |name| settings.services.iter().find(|service| service.name == name).and_then(|service| service.port);
    assert_eq!((port("HTTPS"), port("KVMIP")), (Some(443), Some(5900)));
    assert_eq!((&*settings.fan_mode.mode, &*settings.mouse_mode.mode), ("FullSpeed", "Absolute"));
    assert_eq!(settings.fan_mode.modes, ["Standard", "FullSpeed", "Optimal", "HeavyIO"]);
    assert_eq!((settings.ntp.enabled, &*settings.ntp.primary), (true, "0.pool.ntp.org"));
    assert_eq!(
        (settings.power_restore, settings.sol.map(|sol| sol.bit_rate)),
        (Some(PowerRestore::Previous), Some(Some(115_200)))
    );
    Ok(())
}

/// What its Redfish says of its sensors, in its thermal and power resources as services did before sensors were
/// resources of their own. We read the X10's over IPMI, which is faster; another vendor's BMC may only have these.
#[tokio::test]
async fn says_its_sensors_over_redfish_too() -> Result<()> {
    let bmc = x10().await;
    let client = client(&bmc, PASSWORD, None)?;
    let sensors = client.redfish().sensors().await?;
    let mut asked = bmc.asked();
    asked.sort();
    assert_eq!(asked, gets("Chassis/1 Chassis/1/Power Chassis/1/Thermal"));

    let sensor = |name: &str| sensors.iter().find(|sensor| sensor.name == name).unwrap();
    let (cpu, fan, volts) = (sensor("CPU Temp"), sensor("FAN1"), sensor("VCCP"));
    assert_eq!((cpu.kind, cpu.reading, &*cpu.unit, cpu.high), (SensorKind::Temperature, Some(67.0), "°C", Some(96.0)));
    assert_eq!((fan.kind, fan.reading, &*fan.unit, fan.low), (SensorKind::Fan, Some(700.0), "RPM", Some(100.0)));
    assert_eq!((volts.kind, volts.reading, &*volts.unit), (SensorKind::Voltage, Some(1.84), "V"));
    assert_eq!(volts.severity, Severity::Info);
    Ok(())
}

#[tokio::test]
async fn changes_the_power() -> Result<()> {
    let bmc = x10().await;
    let mut client = client(&bmc, PASSWORD, None)?;
    let reset = "POST /redfish/v1/Systems/1/Actions/ComputerSystem.Reset";

    let boot = r#"PATCH /redfish/v1/Systems/1 {"Boot":{"BootSourceOverrideEnabled":"Once","BootSourceOverrideTarget":"#;

    use PowerAction::*;
    let actions = [
        (On, "On", None),
        (OnToBios, "On", Some("BiosSetup")),
        (OnToPxe, "On", Some("Pxe")),
        (OnToImage, "On", Some("UsbCd")),
        (SoftOff, "GracefulShutdown", None),
        (Off, "ForceOff", None),
        (SoftReset, "GracefulRestart", None),
        (Reset, "ForceRestart", None),
        (ResetToBios, "ForceRestart", Some("BiosSetup")),
        (ResetToPxe, "ForceRestart", Some("Pxe")),
        (ResetToImage, "ForceRestart", Some("UsbCd")),
        (Nmi, "Nmi", None),
    ];
    for (action, kind, target) in actions {
        client.set_power(action).await?;
        let override_first = target.map(|target| format!(r#"{boot}"{target}"}}}}"#));
        let expected = override_first.into_iter().chain([format!(r#"{reset} {{"ResetType":"{kind}"}}"#)]);
        assert_eq!(bmc.asked(), expected.collect::<Vec<_>>(), "{action:?}");
    }

    for action in [Sleep, Hibernate, OnToSerial, ResetToSerial] {
        assert!(matches!(client.set_power(action).await, Err(Error::Unsupported)), "{action:?}");
    }
    assert!(bmc.asked().is_empty());
    // A power cycle is Redfish's to do, where a BMC's Reset allows it: the X10's doesn't.
    assert!(!client.power().await?.actions.contains(&Cycle));
    Ok(())
}

/// The identify light over IPMI, the POST code from Redfish.
#[tokio::test]
async fn lights_the_identify_light() -> Result<()> {
    let bmc = x10().await;
    let mut client = Client::new(&bmc.target(PASSWORD), None)?;
    let panel = client.panel().await?;
    assert_eq!((panel.identify, panel.post_code), (Some(false), Some(0)));
    client.identify(true).await?;
    assert_eq!(client.panel().await?.identify, Some(true));
    client.identify(false).await?;
    assert_eq!(client.panel().await?.identify, Some(false));
    Ok(())
}

/// Redfish's own settings over Redfish, the server's power restore policy and serial console over IPMI, each read back
/// as the BMC took it. What it doesn't take is an error.
#[tokio::test]
async fn changes_the_settings() -> Result<()> {
    let bmc = x10().await;
    let mut client = Client::new(&bmc.target(PASSWORD), None)?;
    client.change(Change::FanMode("Optimal".into())).await?;
    client.change(Change::MouseMode("Relative".into())).await?;
    let ntp = Ntp { enabled: true, primary: "time.example".into(), secondary: String::new() };
    client.change(Change::Ntp(ntp)).await?;
    client.change(Change::Syslog(Syslog { enabled: true, server: "192.0.2.9".into(), port: 514 })).await?;
    client.change(Change::PowerRestore(PowerRestore::PowerOn)).await?;
    client.change(Change::SolBitRate(57_600)).await?;

    let settings = client.bmc_settings().await?;
    assert_eq!((&*settings.fan_mode.mode, &*settings.mouse_mode.mode), ("Optimal", "Relative"));
    assert_eq!((&*settings.ntp.primary, &*settings.ntp.secondary), ("time.example", ""));
    assert_eq!((settings.syslog.enabled, &*settings.syslog.server, settings.syslog.port), (true, "192.0.2.9", 514));
    assert_eq!(settings.power_restore, Some(PowerRestore::PowerOn));
    assert_eq!(settings.sol.map(|sol| sol.bit_rate), Some(Some(57_600)));

    assert!(matches!(client.change(Change::FanMode("Turbo".into())).await, Err(Error::Http(400))));
    assert!(matches!(client.change(Change::SolBitRate(4800)).await, Err(Error::Invalid(_))));
    Ok(())
}

/// Services change as the BMC's own port page changes them: all at once, through its CGI, read back from Redfish. We
/// leave HTTPS, which we speak, as it is; the KVM off takes virtual media along; two can't share a port.
#[tokio::test]
async fn changes_the_services() -> Result<()> {
    let bmc = x10().await;
    let mut client = client(&bmc, PASSWORD, None)?;
    let service = |name: &str, enabled, port| Service { name: name.into(), enabled, port: Some(port) };
    let services = vec![service("SSH", false, 22), service("SNMP", true, 1161), service("HTTPS", false, 8443)];
    client.change(Change::Services(services)).await?;
    let changed = bmc.asked().into_iter().find(|request| request.starts_with("CGI op=config_port")).unwrap();
    assert!(changed.contains("&SSH_SERVICE=0&") && changed.contains("&SNMP_PORT=1161&"), "{changed}");
    assert!(changed.contains("HTTPS_PORT=443&HTTPS_SERVICE=1"), "{changed}");
    assert!(changed.contains("&WSMAN_PORT=5985&") && changed.contains("&SSL_REDIRECT=1&"), "the rest as they were");

    let settings = client.bmc_settings().await?;
    let state = |name| settings.services.iter().find(|service| service.name == name).map(|s| (s.enabled, s.port));
    assert_eq!(
        (state("SSH"), state("SNMP"), state("HTTPS")),
        (Some((false, Some(22))), Some((true, Some(1161))), Some((true, Some(443))))
    );

    let access = settings.remote_access;
    assert_eq!((access.kvm, access.sol, access.ider), (Some(true), true, true));

    client.change(Change::Services(vec![service("KVMIP", false, 5900)])).await?;
    let changed = bmc.asked().into_iter().find(|request| request.starts_with("CGI op=config_port")).unwrap();
    assert!(changed.contains("IKVM_SERVICE=0") && changed.contains("VM_SERVICE=0"), "{changed}");
    // What the UI then closes and no longer offers.
    let access = client.bmc_settings().await?.remote_access;
    assert_eq!((access.kvm, access.sol, access.ider), (Some(false), true, false));

    let taken = client.change(Change::Services(vec![service("SSH", true, 443)])).await;
    assert!(matches!(taken, Err(Error::Invalid(message)) if message == "Two services can't share a port."));

    let taken = client.change(Change::Services(vec![service("SSH", true, 5120)])).await;
    assert!(matches!(taken, Err(Error::Invalid(message)) if message == "The BMC says the SSH port is in use."));
    assert_eq!(bmc.asked().last().map(String::as_str), Some("GET /cgi/logout.cgi"));
    Ok(())
}

/// The FRU over IPMI, as the IPMI flavor changes it (see `tests/ipmi.rs`).
#[tokio::test]
async fn changes_the_fru() -> Result<()> {
    let bmc = x10().await;
    let mut client = client(&bmc, PASSWORD, None)?;
    let mut fru = client.bmc_settings().await?.fru.unwrap();
    fru.product.as_mut().unwrap().asset_tag = "rack-2".into();
    client.change(Change::Fru(fru.clone())).await?;
    assert_eq!(client.bmc_settings().await?.fru, Some(fru));
    Ok(())
}

/// The network as Redfish reads it, changed as the BMC's own network page changes it: all at once, what we don't show
/// (IPv6, its RMCP port, its LAN and link) as it was.
#[tokio::test]
async fn changes_the_network() -> Result<()> {
    let bmc = x10().await;
    let mut client = client(&bmc, PASSWORD, None)?;
    let network = client.bmc_settings().await?.network;
    assert_eq!(
        (network.dhcp, network.hostname.as_deref(), network.dns.as_deref()),
        (true, Some(""), Some("192.0.2.241"))
    );

    let network =
        Network { hostname: Some("x10".into()), dhcp: false, address: "192.0.2.10".into(), vlan: Some(42), ..network };
    client.change(Change::Network(network.clone())).await?;
    let changed = bmc.asked().into_iter().find(|request| request.starts_with("CGI op=config_lan")).unwrap();
    for field in ["bmcip=192.0.2.10", "en_dhcp=off", "en_vlan=on", "vlanID=42", "rmcpport=623", "hostname=x10"] {
        assert!(changed.contains(field), "{field} in {changed}");
    }
    for kept in ["bmcipv6_autoconf=on", "dhcpv6_mode=stateless", "lan_interface=2", "link_conf=0", "bmcipv6_addr=&"] {
        assert!(changed.contains(kept), "{kept} in {changed}");
    }
    assert_eq!(client.bmc_settings().await?.network, network);

    // What we'd write back as it was, on pages that don't say it (another firmware's): nothing changes.
    let config = std::fs::read_to_string(mock::fixture("x10").join("cgi/CONFIG_INFO.XML"))?;
    for (said, unsaid) in [(r#" INTERFACE="2""#, ""), (r#"RMCP_PORT="026f""#, r#"RMCP_PORT="""#)] {
        assert!(config.contains(said));
        bmc.set_cgi("CONFIG_INFO.XML", &config.replace(said, unsaid));
        assert!(matches!(client.change(Change::Network(network.clone())).await, Err(Error::Protocol(_))), "{said}");
        let asked = bmc.asked();
        assert!(!asked.iter().any(|request| request.starts_with("CGI op=")), "{asked:#?}");
        assert_eq!(asked.last().map(String::as_str), Some("GET /cgi/logout.cgi"));
    }
    Ok(())
}
