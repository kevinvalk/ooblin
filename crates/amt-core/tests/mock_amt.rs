//! amt-core against our mock AMT, replaying Intel's samples and the devices we recorded.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use amt_core::wsman::parse_instance;
use amt_core::{
    Alarm, Availability, Client, Configuration, Consent, DnsUpdate, Error, General, Input, Ipv4, Ipv6Configuration,
    Ipv6Settings, PowerAction, PowerState, Redirection, RemoteAccess, Session, Target, discover, fingerprint,
};
use anyhow::{Context, Result, bail};
use flate2::{Compress, Compression, FlushCompress};
use mock_amt::{Amt, Direction, PASSWORD, Transcript, USER};

async fn start(fixture: &Path) -> Arc<Amt> {
    Amt::start("127.0.0.1:0", "127.0.0.1:0", fixture).await
}

async fn samples() -> Arc<Amt> {
    start(&mock_amt::fixture("go-wsman-messages")).await
}

fn target(amt: &Amt, password: &str) -> Target {
    Target { wsman_port: amt.port, redirection_port: amt.redirection_port, ..Target::new("127.0.0.1", USER, password) }
}

fn client(amt: &Amt, password: &str, pin: Option<String>) -> Result<Client> {
    Ok(Client::new(&target(amt, password), pin)?)
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

    // What we sent, by the input or instance in each request's body.
    let sent = || -> Vec<_> {
        amt.requests.lock().unwrap().iter().filter_map(|request| parse_instance(request).ok()).collect()
    };
    client.set_power(PowerAction::Reset).await?;
    let last = sent().pop().context("no request")?;
    assert_eq!((&*last.class, last.get("PowerState")), ("RequestPowerStateChange_INPUT", Some("10")));

    assert!(power.actions.contains(&PowerAction::ResetToBios));
    amt.requests.lock().unwrap().clear();
    client.set_power(PowerAction::ResetToBios).await?;
    let steps: Vec<_> = sent().into_iter().map(|input| input.class).collect();
    assert_eq!(
        steps,
        ["ChangeBootOrder_INPUT", "AMT_BootSettingData", "SetBootConfigRole_INPUT", "RequestPowerStateChange_INPUT"]
    );
    let put = sent().into_iter().find(|input| input.class == "AMT_BootSettingData").context("no boot settings")?;
    assert_eq!(put.get("BIOSSetup"), Some("true"));

    // The BIOS's screen on SOL, booting as usual.
    client.set_power(PowerAction::ResetToSerial).await?;
    let put = sent().into_iter().rfind(|input| input.class == "AMT_BootSettingData").context("no boot settings")?;
    assert_eq!((put.get("UseSOL"), put.get("BIOSSetup")), (Some("true"), Some("false")));
    Ok(())
}

/// Every fixture is a regression test: we must keep reading whatever that device told us.
#[tokio::test]
async fn reads_every_fixture() -> Result<()> {
    for fixture in mock_amt::fixtures() {
        let amt = start(&fixture).await;
        let mut client = client(&amt, PASSWORD, None)?;
        let name = fixture.display().to_string();
        client.info().await.context(name.clone())?;
        client.power().await.context(name.clone())?;
        client.status().await.context(name.clone())?;
        let hardware = client.hardware().await.context(name.clone())?;
        anyhow::ensure!(
            !hardware.processors.is_empty() && !hardware.memory.is_empty(),
            "{name}: no processor or memory"
        );
        client.settings().await.context(name.clone())?;
        client.event_log().await.context(name.clone())?;
        client.audit_log().await.context(name)?;
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
    let amt = start(&mock_amt::fixture("ms-02")).await;
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
    let port = &client.status().await?.ports[0];
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
    let amt = start(&mock_amt::fixture("ms-02")).await;
    let mut client = client(&amt, PASSWORD, None)?;
    let settings = client.settings().await?;
    let sent = || -> Vec<_> {
        let requests = std::mem::take(&mut *amt.requests.lock().unwrap());
        requests.iter().filter_map(|request| parse_instance(request).ok()).collect()
    };
    sent();

    let access = RemoteAccess { sol: false, consent: Consent::All, ..settings.remote_access };
    client.set_remote_access(&access).await?;
    let changes = sent();
    assert_eq!(changes[0].get("RequestedState"), Some("32769"));
    assert_eq!((&*changes[1].class, changes[1].get("OptInRequired")), ("IPS_OptInService", Some("4294967295")));
    assert_eq!(changes.len(), 2, "KVM and the listener stay on");

    let general = General { name: "renamed.example.com".into(), respond_to_ping: false, ..settings.general };
    client.set_general(&general).await?;
    let put = sent().pop().context("no Put")?;
    let name = (put.get("HostName"), put.get("DomainName"));
    assert_eq!((name, put.get("PingResponseEnabled")), ((Some("renamed"), Some("example.com")), Some("false")));
    assert_eq!(put.get("DigestRealm"), Some("Digest:FA88D31BAFC44865DC5345817456CFA3"), "kept");

    let active: Vec<_> = settings.power_policies.iter().filter(|policy| policy.active).collect();
    assert_eq!((&*active[0].id, &*active[0].description), ("SCHEME 1", "ON in S0, ME Wake in S3, S4-5 (AC only)"));
    client.set_power_policy("SCHEME 0").await?;
    let request = amt.requests.lock().unwrap().pop().context("no request")?;
    assert!(request.contains(r#"Name="InstanceID">SCHEME 0<"#), "on the scheme: {request}");

    // Network: what isn't an address never goes out, a manual address names the port, DHCP takes the addresses out.
    let typo = Ipv4 { address: "192.0.2.700".into(), ..client.settings().await?.network.ipv4 };
    sent();
    assert!(matches!(client.set_ipv4(&typo).await, Err(Error::Invalid(_))));
    let typo = Ipv6Settings { address: "2001:db8::g".into(), ..client.settings().await?.network.ipv6 };
    sent();
    assert!(matches!(client.set_ipv6(&typo).await, Err(Error::Invalid(_))));
    assert!(amt.requests.lock().unwrap().is_empty(), "sent");
    let ipv4 = Ipv4 { configuration: Configuration::Manual, address: "192.0.2.7".into(), ..settings.network.ipv4 };
    client.set_ipv4(&ipv4).await?;
    let request = amt.requests.lock().unwrap().pop().context("no Put")?;
    assert!(request.contains(r#"Name="InstanceID">Intel(r) AMT Ethernet Port Settings 0<"#), "{request}");
    let put = parse_instance(&request)?;
    assert_eq!((put.get("DHCPEnabled"), put.get("IPAddress")), (Some("false"), Some("192.0.2.7")));
    let available = Availability { asleep: true, ..settings.network.available };
    client.set_availability(&available).await?;
    let put = sent().pop().context("no Put")?;
    assert_eq!((put.all("LinkPolicy").collect::<Vec<_>>(), put.get("IPAddress")), (vec!["1", "14", "16"], None));

    // IPv6 is off on the MS-02: turning it on links it again, with the port and its settings as keys.
    assert_eq!(settings.network.ipv6.configuration, Ipv6Configuration::Off);
    let ipv6 = Ipv6Settings { configuration: Ipv6Configuration::Automatic, ..settings.network.ipv6 };
    client.set_ipv6(&ipv6).await?;
    let request = amt.requests.lock().unwrap().pop().context("no Put")?;
    assert!(request.contains(">Intel(r) AMT Ethernet Port 0<") && request.contains(">Intel(r) IPS IPv6 Settings 0<"));
    assert_eq!(parse_instance(&request)?.get("IsCurrent"), Some("1"), "{request}");

    // Neither recovery nor erase on the MS-02, and only AMT's own certificate, in use and staying.
    assert_eq!((settings.recovery.one_click, settings.recovery.platform_erase), (None, None));
    let own = &settings.certificates[0];
    assert!(own.read_only && own.in_use && own.name == "Intel® AMT self-signed certificate", "{own:?}");
    assert!(settings.tls.iter().all(|tls| tls.enabled && !tls.mutual) && settings.tls[1].local);
    client.add_certificate(b"-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n", true).await?;
    assert_eq!(sent().pop().context("no add")?.get("CertificateBlob"), Some("MIIB"));
    client.delete_certificate("Intel(r) AMT Certificate: Handle: 1").await?;
    let request = amt.requests.lock().unwrap().pop().context("no Delete")?;
    assert!(request.contains("transfer/Delete") && request.contains(">Intel(r) AMT Certificate: Handle: 1<"));

    let general = General { dns_update: DnsUpdate::Amt, ..client.settings().await?.general };
    client.set_general(&general).await?;
    let put = sent().pop().context("no Put")?;
    assert_eq!((put.get("DDNSUpdateEnabled"), put.get("DDNSUpdateByDHCPServerEnabled")), (Some("true"), Some("false")));

    let alarm = Alarm { name: "Backups".into(), at: 1_800_000_000, every_minutes: 1440, forget: false };
    client.add_alarm(&alarm).await?;
    let request = amt.requests.lock().unwrap().pop().context("no alarm")?;
    assert!(request.contains(">2027-01-15T08:00:00Z<") && request.contains(">P1DT0H0M<"), "{request}");
    client.delete_alarm("Backups").await?;
    assert!(amt.requests.lock().unwrap().pop().context("no Delete")?.contains(r#"Name="InstanceID">Backups<"#));

    // The MS-02 was recorded headless, so with no screen active, and shows the first.
    let screens = client.screens().await?;
    assert!(screens.active.is_empty() && screens.shown == 0, "{screens:?}");
    client.show_screen(1).await?;
    assert_eq!(sent().pop().context("no Put")?.get("DefaultScreen"), Some("1"));

    client.set_clock().await?;
    let set = sent().pop().context("no time")?;
    assert_eq!((&*set.class, set.get("Ta0")), ("SetHighAccuracyTimeSynch_INPUT", Some("1792388976")));
    Ok(())
}

#[tokio::test]
async fn opens_redirection_sessions_with_digest() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    Transcript(vec![received(b"hello")]).save(&fixture.path().join("sol.transcript"))?;
    let amt = start(fixture.path()).await;

    let mut sol = Redirection::open(&target(&amt, PASSWORD), None, Session::Sol).await?;
    assert_eq!(sol.read_exact(5).await?, b"hello");
    let refused = Redirection::open(&target(&amt, "wrong"), None, Session::Sol).await;
    assert!(matches!(refused, Err(Error::Unauthorized)));
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
    let amt = start(&mock_amt::fixture("ms-02")).await;
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
    let fixture = kvm(vec![sent(&[3]), received(&update)])?; // the update request, then the update

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
        sent(&[3]),
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

fn sent(bytes: &[u8]) -> (Direction, Vec<u8>) {
    (Direction::Sent, bytes.to_vec())
}

fn received(bytes: &[u8]) -> (Direction, Vec<u8>) {
    (Direction::Received, bytes.to_vec())
}

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
        sent(&[2]),
    ];
    redirecting("kvm", [opened.to_vec(), then].concat())
}

/// A device whose `session` ("kvm", "sol" or "ider") goes as `transcript` has it, all of them on as on the MS-02.
fn redirecting(session: &str, transcript: Vec<(Direction, Vec<u8>)>) -> Result<tempfile::TempDir> {
    let fixture = tempfile::tempdir()?;
    Transcript(transcript).save(&fixture.path().join(format!("{session}.transcript")))?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for file in ["CIM_KVMRedirectionSAP.Get.xml", "AMT_RedirectionService.Get.xml", "IPS_OptInService.Get.xml"] {
        std::fs::copy(mock_amt::fixture("ms-02").join("wsman").join(file), fixture.path().join("wsman").join(file))?;
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
        let xml = std::fs::read_to_string(mock_amt::fixture("ms-02").join("wsman").join(file))?;
        anyhow::ensure!(xml.contains(on), "{file} has no {on}");
        std::fs::write(fixture.path().join("wsman").join(file), xml.replace(on, off))?;
    }
    let amt = start(fixture.path()).await;
    let mut client = client(&amt, PASSWORD, None)?;
    assert!(matches!(client.kvm().await, Err(Error::Off)));
    assert!(client.remote_access().await?.sol, "SOL is on");
    let sent = amt.requests.lock().unwrap().iter().filter_map(|request| parse_instance(request).ok()).count();
    assert_eq!(sent, 0, "nothing changed");
    Ok(())
}

/// Where consent is required for everything, a session waits for the code someone at the device reads out.
#[tokio::test]
async fn asks_for_consent() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for file in ["CIM_KVMRedirectionSAP.Get.xml", "AMT_RedirectionService.Get.xml", "IPS_OptInService.Get.xml"] {
        let xml = std::fs::read_to_string(mock_amt::fixture("ms-02").join("wsman").join(file))?;
        let xml = xml.replace("OptInRequired>0<", "OptInRequired>4294967295<");
        std::fs::write(fixture.path().join("wsman").join(file), xml)?;
    }
    let amt = start(fixture.path()).await;
    let mut client = client(&amt, PASSWORD, None)?;
    assert!(matches!(client.sol().await, Err(Error::Consent)));

    client.start_consent().await?;
    client.send_consent(123456).await?;
    let sent: Vec<_> = amt.requests.lock().unwrap().iter().filter_map(|request| parse_instance(request).ok()).collect();
    let inputs: Vec<_> = sent.iter().map(|input| (&*input.class, input.get("OptInCode"))).collect();
    assert_eq!(inputs, [("StartOptIn_INPUT", None), ("SendOptInCode_INPUT", Some("123456"))]);
    Ok(())
}

/// A device booting from the image the way MeshCommander answers it: is the drive ready, how big is the disc, then
/// a read larger than a message may carry, a command we don't know, a read past the end and why it failed, a read in
/// READ (12), the empty floppy drive, and the end. The mock hangs up on any byte we get wrong.
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
        let end: [u8; 14] = if last { [0x85, 0, 3, 0, 0, 0, 0xB0, 0x50, 0, 0, 0, 0, 0, 0] } else { [0; 14] };
        message(0x54, if last { 2 } else { 0 }, sequence, &[&header[..], &end, data].concat())
    };
    let opened = Transcript::load(&mock_amt::fixture("ms-02").join("ider.transcript"))?;
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
        received(&message(0x43, 0, 10, &[])),
    ]);
    let fixture = redirecting("ider", transcript)?;

    let amt = start(fixture.path()).await;
    let ider = client(&amt, PASSWORD, None)?.ider(std::fs::File::open(file.path())?).await?;
    let mut reads = Vec::new();
    ider.serve(|served| reads.push(served)).await?;
    assert_eq!(reads, [2048..6 * 2048, 0..2048]);
    Ok(())
}

/// A serial console: what the device writes comes in as it writes it, what we type goes out, and keeping the session
/// alive in between goes unnoticed. The mock hangs up on any byte of the start we get wrong.
#[tokio::test]
async fn reads_and_types_on_the_serial_console() -> Result<()> {
    let opened = Transcript::load(&mock_amt::fixture("ms-02").join("sol.transcript"))?;
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
    Ok(())
}

/// As the MS-02 recorded it: the kernel saying so as a disk image is mounted.
#[tokio::test]
async fn shows_the_ms_02_serial_console() -> Result<()> {
    let amt = start(&mock_amt::fixture("ms-02")).await;
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
/// An address in a network and on its own is pinged once, a silent one once more.
#[tokio::test]
async fn finds_the_ms_02_by_its_presence_pong() -> Result<()> {
    let port = mock_amt::answer_pings("127.0.0.1:0").await?;
    let mut found = Vec::new();
    discover("127.0.0.1, 127.0.0.0/30", port, |device| found.push(device)).await?;
    assert_eq!(found, [amt_core::Found { host: "127.0.0.1".into(), version: "19.0".into(), provisioned: true }]);

    for networks in ["127.0.0.1/33", "localhost", "10.0.0.0/15", "10.0.0.0/16 10.1.0.0/16"] {
        assert!(matches!(discover(networks, port, |_| {}).await, Err(Error::Invalid(_))), "{networks}");
    }
    Ok(())
}
