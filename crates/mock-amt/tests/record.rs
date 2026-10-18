//! Recording devices we scripted ourselves, through the mock and the real handshakes: what we record has to be exactly
//! what the device said, minus what we scrub.

use std::path::Path;

use amt_core::{Client, Connection, Target};
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
async fn records_every_session_as_the_device_said_it() {
    let device = tempfile::tempdir().unwrap();
    for (name, transcript) in [("sol", sol()), ("kvm", kvm()), ("ider", ider())] {
        transcript.save(&device.path().join(format!("{name}.transcript"))).unwrap();
    }
    let amt = Amt::start("127.0.0.1:0", "127.0.0.1:0", device.path()).await;

    let recorded = tempfile::tempdir().unwrap();
    mock_amt::record(&target(&amt), recorded.path()).await.unwrap();
    for name in ["sol", "kvm", "ider"] {
        let load = |dir: &Path| Transcript::load(&dir.join(format!("{name}.transcript"))).unwrap();
        assert_eq!(load(recorded.path()), load(device.path()), "{name}");
    }
}

#[tokio::test]
async fn crawls_into_a_scrubbed_fixture_that_replays() {
    let samples = mock_amt::fixture("go-wsman-messages");
    let device = Amt::start("127.0.0.1:0", "127.0.0.1:0", &samples).await;
    let crawled = tempfile::tempdir().unwrap();
    let answered =
        mock_amt::crawl(&mut Connection::new(&target(&device), None).unwrap(), &crawled.path().join("wsman"))
            .await
            .unwrap();
    let mut classes: Vec<_> = std::fs::read_dir(samples.join("wsman"))
        .unwrap()
        .map(|file| file.unwrap().file_name().to_string_lossy().split('.').next().unwrap().to_owned())
        .collect();
    classes.sort();
    classes.dedup();
    assert_eq!(answered, classes.len(), "every class in the samples answers the crawl");

    let replay = Amt::start("127.0.0.1:0", "127.0.0.1:0", crawled.path()).await;
    let inventory = Client::new(&target(&replay), None).unwrap().inventory().await.unwrap();
    assert_eq!(inventory.model, "NUC9V7QNX");
    assert_ne!(inventory.serial, "JRQN0243007J", "serial numbers are scrubbed");
}
