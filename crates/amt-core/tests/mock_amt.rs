//! amt-core against our mock AMT: TLS and pinning, Digest on both ports, and reading what real devices told us.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use amt_core::{Client, ControlMode, Error, Fingerprint, PowerAction, PowerState, Redirection, Session, Target, Trust};
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

fn client(amt: &Amt, password: &str, pin: Option<Fingerprint>) -> Client {
    Client::new(&target(amt, password), pin).unwrap()
}

fn fingerprint() -> Fingerprint {
    Fingerprint::of(&Amt::certificate())
}

#[tokio::test]
async fn trusts_on_first_use_and_reads_info_over_one_connection() {
    let amt = samples().await;
    let info = client(&amt, PASSWORD, None).info().await.unwrap();

    assert_eq!(info.hostname, "Test Host Name");
    assert_eq!(info.version, "12.0.67");
    assert_eq!(info.control_mode, ControlMode::Admin);
    assert_eq!((info.fingerprint, info.trust), (fingerprint(), Trust::FirstUse));
    assert_eq!(amt.connections.load(Ordering::SeqCst), 1);
    assert_eq!(amt.challenges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn accepts_the_pinned_certificate() {
    let amt = samples().await;
    let info = client(&amt, PASSWORD, Some(fingerprint())).info().await.unwrap();
    assert_eq!(info.trust, Trust::Pinned);
}

#[tokio::test]
async fn refuses_a_changed_certificate() {
    let amt = samples().await;
    let old = Fingerprint::of(b"the certificate before reprovisioning");
    match client(&amt, PASSWORD, Some(old)).info().await {
        Err(Error::PinMismatch { expected, actual }) => assert_eq!((expected, actual), (old, fingerprint())),
        other => panic!("expected a pin mismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_wrong_credentials() {
    let amt = samples().await;
    assert!(matches!(client(&amt, "wrong", None).info().await, Err(Error::Unauthorized)));
}

#[tokio::test]
async fn answers_a_fresh_challenge_silently() {
    let amt = samples().await;
    let mut client = client(&amt, PASSWORD, None);
    client.get("AMT_GeneralSettings").await.unwrap();

    amt.expire_nonce();
    client.get("AMT_GeneralSettings").await.unwrap();
    assert_eq!(amt.challenges.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reads_and_changes_the_power_state() {
    let amt = samples().await;
    let mut client = client(&amt, PASSWORD, None);
    assert_eq!(client.power_state().await.unwrap(), PowerState::On);

    client.power(PowerAction::Reset).await.unwrap();
    assert!(amt.requests.lock().unwrap().last().unwrap().contains("<h:PowerState>10</h:PowerState>"));
}

#[tokio::test]
async fn reads_the_inventory() {
    let amt = samples().await;
    let inventory = client(&amt, PASSWORD, None).inventory().await.unwrap();

    assert_eq!(inventory.model, "NUC9V7QNX");
    assert_eq!(inventory.board, "Intel Corporation NUC9V7QNB");
    assert_eq!((inventory.bios.as_str(), inventory.bios_date.as_str()), ("QNCFLX70.0054.2020.0810.2227", "2020-08-10"));
    assert_eq!(inventory.processor, "Intel(R) Core(TM) i7-9850H CPU @ 2.60GHz");
    assert_eq!(inventory.memory.len(), 2);
    assert_eq!((inventory.memory[0].bytes, inventory.memory[0].mhz), (16 << 30, 2400));
    assert_eq!(inventory.memory[0].part, "CT16G4SFD824A.M16FE");
}

/// Every fixture is a regression test: we must keep reading whatever that device told us.
#[tokio::test]
async fn reads_every_fixture() {
    for fixture in mock_amt::fixtures() {
        let amt = start(&fixture).await;
        let mut client = client(&amt, PASSWORD, None);
        let name = fixture.display();

        let info = client.info().await.unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!info.version.is_empty(), "{name}: no AMT version");
        client.power_state().await.unwrap_or_else(|error| panic!("{name}: {error}"));
        let inventory = client.inventory().await.unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!inventory.processor.is_empty() && !inventory.memory.is_empty(), "{name}: no CPU or memory");
    }
}

/// A fixture whose SOL session says hello once it's open.
fn hello() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    Transcript(vec![(Direction::Received, b"hello".to_vec())]).save(&dir.path().join("sol.transcript")).unwrap();
    dir
}

#[tokio::test]
async fn opens_a_redirection_session_with_digest() {
    let fixture = hello();
    let amt = start(fixture.path()).await;
    let mut sol = Redirection::open(&target(&amt, PASSWORD), None, Session::Sol).await.unwrap();
    assert_eq!(sol.read_exact(5).await.unwrap(), b"hello");
}

#[tokio::test]
async fn refuses_a_redirection_session_with_a_wrong_password() {
    let fixture = hello();
    let amt = start(fixture.path()).await;
    let opened = Redirection::open(&target(&amt, "wrong"), None, Session::Sol).await;
    assert!(matches!(opened, Err(Error::Unauthorized)));
}

#[tokio::test]
async fn reports_a_redirection_session_that_is_not_enabled() {
    let fixture = hello();
    let amt = start(fixture.path()).await;
    let opened = Redirection::open(&target(&amt, PASSWORD), None, Session::Kvm).await;
    assert!(matches!(opened, Err(Error::Protocol(message)) if message.contains("is it enabled")));
}
