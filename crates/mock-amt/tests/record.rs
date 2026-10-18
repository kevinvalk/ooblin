//! Recording devices we scripted ourselves, through the mock and the real handshakes: what we record has to be exactly
//! what the device said, minus what we scrub.

use std::path::Path;

use amt_core::{Client, Connection, Target};
use anyhow::Result;
use mock_amt::{Amt, Direction, PASSWORD, Transcript, USER};

fn target(amt: &Amt) -> Target {
    Target { wsman_port: amt.port, redirection_port: amt.redirection_port, ..Target::new("127.0.0.1", USER, PASSWORD) }
}

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
        sent(&[2, 0, 0, 2, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x21]),
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
    mock_amt::record(&target(&amt), recorded.path()).await?;
    for name in ["sol", "kvm", "ider"] {
        let load = |dir: &Path| Transcript::load(&dir.join(format!("{name}.transcript")));
        assert_eq!(load(recorded.path())?, load(device.path())?, "{name}");
    }
    Ok(())
}

#[tokio::test]
async fn crawls_into_a_scrubbed_fixture_that_replays() -> Result<()> {
    let device = Amt::start("127.0.0.1:0", "127.0.0.1:0", &mock_amt::fixture("go-wsman-messages")).await;
    let crawled = tempfile::tempdir()?;
    mock_amt::crawl(&mut Connection::new(&target(&device), None)?, &crawled.path().join("wsman")).await?;

    let replay = Amt::start("127.0.0.1:0", "127.0.0.1:0", crawled.path()).await;
    let hardware = Client::new(&target(&replay), None)?.hardware().await?;
    assert_eq!(hardware.system.model, "NUC9V7QNX");
    assert_ne!(hardware.system.serial_number, "JRQN0243007J", "serial numbers are scrubbed");
    Ok(())
}
