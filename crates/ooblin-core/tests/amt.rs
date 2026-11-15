//! AMT against our mock, replaying Intel's samples and the devices we recorded.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result, bail};
use flate2::{Compress, Compression, FlushCompress};
use futures_util::future::try_join_all;
use mock::{Amt, Bmc, Direction, PASSWORD, PONG, Transcript, hex, received, sent};
use ooblin_core::amt::wsman::{Instance, parse_instance};
use ooblin_core::amt::{
    Alarm, Availability, Change, Client, DnsUpdate, General, Ipv6Configuration, Ipv6Settings, Recovery, Redirection,
    Session,
};
use ooblin_core::{
    Configuration, Consent, Error, Flavor, Found, Input, Ipv4, PowerAction, PowerState, RemoteAccess, Target, discover,
    fingerprint,
};
use tokio::net::UdpSocket;

async fn start(fixture: &Path) -> Arc<Amt> {
    Amt::start("127.0.0.1:0", "127.0.0.1:0", fixture).await
}

async fn samples() -> Arc<Amt> {
    start(&mock::fixture("go-wsman-messages")).await
}

fn client(amt: &Amt, password: &str, pin: Option<String>) -> Result<Client> {
    Ok(Client::new(&amt.target(password), pin)?)
}

/// What the mock was asked since the last call, by the input or instance in each request's body.
fn asked(amt: &Amt) -> Vec<Instance> {
    amt.asked().iter().filter_map(|request| parse_instance(request).ok()).collect()
}

#[tokio::test]
async fn pins_the_certificate_on_first_use_and_refuses_another() -> Result<()> {
    let amt = samples().await;
    let pin = fingerprint(&Amt::certificate());

    assert_eq!(client(&amt, PASSWORD, None)?.info().await?.fingerprint, pin);
    assert_eq!(client(&amt, PASSWORD, Some(pin.clone()))?.info().await?.fingerprint, pin);

    let old = fingerprint(b"the certificate before reprovisioning");
    match client(&amt, PASSWORD, Some(old.clone()))?.info().await {
        Err(Error::PinMismatch { expected, actual }) => assert_eq!((expected, actual), (old, pin)),
        other => bail!("expected a pin mismatch, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn logs_in_with_digest_once_per_nonce() -> Result<()> {
    let amt = samples().await;
    assert!(matches!(client(&amt, "wrong", None)?.info().await, Err(Error::Unauthorized)));

    let mut client = client(&amt, PASSWORD, None)?;
    client.info().await?;
    let challenges = amt.challenges.load(Ordering::SeqCst);
    amt.expire_nonce();
    client.info().await?;
    assert_eq!(amt.challenges.load(Ordering::SeqCst), challenges + 1, "a stale nonce costs one more challenge");
    Ok(())
}

#[tokio::test]
async fn reads_and_changes_the_power() -> Result<()> {
    let amt = samples().await;
    let mut client = client(&amt, PASSWORD, None)?;
    let power = client.power().await?;
    assert_eq!(power.state, PowerState::On);
    assert!(power.actions.contains(&PowerAction::Reset) && !power.actions.contains(&PowerAction::On));

    // Each action's number, CIM's: a hard off is 8, the soft one 12. Being on, the sample offers all of them but on.
    use PowerAction::*;
    let states = [
        (On, 2),
        (Sleep, 4),
        (Cycle, 5),
        (Hibernate, 7),
        (Off, 8),
        (Reset, 10),
        (Nmi, 11),
        (SoftOff, 12),
        (SoftReset, 14),
    ];
    asked(&amt);
    for (action, state) in states {
        assert_eq!(power.actions.contains(&action), action != On, "{action:?}");
        client.set_power(action).await?;
        let [change] = &*asked(&amt) else { bail!("{action:?} took more than a request") };
        let input = (&*change.class, change.parse("PowerState"));
        assert_eq!(input, ("RequestPowerStateChange_INPUT", Some(state)), "{action:?}");
    }

    assert!(power.actions.contains(&ResetToBios));
    client.set_power(ResetToBios).await?;
    let steps = asked(&amt);
    assert_eq!(
        steps.iter().map(|input| &*input.class).collect::<Vec<_>>(),
        ["ChangeBootOrder_INPUT", "AMT_BootSettingData", "SetBootConfigRole_INPUT", "RequestPowerStateChange_INPUT"]
    );
    assert_eq!((steps[1].get("BIOSSetup"), steps[3].get("PowerState")), (Some("true"), Some("10")));

    // The BIOS's screen on SOL, booting as usual.
    client.set_power(ResetToSerial).await?;
    let put = asked(&amt).into_iter().find(|input| input.class == "AMT_BootSettingData").context("no boot settings")?;
    assert_eq!((put.get("UseSOL"), put.get("BIOSSetup")), (Some("true"), Some("false")));
    Ok(())
}

/// Intel's sample of a device that won't show a consent code: a return value that isn't 0 is the error.
#[tokio::test]
async fn a_refused_method_is_an_error() -> Result<()> {
    let amt = samples().await;
    match client(&amt, PASSWORD, None)?.start_consent().await {
        Err(Error::Method { method, code }) => assert_eq!((&*method, &*code), ("StartOptIn", "2")),
        other => bail!("expected a refusal, got {other:?}"),
    }
    Ok(())
}

/// A device without a class faults its methods as it does its reads.
#[tokio::test]
async fn a_method_of_a_class_the_device_lacks_is_a_fault() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let amt = start(fixture.path()).await;
    let refused = client(&amt, PASSWORD, None)?.cancel_consent().await;
    assert!(matches!(refused, Err(Error::Fault(_))), "{refused:?}");
    Ok(())
}

/// AMT at times hangs up instead of answering. Reading changes nothing, so we ask again; a change may have gone
/// through, so we never make it twice.
#[tokio::test]
async fn a_read_lost_on_the_way_is_tried_again_a_change_is_not() -> Result<()> {
    let amt = samples().await;
    let mut client = client(&amt, PASSWORD, None)?;
    client.info().await?;
    amt.asked();

    amt.hang_up();
    client.get("AMT_GeneralSettings").await?;
    let reads = amt.asked();
    assert!(reads.len() == 2 && reads[0] == reads[1], "{reads:#?}");

    amt.hang_up();
    assert!(matches!(client.set_power(PowerAction::Reset).await, Err(Error::Transport(_))));
    assert_eq!(asked(&amt).len(), 1);
    Ok(())
}

/// A WS-Man reply with `body` as its body.
fn reply(body: &str) -> String {
    format!(r#"<Envelope xmlns="http://www.w3.org/2003/05/soap-envelope"><Header/><Body>{body}</Body></Envelope>"#)
}

/// A list or a reply the device never ends is an error, rather than ours to read forever.
#[tokio::test]
async fn refuses_what_has_no_end() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let wsman = fixture.path().join("wsman");
    std::fs::create_dir(&wsman)?;
    let page = reply("<PullResponse><EnumerationContext>more</EnumerationContext><Items/></PullResponse>");
    std::fs::write(wsman.join("CIM_Chip.Pull.xml"), &page)?;
    for n in 2..=101 {
        std::fs::write(wsman.join(format!("CIM_Chip.Pull.{n}.xml")), &page)?;
    }
    let events = reply("<GetRecords_OUTPUT><RecordArray>AA==</RecordArray></GetRecords_OUTPUT>");
    std::fs::write(wsman.join("AMT_MessageLog.GetRecords.IterationIdentifier=,MaxReadRecords=390.xml"), events)?;
    let total = "<TotalRecordCount>4294967295</TotalRecordCount>";
    let records = reply(&format!("<ReadRecords_OUTPUT>{total}<EventRecords>AA==</EventRecords></ReadRecords_OUTPUT>"));
    for start in 1..=100 {
        std::fs::write(wsman.join(format!("AMT_AuditLog.ReadRecords.StartIndex={start}.xml")), &records)?;
    }
    std::fs::write(wsman.join("AMT_GeneralSettings.Get.xml"), reply(&"<a/>".repeat(3 << 20)))?;

    let amt = start(fixture.path()).await;
    let mut client = client(&amt, PASSWORD, None)?;
    let endless = |error| matches!(error, Err(Error::Protocol(error)) if error == "a list that doesn't end");
    assert!(endless(client.enumerate("CIM_Chip").await.map(drop)));
    assert!(endless(client.event_log().await.map(drop)));
    assert!(endless(client.audit_log().await.map(drop)));
    match client.get("AMT_GeneralSettings").await {
        Err(Error::Protocol(error)) => assert_eq!(error, "a reply of more than 8388608 bytes"),
        other => bail!("expected a refusal, got {:?}", other.map(drop)),
    }
    Ok(())
}

/// Every fixture is a regression test: we must keep reading whatever that device told us, and know it for what it is
/// without being told.
#[tokio::test]
async fn reads_every_fixture() -> Result<()> {
    for fixture in mock::fixtures() {
        let (flavor, name) = (mock::flavor(&fixture), fixture.display().to_string());
        let target = match flavor {
            Flavor::Amt => start(&fixture).await.target(PASSWORD),
            Flavor::Supermicro | Flavor::Ipmi | Flavor::Redfish => {
                Bmc::start("127.0.0.1:0", &fixture).await.target(PASSWORD)
            }
        };
        let (mut client, info) = ooblin_core::Client::login(&target, None, None).await.context(name.clone())?;
        assert_eq!(info.flavor, flavor, "{name}");
        client.power().await.context(name.clone())?;
        client.event_log().await.context(name.clone())?;
        // We read hardware over AMT and Redfish only.
        if flavor != Flavor::Ipmi {
            let hardware = client.hardware().await.context(name.clone())?;
            anyhow::ensure!(
                !hardware.processors.is_empty() && !hardware.memory.is_empty(),
                "{name}: no processor or memory"
            );
        }
        match &mut client {
            ooblin_core::Client::Amt(client) => {
                client.settings().await.context(name.clone())?;
                client.audit_log().await.context(name)?;
            }
            ooblin_core::Client::Supermicro(client) => {
                client.sensors().await.context(name.clone())?;
                client.bmc_settings().await.context(name)?;
            }
            ooblin_core::Client::Ipmi(client) => {
                client.sensors().await.context(name.clone())?;
                client.settings().await.context(name)?;
            }
            ooblin_core::Client::Redfish(client) => {
                client.sensors().await.context(name.clone())?;
                client.status().await.context(name)?;
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn reads_what_intels_samples_say() -> Result<()> {
    let amt = samples().await;
    let mut client = client(&amt, PASSWORD, None)?;
    let hardware = client.hardware().await?;
    assert_eq!(hardware.system.model, "NUC9V7QNX");
    assert_eq!(hardware.processors[0].model, "Intel(R) Core(TM) i7-9850H CPU @ 2.60GHz");
    let module = hardware.memory.iter().find(|module| module.slot == "BANK 2").context("no BANK 2")?;
    assert_eq!(module.part_number, "CT16G4SFD824A.M16FE");
    let times: Vec<_> = client.audit_log().await?.into_iter().map(|record| record.time).collect();
    assert!(times.len() == 2 && times[0] > times[1], "newest first: {times:?}");
    Ok(())
}

/// As the MS-02's own web pages show it.
#[tokio::test]
async fn reads_what_the_ms_02_says() -> Result<()> {
    let amt = start(&mock::fixture("ms-02")).await;
    let mut client = client(&amt, PASSWORD, None)?;
    let hardware = client.hardware().await?;
    let processor = &hardware.processors[0];
    assert_eq!(
        (&*processor.family, &*processor.socket, &*processor.upgrade_method),
        ("Core Ultra 9", "U3E1", "Socket BGA2114")
    );
    assert_eq!((processor.mhz, processor.max_mhz), (Some(4600), Some(8500)));
    assert_eq!((hardware.memory[0].bytes, hardware.disks[0].bytes), (Some(32 << 30), Some(2_000_398_934_000)));
    assert!(hardware.batteries.is_empty());
    let status = client.status().await?;
    // PlatformGUID 419CA440943AC455A192A4409CD22E55, its first three fields swapped as SMBIOS does.
    assert_eq!([&*status.system_id, &*hardware.system.id], ["40a49c41-3a94-55c4-a192-a4409cd22e55"; 2]);
    let port = &status.ports[0];
    assert_eq!(port.ipv4.subnet_mask, "255.255.192.0");
    assert!(port.ipv6.is_none(), "IPv6 is off");
    for times in [
        client.event_log().await?.into_iter().map(|event| event.time).collect::<Vec<_>>(),
        client.audit_log().await?.into_iter().map(|record| record.time).collect(),
    ] {
        assert!(times.is_sorted_by(|a, b| a >= b), "newest first");
    }
    Ok(())
}

/// Each change is only what differs, an instance going back whole with it.
#[tokio::test]
async fn changes_the_settings() -> Result<()> {
    let amt = start(&mock::fixture("ms-02")).await;
    let mut client = client(&amt, PASSWORD, None)?;
    let settings = client.settings().await?;
    amt.asked();

    let access = RemoteAccess { sol: false, consent: Consent::All, ..settings.remote_access };
    client.change(Change::RemoteAccess(access)).await?;
    let changes = asked(&amt);
    assert_eq!(changes[0].get("RequestedState"), Some("32769"));
    assert_eq!((&*changes[1].class, changes[1].get("OptInRequired")), ("IPS_OptInService", Some("4294967295")));
    assert_eq!(changes.len(), 2, "KVM and the listener stay on");

    let general = General { name: "renamed.example.com".into(), respond_to_ping: false, ..settings.general };
    client.change(Change::General(general)).await?;
    let put = asked(&amt).pop().context("no Put")?;
    let name = (put.get("HostName"), put.get("DomainName"));
    assert_eq!((name, put.get("PingResponseEnabled")), ((Some("renamed"), Some("example.com")), Some("false")));
    assert_eq!(put.get("DigestRealm"), Some("Digest:FA88D31BAFC44865DC5345817456CFA3"), "kept");

    let active: Vec<_> = settings.power_policies.iter().filter(|policy| policy.active).collect();
    assert_eq!((&*active[0].id, &*active[0].description), ("SCHEME 1", "ON in S0, ME Wake in S3, S4-5 (AC only)"));
    client.change(Change::PowerPolicy("SCHEME 0".into())).await?;
    let request = amt.asked().pop().context("no request")?;
    assert!(request.contains(r#"Name="InstanceID">SCHEME 0<"#), "on the scheme: {request}");

    // Network: what isn't an address never goes out, nor a manual configuration without one. A manual address names
    // the port, the OS's and DHCP take the addresses out.
    let network = client.settings().await?.network;
    let typo = Ipv4 { address: "192.0.2.700".into(), ..network.ipv4 };
    let blank =
        Ipv4 { configuration: Configuration::Manual, address: String::new(), ..client.settings().await?.network.ipv4 };
    let typo6 = Ipv6Settings { address: "2001:db8::g".into(), ..network.ipv6 };
    amt.asked();
    for change in [Change::Ipv4(typo), Change::Ipv4(blank), Change::Ipv6(typo6)] {
        assert!(matches!(client.change(change).await, Err(Error::Invalid(_))));
    }
    assert!(amt.asked().is_empty(), "sent");
    let ipv4 = Ipv4 { configuration: Configuration::Manual, address: "192.0.2.7".into(), ..settings.network.ipv4 };
    client.change(Change::Ipv4(ipv4)).await?;
    let request = amt.asked().pop().context("no Put")?;
    assert!(request.contains(r#"Name="InstanceID">Intel(r) AMT Ethernet Port Settings 0<"#), "{request}");
    let put = parse_instance(&request)?;
    assert_eq!((put.get("DHCPEnabled"), put.get("IPAddress")), (Some("false"), Some("192.0.2.7")));
    for (configuration, dhcp, shared) in [(Configuration::Os, "false", "true"), (Configuration::Dhcp, "true", "false")]
    {
        let ipv4 = Ipv4 { configuration, address: "192.0.2.7".into(), ..client.settings().await?.network.ipv4 };
        client.change(Change::Ipv4(ipv4)).await?;
        let put = asked(&amt).pop().context("no Put")?;
        let port = [put.get("DHCPEnabled"), put.get("IpSyncEnabled"), put.get("SharedStaticIp"), put.get("IPAddress")];
        assert_eq!(port, [Some(dhcp), Some(shared), Some(shared), None], "{configuration:?}");
    }
    let available = Availability { asleep: true, ..settings.network.available };
    client.change(Change::Availability(available)).await?;
    let put = asked(&amt).pop().context("no Put")?;
    assert_eq!((put.all("LinkPolicy").collect::<Vec<_>>(), put.get("IPAddress")), (vec!["1", "14", "16"], None));

    // IPv6 is off on the MS-02: turning it on links it again, with the port and its settings as keys.
    assert_eq!(settings.network.ipv6.configuration, Ipv6Configuration::Off);
    let ipv6 = Ipv6Settings { configuration: Ipv6Configuration::Automatic, ..settings.network.ipv6 };
    client.change(Change::Ipv6(ipv6)).await?;
    let request = amt.asked().pop().context("no Put")?;
    assert!(request.contains(">Intel(r) AMT Ethernet Port 0<") && request.contains(">Intel(r) IPS IPv6 Settings 0<"));
    assert_eq!(parse_instance(&request)?.get("IsCurrent"), Some("1"), "{request}");

    // Neither recovery nor erase on the MS-02: asked for, its boot service stays as it was, which is an error. And
    // only AMT's own certificate, in use and staying.
    assert_eq!((settings.recovery.one_click, settings.recovery.platform_erase), (None, None));
    let recovery = Recovery { one_click: Some(true), platform_erase: None };
    assert!(matches!(client.change(Change::Recovery(recovery)).await, Err(Error::Protocol(_))));
    assert_eq!(asked(&amt)[0].get("RequestedState"), Some("32769"));
    let own = &settings.certificates[0];
    assert!(own.read_only && own.in_use && own.name == "Intel® AMT self-signed certificate", "{own:?}");
    assert!(settings.tls.iter().all(|tls| tls.enabled && !tls.mutual) && settings.tls[1].local);
    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(&file, "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n")?;
    client.change(Change::AddCertificate { path: file.path().into(), trusted_root: true }).await?;
    assert_eq!(asked(&amt).pop().context("no add")?.get("CertificateBlob"), Some("MIIB"));
    let gone = Change::AddCertificate { path: file.path().with_extension("gone"), trusted_root: true };
    assert!(matches!(client.change(gone).await, Err(Error::Invalid(_))));
    client.change(Change::DeleteCertificate("Intel(r) AMT Certificate: Handle: 1".into())).await?;
    let request = amt.asked().pop().context("no Delete")?;
    assert!(request.contains("transfer/Delete") && request.contains(">Intel(r) AMT Certificate: Handle: 1<"));

    let general = General { dns_update: DnsUpdate::Amt, ..client.settings().await?.general };
    client.change(Change::General(general)).await?;
    let put = asked(&amt).pop().context("no Put")?;
    assert_eq!((put.get("DDNSUpdateEnabled"), put.get("DDNSUpdateByDHCPServerEnabled")), (Some("true"), Some("false")));

    let alarm = Alarm { name: "Backups".into(), at: 1_800_000_000, every_minutes: 1440, forget: false };
    client.change(Change::AddAlarm(alarm)).await?;
    let request = amt.asked().pop().context("no alarm")?;
    assert!(request.contains(">2027-01-15T08:00:00Z<") && request.contains(">P1DT0H0M<"), "{request}");
    client.change(Change::DeleteAlarm("Backups".into())).await?;
    assert!(amt.asked().pop().context("no Delete")?.contains(r#"Name="InstanceID">Backups<"#));

    // The MS-02 was recorded headless, so with no screen active, and shows the first.
    let screens = client.screens().await?;
    assert!(screens.active.is_empty() && screens.shown == 0, "{screens:?}");
    client.show_screen(1).await?;
    assert_eq!(asked(&amt).pop().context("no Put")?.get("DefaultScreen"), Some("1"));

    client.change(Change::Clock).await?;
    let set = asked(&amt).pop().context("no time")?;
    assert_eq!((&*set.class, set.get("Ta0")), ("SetHighAccuracyTimeSynch_INPUT", Some("1792388976")));
    Ok(())
}

#[tokio::test]
async fn opens_redirection_sessions_with_digest() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    Transcript(vec![received(b"hello")]).save(&fixture.path().join("sol.transcript"))?;
    let amt = start(fixture.path()).await;

    let mut sol = Redirection::open(&amt.target(PASSWORD), None, Session::Sol).await?;
    assert_eq!(sol.read_exact(5).await?, b"hello");
    let refused = Redirection::open(&amt.target("wrong"), None, Session::Sol).await;
    assert!(matches!(refused, Err(Error::Unauthorized)));
    Ok(())
}

#[tokio::test]
async fn reaches_an_ipv6_address_with_or_without_brackets() -> Result<()> {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        eprintln!("Skipped: this machine has no IPv6 loopback.");
        return Ok(());
    }
    let amt = Amt::start("[::1]:0", "[::1]:0", &mock::fixture("ms-02")).await;

    for host in ["::1", "[::1]"] {
        let target = Target { host: host.into(), ..amt.target(PASSWORD) };
        Client::new(&target, None)?.info().await.context(host)?;
        Redirection::open(&target, None, Session::Sol).await.context(host)?;
    }
    Ok(())
}

/// A frame's rectangles after its size: x, y, width, height and RGBA pixels (see `Kvm::frame`).
fn rects(frame: &[u8]) -> Vec<(u16, u16, u16, u16, &[u8])> {
    let u16 = |at: usize| u16::from_le_bytes([frame[at], frame[at + 1]]);
    let mut rects = Vec::new();
    let mut at = 4;
    while at < frame.len() {
        let (width, height) = (u16(at + 4), u16(at + 6));
        let len = usize::from(width) * usize::from(height) * 4;
        rects.push((u16(at), u16(at + 2), width, height, &frame[at + 8..at + 8 + len]));
        at += 8 + len;
    }
    rects
}

/// As the MS-02 recorded it: AMT 19 first only says its size, then sends the screen in ZRLE tiles of 64 by 64.
#[tokio::test]
async fn shows_the_ms_02_screen() -> Result<()> {
    let amt = start(&mock::fixture("ms-02")).await;
    let kvm = client(&amt, PASSWORD, None)?.kvm().await?;
    let size = [0x20, 3, 0x58, 2]; // 800 by 600
    assert_eq!(kvm.frame().await?, size);
    let frame = kvm.frame().await?;
    assert_eq!(frame[..4], size);
    let rects = rects(&frame);
    assert_eq!(rects.iter().map(|&(_, _, w, h, _)| u32::from(w) * u32::from(h)).sum::<u32>(), 800 * 600);
    assert_eq!(kvm.frame().await?, size, "nothing changed, so after a second just the size");
    Ok(())
}

/// Every kind of ZRLE tile (RFC 6143 7.7.6), all on one zlib stream, on a 3 by 14 screen. And as AMT has it, a tile it
/// didn't compress outside that stream, a zlib stored block, which the next tile's back-references must not see.
#[tokio::test]
async fn decodes_every_kind_of_zrle_tile() -> Result<()> {
    let [r, g, b] = [0xF800u16, 0x07E0, 0x001F].map(u16::to_le_bytes);
    let tiles = [
        ([&[1][..], &r].concat(), [r, r, r, r, r, r]),                     // solid
        ([&[0][..], &r, &g, &b, &b, &g, &r].concat(), [r, g, b, b, g, r]), // raw
        ([&[2][..], &r, &b, &[0b0101_0100]].concat(), [r, b, r, b, r, b]), // a bit each, across rows
        ([&[128][..], &r, &[1], &g, &[3]].concat(), [r, r, g, g, g, g]),   // runs
        ([&[131][..], &r, &g, &b, &[0x80, 2, 0x81, 1, 2]].concat(), [r, r, r, g, g, b]), // runs of a palette
        ([&[0][..], &b, &b, &b, &g, &g, &g].concat(), [b, b, b, g, g, g]), // stored, see below
        ([&[0][..], &r, &g, &b, &b, &g, &r].concat(), [r, g, b, b, g, r]), // the raw one again
    ];
    let mut update = vec![0, 0, 0, tiles.len() as u8];
    let mut zlib = Compress::new(Compression::default(), true);
    for (i, (tile, _)) in tiles.iter().enumerate() {
        let mut data = Vec::with_capacity(1024);
        if i == 5 {
            let len = (tile.len() as u16).to_le_bytes();
            data = [&[0][..], &len, &len.map(|byte| !byte), tile].concat();
        } else {
            zlib.compress_vec(tile, &mut data, FlushCompress::Sync)?;
        }
        update.extend(
            [&[0, 0, 0, 2 * i as u8, 0, 3, 0, 2, 0, 0, 0, 16][..], &(data.len() as u32).to_be_bytes(), &data].concat(),
        );
    }
    let fixture = kvm(vec![sent(&WHOLE), received(&update)])?;

    let amt = start(fixture.path()).await;
    let frame = client(&amt, PASSWORD, None)?.kvm().await?.frame().await?;
    let rects = rects(&frame);
    assert_eq!((&frame[..4], rects.len()), (&[3, 0, 14, 0][..], tiles.len()));
    let rgba = |pixel: [u8; 2]| match pixel {
        pixel if pixel == r => [255, 0, 0, 255],
        pixel if pixel == g => [0, 255, 0, 255],
        _ => [0, 0, 255, 255],
    };
    for (i, ((_, expected), (x, y, w, h, pixels))) in tiles.iter().zip(rects).enumerate() {
        assert_eq!((x, y, w, h), (0, 2 * i as u16, 3, 2));
        assert_eq!(pixels, expected.map(rgba).concat(), "tile {i}");
    }
    Ok(())
}

/// A key and a mouse button held when the screen loses focus are let go of, the mouse where it was. The mock hangs up on
/// any byte we get wrong, which the next frame would see.
#[tokio::test]
async fn lets_go_of_what_is_held() -> Result<()> {
    let fixture = kvm(vec![
        sent(&[4, 1, 0, 0, 0, 0, 0, b'a']),
        sent(&[5, 1, 0, 2, 0, 9]), // the left button, at 2, 9
        sent(&[4, 0, 0, 0, 0, 0, 0, b'a']),
        sent(&[5, 0, 0, 2, 0, 9]),
        sent(&WHOLE),
        received(&[0, 0, 0, 0]), // an update of nothing
    ])?;
    let amt = start(fixture.path()).await;
    let kvm = client(&amt, PASSWORD, None)?.kvm().await?;
    let held = [Input::Key { code: "KeyA".into(), down: true }, Input::Pointer { x: 2, y: 9, buttons: 1, scroll: 0 }];
    kvm.input(held.into()).await?;
    kvm.input(vec![Input::Release, Input::Release]).await?;
    assert_eq!(kvm.frame().await?, [3, 0, 14, 0]);
    Ok(())
}

/// A screen AMT can't keep at two bytes a pixel, 3840 by 2160: once it says its size we ask for one byte a pixel, as
/// MeshCommander does, and for all of it anew.
#[tokio::test]
async fn asks_for_fewer_colors_on_a_large_screen() -> Result<()> {
    let size = [0x0F, 0, 0x08, 0x70];
    let fixture = kvm(vec![
        sent(&WHOLE),
        received(&[&[0, 0, 0, 1, 0, 0, 0, 0][..], &size, &[0xFF, 0xFF, 0xFF, 0x21]].concat()),
        sent(&[0, 0, 0, 0, 8, 8, 0, 1, 0, 7, 0, 7, 0, 3, 5, 2, 0, 0, 0, 0]),
        sent(&[&[3, 0, 0, 0, 0, 0][..], &size].concat()),
        received(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0, 0xE0, 0x03]), // red and blue, in Raw
    ])?;
    let amt = start(fixture.path()).await;
    let kvm = client(&amt, PASSWORD, None)?.kvm().await?;
    assert_eq!(kvm.frame().await?, [0, 0x0F, 0x70, 0x08]);
    assert_eq!(rects(&kvm.frame().await?), [(0, 0, 2, 1, &[255, 0, 0, 255, 0, 0, 255, 255][..])]);
    Ok(())
}

/// Text being typed keeps neither the screen nor `close` waiting: both get the keyboard between two characters.
#[tokio::test]
async fn shows_the_screen_while_typing() -> Result<()> {
    let fixture = kvm(vec![sent(&WHOLE), received(&[0, 0, 0, 0])])?;
    let amt = start(fixture.path()).await;
    let kvm = Arc::new(client(&amt, PASSWORD, None)?.kvm().await?);

    let typing = tokio::spawn({
        let kvm = kvm.clone();
        async move { kvm.input(vec![Input::Text { text: "a".repeat(1000) }]).await }
    });
    tokio::task::yield_now().await;
    assert_eq!(kvm.frame().await?, [3, 0, 14, 0]);
    assert!(!typing.is_finished(), "the screen waited for the typing");

    kvm.close().await;
    assert!(typing.await?.is_err(), "the typing went on after the goodbye");
    Ok(())
}

/// An update of more rectangles than any screen has: 17 000 tiles of 64 by 64, each a stored block of one color.
#[tokio::test]
async fn refuses_an_update_larger_than_any_screen() -> Result<()> {
    let tile = [&[0, 0, 0, 0, 0, 64, 0, 64, 0, 0, 0, 16, 0, 0, 0, 8][..], &[0, 3, 0, 0xFC, 0xFF, 1, 0, 0]].concat();
    let update = [&[0, 0, 0x42, 0x68][..], &tile.repeat(0x4268)].concat();
    let fixture = kvm(vec![sent(&WHOLE), received(&update)])?;

    let amt = start(fixture.path()).await;
    let kvm = client(&amt, PASSWORD, None)?.kvm().await?;
    match kvm.frame().await {
        Err(Error::Protocol(error)) => assert_eq!(error, "an update of more than 268435456 bytes"),
        other => bail!("expected a refusal, got {:?}", other.map(|frame| frame.len())),
    }
    Ok(())
}

/// A device that goes away in the middle of a message, a rectangle's pixels, a line of its console or a command for
/// its drive: an error at once, where waiting for the rest would hang.
#[tokio::test]
async fn a_device_hanging_up_mid_message_is_an_error() -> Result<()> {
    let gone = |error: Error| matches!(error, Error::Transport(why) if why == "the device closed the connection");
    let open = |session: &str| Transcript::load(&mock::fixture("ms-02").join(format!("{session}.transcript")));

    let half = received(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 3, 0, 14, 0, 0, 0, 0, 0xF8]);
    let fixture = kvm(vec![sent(&WHOLE), half, received(&[])])?;
    let amt = start(fixture.path()).await;
    assert!(client(&amt, PASSWORD, None)?.kvm().await?.frame().await.is_err_and(gone));

    let half = received(&[0x2A, 0, 0, 0, 1, 0, 0, 0, 5, 0, b'h']);
    let fixture = redirecting("sol", [&open("sol")?.0[..3], &[half, received(&[])]].concat())?;
    let amt = start(fixture.path()).await;
    assert!(client(&amt, PASSWORD, None)?.sol().await?.read().await.is_err_and(gone));

    let (on, half) =
        (sent(&[0x48, 0, 0, 0, 1, 0, 0, 0, 3, 0x09, 0, 0, 0]), received(&[0x50, 0, 0, 0, 2, 0, 0, 0, 0, 0]));
    let fixture = redirecting("ider", [open("ider")?.0, vec![on, half, received(&[])]].concat())?;
    let amt = start(fixture.path()).await;
    let image = tempfile::tempfile()?;
    image.set_len(2048)?;
    let ider = client(&amt, PASSWORD, None)?.ider(image).await?;
    assert!(ider.serve(|_| {}, std::future::pending()).await.is_err_and(gone));
    Ok(())
}

/// Our request for the whole of a 3 by 14 screen.
const WHOLE: [u8; 10] = [3, 0, 0, 0, 0, 0, 0, 3, 0, 14];

/// A KVM session on a 3 by 14 screen, opened as AMT 19 does, then `then` after our SetEncodings.
fn kvm(then: Vec<(Direction, Vec<u8>)>) -> Result<tempfile::TempDir> {
    let init = [0, 3, 0, 14, 16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 11, 5, 0, 0, 0, 0, 0, 0, 0, 0];
    let opened = [
        sent(&[0x40, 0, 0, 0, 0, 0, 0, 0]),
        received(&[&[0x41, 0, 0, 0, 0, 0, 0, 0][..], b"RFB 004.000\n"].concat()),
        sent(b"RFB 003.008\n"),
        received(&[1, 1]),
        sent(&[1]),
        received(&[0, 0, 0, 0]),
        sent(&[1]),
        received(&init),
        sent(&[2, 0, 0, 3, 0, 0, 0, 16, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x21]),
    ];
    redirecting("kvm", [opened.to_vec(), then].concat())
}

/// A device whose `session` ("kvm", "sol" or "ider") goes as `transcript` has it, all of them on as on the MS-02.
fn redirecting(session: &str, transcript: Vec<(Direction, Vec<u8>)>) -> Result<tempfile::TempDir> {
    let fixture = tempfile::tempdir()?;
    Transcript(transcript).save(&fixture.path().join(format!("{session}.transcript")))?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for file in ["CIM_KVMRedirectionSAP.Get.xml", "AMT_RedirectionService.Get.xml", "IPS_OptInService.Get.xml"] {
        std::fs::copy(mock::fixture("ms-02").join("wsman").join(file), fixture.path().join("wsman").join(file))?;
    }
    Ok(fixture)
}

/// What's off stays off: a session needs it turned on in the settings first, and changes nothing.
#[tokio::test]
async fn opens_only_what_is_on() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for (file, on, off) in [
        ("CIM_KVMRedirectionSAP.Get.xml", "EnabledState>6<", "EnabledState>3<"),
        ("AMT_RedirectionService.Get.xml", "ListenerEnabled>true<", "ListenerEnabled>true<"),
        ("IPS_OptInService.Get.xml", "OptInRequired>0<", "OptInRequired>0<"),
    ] {
        let xml = std::fs::read_to_string(mock::fixture("ms-02").join("wsman").join(file))?;
        anyhow::ensure!(xml.contains(on), "{file} has no {on}");
        std::fs::write(fixture.path().join("wsman").join(file), xml.replace(on, off))?;
    }
    let amt = start(fixture.path()).await;
    let mut client = client(&amt, PASSWORD, None)?;
    assert!(matches!(client.kvm().await, Err(Error::Off)));
    assert!(client.remote_access().await?.sol, "SOL is on");
    assert!(asked(&amt).is_empty(), "nothing changed");
    Ok(())
}

/// Where consent is required for everything, a session waits for the code someone at the device reads out.
#[tokio::test]
async fn asks_for_consent() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for file in ["CIM_KVMRedirectionSAP.Get.xml", "AMT_RedirectionService.Get.xml", "IPS_OptInService.Get.xml"] {
        let xml = std::fs::read_to_string(mock::fixture("ms-02").join("wsman").join(file))?;
        let xml = xml.replace("OptInRequired>0<", "OptInRequired>4294967295<");
        std::fs::write(fixture.path().join("wsman").join(file), xml)?;
    }
    let amt = start(fixture.path()).await;
    let mut client = client(&amt, PASSWORD, None)?;
    assert!(matches!(client.sol().await, Err(Error::Consent)));

    client.start_consent().await?;
    client.send_consent(123456).await?;
    client.cancel_consent().await?;
    let inputs = asked(&amt);
    let inputs: Vec<_> = inputs.iter().map(|input| (&*input.class, input.get("OptInCode"))).collect();
    assert_eq!(
        inputs,
        [("StartOptIn_INPUT", None), ("SendOptInCode_INPUT", Some("123456")), ("CancelOptIn_INPUT", None)]
    );
    Ok(())
}

/// A device booting from the image the way MeshCommander answers it: is the drive ready, how big is the disc, then
/// a read larger than a message may carry, a command we don't know, a read past the end and why it failed, a read in
/// READ (12), the empty floppy drive, a ping, a read in DMA, a read the drive's reset cuts short, and the end. The mock
/// hangs up on any byte we get wrong.
#[tokio::test]
async fn serves_a_disk_image_over_ider() -> Result<()> {
    let image: Vec<u8> = (0..6 * 2048).map(|i| (i % 251) as u8).collect();
    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), &image)?;

    // The device's messages carry its sequence, ours ours. A command is for the CD-ROM (0xB0), in PIO.
    let message = |kind: u8, attributes: u8, sequence: u32, data: &[u8]| {
        [&[kind, 0, 0, attributes][..], &sequence.to_le_bytes(), data].concat()
    };
    let to = |device: u8, sequence, cdb: &[u8]| {
        let mut cdb = cdb.to_vec();
        cdb.resize(12, 0);
        message(0x50, 0, sequence, &[&[0, 0, 0, 0, 0, 0, device, 0][..], &cdb].concat())
    };
    let command = |sequence, cdb: &[u8]| to(0xB0, sequence, cdb);
    // The same with DMA asked for, in the features register.
    let dma = |sequence, cdb: &[u8]| {
        let mut command = command(sequence, cdb);
        command[9] = 1;
        command
    };
    let done = [0x85, 0, 3, 0, 0, 0, 0xB0, 0x50, 0, 0, 0, 0, 0, 0];
    let failed = |sequence, device, [sense, asc]: [u8; 2]| {
        message(
            0x51,
            2,
            sequence,
            &[&[0; 12][..], &[0x87, sense << 4, 3, 0, 0, 0, device, 0x51, sense, asc, 0]].concat(),
        )
    };
    let ok =
        |sequence| message(0x51, 2, sequence, &[&[0; 12][..], &[0xC5, 0, 3, 0, 0, 0, 0xB0, 0x50, 0, 0, 0]].concat());
    let data = |sequence, data: &[u8], last: bool| {
        let len = (data.len() as u16).to_le_bytes();
        let header = [0, len[0], len[1], 0, 0xB5, 0, 2, 0, len[0], len[1], 0xB0, 0x58];
        let end = if last { done } else { [0; 14] };
        message(0x54, if last { 2 } else { 0 }, sequence, &[&header[..], &end, data].concat())
    };
    let opened = Transcript::load(&mock::fixture("ms-02").join("ider.transcript"))?;
    let mut transcript = opened.0; // our open and the MS-02's answer, 8192 bytes at most a message
    transcript.extend([
        sent(&message(0x48, 0, 1, &[3, 0x09, 0, 0, 0])),
        received(&message(0x49, 0, 1, &[3, 1, 0, 0, 0])),
        received(&command(2, &[0x00])), // TEST UNIT READY
        sent(&ok(2)),
        received(&command(3, &[0x25])), // READ CAPACITY
        sent(&data(3, &[0, 0, 0, 5, 0, 0, 8, 0], true)),
        received(&command(4, &[0x28, 0, 0, 0, 0, 1, 0, 0, 5])), // READ (10), blocks 1 to 5
        sent(&data(4, &image[2048..2048 + 8192], false)),
        sent(&data(5, &image[2048 + 8192..], true)),
        received(&command(5, &[0x12])), // INQUIRY, which AMT answers itself
        sent(&failed(6, 0xB0, [5, 0x20])),
        received(&command(6, &[0x28, 0, 0, 0, 0, 5, 0, 0, 2])), // READ (10), blocks 5 and 6 of 6
        sent(&failed(7, 0xB0, [5, 0x21])),
        received(&command(7, &[0x03, 0, 0, 0, 18])), // REQUEST SENSE: why, out of range
        sent(&data(8, &[0x70, 0, 5, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x21, 0, 0, 0, 0, 0], true)),
        received(&command(8, &[0xA8, 0, 0, 0, 0, 0, 0, 0, 0, 1])), // READ (12), block 0
        sent(&data(9, &image[..2048], true)),
        received(&to(0xA0, 9, &[0x28, 0, 0, 0, 0, 0, 0, 0, 1])), // a read from the floppy drive
        sent(&failed(10, 0xA0, [2, 0x3A])),
        received(&message(0x44, 0, 10, &[])), // a ping, which we pong
        sent(&message(0x45, 0, 11, &[])),
        received(&dma(11, &[0x28, 0, 0, 0, 0, 0, 0, 0, 1])), // block 0 again, in DMA
        sent(&message(
            0x54,
            3,
            12,
            &[&[0, 0, 8, 0, 0xB4, 0, 2, 0, 0, 0, 0xB0, 0x58][..], &done, &image[..2048]].concat(),
        )),
        // Blocks 1 to 5 again, the drive reset before we sent any: the first message goes, the rest doesn't.
        received(&[command(12, &[0x28, 0, 0, 0, 0, 1, 0, 0, 5]), message(0x46, 0, 13, &[1])].concat()),
        sent(&data(13, &image[2048..2048 + 8192], false)),
        sent(&message(0x47, 0, 14, &[])),
        received(&message(0x43, 0, 14, &[])),
    ]);
    let fixture = redirecting("ider", transcript)?;

    let amt = start(fixture.path()).await;
    let ider = client(&amt, PASSWORD, None)?.ider(std::fs::File::open(file.path())?).await?;
    let mut reads = Vec::new();
    ider.serve(|served| reads.push(served), std::future::pending()).await?;
    assert_eq!(reads, [2048..6 * 2048, 0..2048, 0..2048, 2048..2048 + 8192]);
    Ok(())
}

/// A serial console: what the device writes comes in as it writes it, what we type goes out, and keeping the session
/// alive in between goes unnoticed. The mock hangs up on any byte of the start we get wrong, and on a message that
/// isn't numbered as the next.
#[tokio::test]
async fn reads_and_types_on_the_serial_console() -> Result<()> {
    let opened = Transcript::load(&mock::fixture("ms-02").join("sol.transcript"))?;
    let output = |sequence: u8, text: &[u8]| {
        let len = (text.len() as u16).to_le_bytes();
        received(&[&[0x2A, 0, 0, 0, sequence, 0, 0, 0, len[0], len[1]][..], text].concat())
    };
    let mut transcript = opened.0[..3].to_vec(); // our start, the MS-02's answer, our control lines
    transcript.extend([output(1, b"hello"), sent(&[0x28, 0, 0, 0, 3, 0, 0, 0, 1, 0, b'\r']), output(2, b"login: ")]);
    let fixture = redirecting("sol", transcript)?;

    let amt = start(fixture.path()).await;
    let sol = client(&amt, PASSWORD, None)?.sol().await?;
    assert_eq!(sol.read().await?, b"hello");
    assert_eq!(sol.read().await?, b"", "nothing for a second");
    sol.write(b"\r").await?;
    assert_eq!(sol.read().await?, b"login: ");
    assert_eq!(amt.asked().last().map(String::as_str), Some(r#"SOL "\r""#));
    Ok(())
}

/// A device that keeps writing hears from us as often as a quiet one: the mock hangs up unless our keep-alive comes
/// before its next line.
#[tokio::test]
async fn keeps_a_busy_serial_console_alive() -> Result<()> {
    let opened = Transcript::load(&mock::fixture("ms-02").join("sol.transcript"))?;
    let output = |text: &[u8]| received(&[&[0x2A, 0, 0, 0, 1, 0, 0, 0, text.len() as u8, 0][..], text].concat());
    let mut transcript = opened.0[..3].to_vec(); // our start, the MS-02's answer, our control lines
    transcript.extend([output(b"one"), sent(&[0x2B, 0, 0, 0, 3, 0, 0, 0]), output(b"two")]);
    let fixture = redirecting("sol", transcript)?;

    let amt = start(fixture.path()).await;
    let sol = client(&amt, PASSWORD, None)?.sol().await?;
    assert_eq!(sol.read().await?, b"one");
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(sol.read().await?, b"two");
    Ok(())
}

/// As the MS-02 recorded it: the kernel saying so as a disk image is mounted.
#[tokio::test]
async fn shows_the_ms_02_serial_console() -> Result<()> {
    let amt = start(&mock::fixture("ms-02")).await;
    let sol = client(&amt, PASSWORD, None)?.sol().await?;
    let mut text = Vec::new();
    while let read @ [_, ..] = &sol.read().await?[..] {
        text.extend_from_slice(read);
    }
    let text = String::from_utf8(text)?;
    assert!(text.contains("Intel    Virtual CD") && text.contains("USB disconnect"), "{text}");
    Ok(())
}

/// The MS-02 answers a presence ping with an acknowledgement and then its pong, which reads as AMT 19.0, configured.
/// An address in a network and on its own is found once.
#[tokio::test]
async fn finds_the_ms_02_by_its_presence_pong() -> Result<()> {
    let port = mock::answer_pings("127.0.0.1:0").await?;
    let mut found = Vec::new();
    discover("127.0.0.1, 127.0.0.0/30", port, |device| found.push(device)).await?;
    let ms_02 = Found { host: "127.0.0.1".into(), flavor: Flavor::Amt, version: "19.0".into(), provisioned: true };
    assert_eq!(found, [ms_02]);

    for networks in ["127.0.0.1/33", "localhost", "10.0.0.0/15", "10.0.0.0/16 10.1.0.0/16"] {
        assert!(matches!(discover(networks, port, |_| {}).await, Err(Error::Invalid(_))), "{networks}");
    }
    Ok(())
}

/// What a pong says, each from a device of its own: the MS-02's and the X10's as they sent them, then the MS-02's
/// with the state of AMT not provisioned yet (byte 19) and with 11.8 as older AMT packs its version (byte 18), and the
/// X10's of a device that says it has no IPMI, which is none we know.
#[tokio::test]
async fn reads_what_a_pong_says() -> Result<()> {
    let x10 = hex("0600ff06000011be40000010000011be000000008100000000000000");
    let with = |pong: &[u8], at: usize, byte: u8| {
        let mut pong = pong.to_vec();
        pong[at] = byte;
        pong
    };
    let amt = |version: &str, provisioned| Some((Flavor::Amt, version.to_owned(), provisioned));
    let pongs = [
        (PONG.to_vec(), amt("19.0", true)),
        (x10.clone(), Some((Flavor::Ipmi, String::new(), true))),
        (with(&PONG, 19, 0), amt("19.0", false)),
        (with(&PONG, 18, 0xB8), amt("11.8", true)),
        (with(&x10, 20, 0), None),
    ];
    try_join_all(pongs.map(async |(pong, said)| {
        let device = UdpSocket::bind("127.0.0.1:0").await?;
        let port = device.local_addr()?.port();
        let answer = async {
            let mut ping = [0; 12];
            while let Ok((_, from)) = device.recv_from(&mut ping).await {
                let _ = device.send_to(&pong, from).await;
            }
        };
        let mut found = Vec::new();
        tokio::select! {
            () = answer => anyhow::bail!("the device stopped answering"),
            discovered = discover("127.0.0.1", port, |device| found.push(device)) => discovered?,
        }
        let found = found.pop().map(|found| (found.flavor, found.version, found.provisioned));
        anyhow::ensure!(found == said, "{found:?}, not {said:?}");
        anyhow::Ok(())
    }))
    .await?;
    Ok(())
}
