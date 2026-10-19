//! amt-core against our mock AMT, replaying Intel's samples and the devices we recorded.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use amt_core::wsman::parse_instance;
use amt_core::{Client, Error, PowerAction, PowerState, Redirection, Session, Target, fingerprint};
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
    let admin = &client.settings().await?.accounts[0];
    assert!(admin.admin && admin.name == "admin");
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

#[tokio::test]
async fn opens_redirection_sessions_with_digest() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    Transcript(vec![(Direction::Received, b"hello".to_vec())]).save(&fixture.path().join("sol.transcript"))?;
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

/// Every kind of ZRLE tile (RFC 6143 7.7.6), all on one zlib stream, on a 3 by 10 screen.
#[tokio::test]
async fn decodes_every_kind_of_zrle_tile() -> Result<()> {
    let [r, g, b] = [0xF800u16, 0x07E0, 0x001F].map(u16::to_le_bytes);
    let tiles = [
        ([&[1][..], &r].concat(), [r, r, r, r, r, r]),                     // solid
        ([&[0][..], &r, &g, &b, &b, &g, &r].concat(), [r, g, b, b, g, r]), // raw
        ([&[2][..], &r, &b, &[0b010_00000, 0b101_00000]].concat(), [r, b, r, b, r, b]), // a bit each, rows padded
        ([&[128][..], &r, &[1], &g, &[3]].concat(), [r, r, g, g, g, g]),   // runs
        ([&[131][..], &r, &g, &b, &[0x80, 2, 0x81, 1, 2]].concat(), [r, r, r, g, g, b]), // runs of a palette
    ];
    let mut update = vec![0, 0, 0, tiles.len() as u8];
    let mut zlib = Compress::new(Compression::default(), true);
    for (i, (tile, _)) in tiles.iter().enumerate() {
        let mut data = Vec::with_capacity(1024);
        zlib.compress_vec(tile, &mut data, FlushCompress::Sync)?;
        update.extend(
            [&[0, 0, 0, 2 * i as u8, 0, 3, 0, 2, 0, 0, 0, 16][..], &(data.len() as u32).to_be_bytes(), &data].concat(),
        );
    }
    let init = [0, 3, 0, 10, 16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 11, 5, 0, 0, 0, 0, 0, 0, 0, 0];
    let fixture = tempfile::tempdir()?;
    let (sent, received) =
        (|bytes: &[u8]| (Direction::Sent, bytes.to_vec()), |bytes: &[u8]| (Direction::Received, bytes.to_vec()));
    Transcript(vec![
        sent(&[0x40, 0, 0, 0, 0, 0, 0, 0]),
        received(&[&[0x41, 0, 0, 0, 0, 0, 0, 0][..], b"RFB 004.000\n"].concat()),
        sent(b"RFB 003.008\n"),
        received(&[1, 1]),
        sent(&[1]),
        received(&[0, 0, 0, 0]),
        sent(&[1]),
        received(&init),
        sent(&[2]), // SetEncodings
        sent(&[3]), // the update request
        received(&update),
    ])
    .save(&fixture.path().join("kvm.transcript"))?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for file in ["CIM_KVMRedirectionSAP.Get.xml", "AMT_RedirectionService.Get.xml", "IPS_OptInService.Get.xml"] {
        std::fs::copy(mock_amt::fixture("ms-02").join("wsman").join(file), fixture.path().join("wsman").join(file))?;
    }

    let amt = start(fixture.path()).await;
    let frame = client(&amt, PASSWORD, None)?.kvm().await?.frame().await?;
    let rects = rects(&frame);
    assert_eq!((&frame[..4], rects.len()), (&[3, 0, 10, 0][..], tiles.len()));
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

/// Like Intel's console: KVM on, then the redirection listener, then the session.
#[tokio::test]
async fn turns_kvm_and_the_listener_on_first() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for (file, on, off) in [
        ("CIM_KVMRedirectionSAP.Get.xml", "EnabledState>6<", "EnabledState>3<"),
        ("AMT_RedirectionService.Get.xml", "ListenerEnabled>true<", "ListenerEnabled>false<"),
        ("IPS_OptInService.Get.xml", "OptInRequired>0<", "OptInRequired>0<"),
    ] {
        let xml = std::fs::read_to_string(mock_amt::fixture("ms-02").join("wsman").join(file))?;
        anyhow::ensure!(xml.contains(on), "{file} has no {on}");
        std::fs::write(fixture.path().join("wsman").join(file), xml.replace(on, off))?;
    }
    let amt = start(fixture.path()).await;
    // Without a transcript the session is refused, as on a device where KVM didn't come on.
    assert!(matches!(client(&amt, PASSWORD, None)?.kvm().await, Err(Error::Protocol(_))));

    let sent: Vec<_> = amt.requests.lock().unwrap().iter().filter_map(|request| parse_instance(request).ok()).collect();
    let changes: Vec<_> = sent
        .iter()
        .map(|input| (&*input.class, input.get("RequestedState").or(input.get("ListenerEnabled"))))
        .collect();
    assert_eq!(changes, [("RequestStateChange_INPUT", Some("2")), ("AMT_RedirectionService", Some("true"))]);
    Ok(())
}

/// A device booting from the image the way MeshCommander answers it: is the drive ready, how big is the disc, then
/// a read larger than a message may carry, a command we don't know, a read past the end, the empty floppy drive, and
/// the end. The mock hangs up on any byte we get wrong.
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
    let (sent, received) = (|bytes: Vec<u8>| (Direction::Sent, bytes), |bytes: Vec<u8>| (Direction::Received, bytes));
    let opened = Transcript::load(&mock_amt::fixture("ms-02").join("ider.transcript"))?;
    let fixture = tempfile::tempdir()?;
    let mut transcript = opened.0; // our open and the MS-02's answer, 8192 bytes at most a message
    transcript.extend([
        sent(message(0x48, 0, 1, &[3, 0x09, 0, 0, 0])),
        received(message(0x49, 0, 1, &[3, 1, 0, 0, 0])),
        received(command(2, &[0x00])), // TEST UNIT READY
        sent(ok(2)),
        received(command(3, &[0x25])), // READ CAPACITY
        sent(data(3, &[0, 0, 0, 5, 0, 0, 8, 0], true)),
        received(command(4, &[0x28, 0, 0, 0, 0, 1, 0, 0, 5])), // READ (10), blocks 1 to 5
        sent(data(4, &image[2048..2048 + 8192], false)),
        sent(data(5, &image[2048 + 8192..], true)),
        received(command(5, &[0x12])), // INQUIRY, which AMT answers itself
        sent(failed(6, 0xB0, [5, 0x20])),
        received(command(6, &[0x28, 0, 0, 0, 0, 5, 0, 0, 2])), // READ (10), blocks 5 and 6 of 6
        sent(failed(7, 0xB0, [5, 0x21])),
        received(to(0xA0, 7, &[0x28, 0, 0, 0, 0, 0, 0, 0, 1])), // a read from the floppy drive
        sent(failed(8, 0xA0, [2, 0x3A])),
        received(message(0x43, 0, 8, &[])),
    ]);
    Transcript(transcript).save(&fixture.path().join("ider.transcript"))?;
    std::fs::create_dir(fixture.path().join("wsman"))?;
    for file in ["AMT_RedirectionService.Get.xml", "IPS_OptInService.Get.xml"] {
        std::fs::copy(mock_amt::fixture("ms-02").join("wsman").join(file), fixture.path().join("wsman").join(file))?;
    }

    let amt = start(fixture.path()).await;
    let ider = client(&amt, PASSWORD, None)?.ider(std::fs::File::open(file.path())?).await?;
    let mut reads = Vec::new();
    ider.serve(|served| reads.push(served)).await?;
    assert_eq!(reads, [5 * 2048]);
    Ok(())
}
