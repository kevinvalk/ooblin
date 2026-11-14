//! Recording devices we scripted ourselves, through the mock and the real handshakes: what we record has to be exactly
//! what the device said, minus what we scrub.

use std::path::Path;

use anyhow::Result;
use mock::{Amt, Bmc, Direction, PASSWORD, Transcript};
use ooblin_core::amt::{Client, Connection};
use ooblin_core::{ipmi, supermicro};

fn sent(bytes: &[u8]) -> (Direction, Vec<u8>) {
    (Direction::Sent, bytes.to_vec())
}

fn received(bytes: &[u8]) -> (Direction, Vec<u8>) {
    (Direction::Received, bytes.to_vec())
}

fn sol() -> Transcript {
    let start =
        [&[0x20, 0, 0, 0, 1, 0, 0, 0][..], &[0x10, 0x27, 100, 0, 0, 0, 0x10, 0x27, 100, 0, 0, 0, 0, 0, 0, 0]].concat();
    Transcript(vec![
        sent(&start),
        received(&[0x21; 23]),
        sent(&[0x27, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0x1B, 0, 0, 0]),
        sent(&[0x28, 0, 0, 0, 3, 0, 0, 0, 1, 0, b'\r']),
        received(&[&[0x2A, 0, 0, 0, 1, 0, 0, 0, 7, 0][..], b"login: "].concat()),
    ])
}

/// A 2 by 1 screen in 16 bits, one full frame in Raw.
fn kvm() -> Transcript {
    let init = [0, 2, 0, 1, 16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 11, 5, 0, 0, 0, 0, 0, 0, 0, 4];
    Transcript(vec![
        sent(&[0x40, 0, 0, 0, 0, 0, 0, 0]),
        received(&[0x41, 0, 0, 0, 0, 0, 0, 0]),
        received(b"RFB 004.000\n"),
        sent(b"RFB 003.008\n"),
        received(&[1]),
        received(&[1]),
        sent(&[1]),
        received(&[0, 0, 0, 0]),
        sent(&[1]),
        received(&init),
        received(b"test"),
        sent(&[2, 0, 0, 3, 0, 0, 0, 16, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x21]),
        sent(&[3, 0, 0, 0, 0, 0, 0, 2, 0, 1]),
        received(&[2]), // AMT 19 rings the Bell, then only tells its desktop size
        received(&[0]),
        received(&[0, 0, 1]),
        received(&[0, 0, 0, 0, 0, 2, 0, 1, 0xFF, 0xFF, 0xFF, 0x21]),
        sent(&[3, 0, 0, 0, 0, 0, 0, 2, 0, 1]),
        received(&[0]),
        received(&[0, 0, 1]),
        received(&[0, 0, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0]),
        received(&[0xF8, 0x00, 0x07, 0xE0]),
    ])
}

fn ider() -> Transcript {
    let mut reply =
        vec![0x41, 0, 0, 0, 0, 0, 0, 0, 1, 0, 19, 0, 0, 0, 0, 0, 0, 0x20, 0, 0x20, 0, 0, 0, 0, 0, 0x57, 0x01, 0, 0, 2];
    reply.extend_from_slice(b"ok");
    Transcript(vec![
        sent(&[0x40, 0, 0, 0, 0, 0, 0, 0, 0x30, 0x75, 0, 0, 0x20, 0x4E, 1, 0, 0, 0]),
        received(&reply[..30]),
        received(&reply[30..]),
    ])
}

#[tokio::test]
async fn records_every_session_as_the_device_said_it() -> Result<()> {
    let device = tempfile::tempdir()?;
    for (name, transcript) in [("sol", sol()), ("kvm", kvm()), ("ider", ider())] {
        transcript.save(&device.path().join(format!("{name}.transcript")))?;
    }
    let amt = Amt::start("127.0.0.1:0", "127.0.0.1:0", device.path()).await;

    let recorded = tempfile::tempdir()?;
    mock::record(&amt.target(PASSWORD), recorded.path()).await?;
    for name in ["sol", "kvm", "ider"] {
        let load = |dir: &Path| Transcript::load(&dir.join(format!("{name}.transcript")));
        assert_eq!(load(recorded.path())?, load(device.path())?, "{name}");
    }
    Ok(())
}

#[tokio::test]
async fn crawls_into_a_scrubbed_fixture_that_replays() -> Result<()> {
    let device = Amt::start("127.0.0.1:0", "127.0.0.1:0", &mock::fixture("go-wsman-messages")).await;
    let crawled = tempfile::tempdir()?;
    mock::crawl(&mut Connection::new(&device.target(PASSWORD), None)?, &crawled.path().join("wsman")).await?;

    let replay = Amt::start("127.0.0.1:0", "127.0.0.1:0", crawled.path()).await;
    let hardware = Client::new(&replay.target(PASSWORD), None)?.hardware().await?;
    assert_eq!(hardware.system.model, "NUC9V7QNX");
    assert_ne!(hardware.system.serial_number, "JRQN0243007J", "serial numbers are scrubbed");
    Ok(())
}

#[tokio::test]
async fn records_a_bmc_into_a_scrubbed_fixture_that_replays() -> Result<()> {
    let device = Bmc::start("127.0.0.1:0", &mock::fixture("x10")).await;
    let recorded = tempfile::tempdir()?;
    mock::redfish::record(&device.target(PASSWORD), recorded.path()).await?;
    mock::ipmi::record(&device.target(PASSWORD), recorded.path()).await?;
    let replay = Bmc::start("127.0.0.1:0", recorded.path()).await;
    let client = |bmc: &Bmc| supermicro::Client::new(&bmc.target(PASSWORD), None);
    let (mut device, mut replay) = (client(&device)?, client(&replay)?);
    assert_eq!(replay.info().await?.version, device.info().await?.version);
    assert_eq!(format!("{:?}", replay.event_log().await?), format!("{:?}", device.event_log().await?));
    let (hardware, original) = (replay.hardware().await?, device.hardware().await?);
    assert_eq!(hardware.processors[0].model, original.processors[0].model);
    assert_ne!(hardware.memory[0].serial_number, original.memory[0].serial_number, "serial numbers are scrubbed");

    // A fake of a fake is another fake.
    let (status, original) = (replay.status().await?, device.status().await?);
    let (port, original_port) = (&status.ports[0], &original.ports[0]);
    assert_ne!(status.system_id, original.system_id);
    assert_ne!(port.mac, original_port.mac);
    assert_ne!(port.ipv4.address, original_port.ipv4.address);
    assert_ne!(port.ipv6.as_ref().map(|ipv6| &ipv6.addresses), original_port.ipv6.as_ref().map(|ipv6| &ipv6.addresses));
    Ok(())
}

/// A BMC over IPMI alone, recorded and replayed: what it reads the same but its addresses, user names, GUID and FRU
/// serials, and found the same, whichever pings it answers. ipmi_sim's starts sessions at User and knows SHA-1 alone.
#[tokio::test]
async fn records_a_bmc_over_ipmi_into_a_scrubbed_fixture_that_replays() -> Result<()> {
    for fixture in ["ipmi-sim", "openbmc"] {
        let device = Bmc::start("127.0.0.1:0", &mock::fixture(fixture)).await;
        let recorded = tempfile::tempdir()?;
        mock::ipmi::record(&device.target(PASSWORD), recorded.path()).await?;
        let replay = Bmc::start("127.0.0.1:0", recorded.path()).await;
        let mut found = 0;
        ooblin_core::discover("127.0.0.1", replay.ipmi_port, |_| found += 1).await?;
        assert_eq!(found, 1, "{fixture}");

        let (device, replay) =
            (ipmi::Client::new(&device.target(PASSWORD)), ipmi::Client::new(&replay.target(PASSWORD)));
        assert_eq!(format!("{:?}", replay.sensors().await?), format!("{:?}", device.sensors().await?), "{fixture}");
        assert_eq!(format!("{:?}", replay.event_log().await?), format!("{:?}", device.event_log().await?));
        assert_eq!(format!("{:?}", replay.power().await?), format!("{:?}", device.power().await?));
        assert_eq!(format!("{:?}", replay.panel().await?), format!("{:?}", device.panel().await?));

        let (settings, original) = (replay.settings().await?, device.settings().await?);
        assert_eq!(settings.users.len(), original.users.len());
        assert_ne!(settings.users[0].name, original.users[0].name, "user names are scrubbed");
        let (network, original_network) = (settings.network.unwrap(), original.network.unwrap());
        assert_eq!(network.subnet_mask, original_network.subnet_mask);
        assert_ne!(network.address, original_network.address);
        let (mac, original_mac) = (&replay.status().await?.ports[0].mac, &device.status().await?.ports[0].mac);
        assert!(original_mac.is_empty() || mac != original_mac, "{fixture}");
        let (fru, original) = (settings.fru.unwrap(), original.fru.unwrap());
        assert_eq!(fru.board.as_ref().map(|b| &b.product), original.board.as_ref().map(|b| &b.product));
        if let (Some(board), Some(original)) = (fru.board, original.board) {
            assert_ne!(board.serial_number, original.serial_number, "serial numbers are scrubbed");
        }
    }
    Ok(())
}
