//! amt-core against our mock AMT, replaying Intel's samples and the devices we recorded.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use amt_core::{Client, Error, Fingerprint, PowerAction, PowerState, Redirection, Session, Target, Trust};
use anyhow::{Context, Result, bail};
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

fn client(amt: &Amt, password: &str, pin: Option<Fingerprint>) -> Result<Client> {
    Ok(Client::new(&target(amt, password), pin)?)
}

#[tokio::test]
async fn pins_the_certificate_on_first_use_and_refuses_another() -> Result<()> {
    let amt = samples().await;
    let pin = Fingerprint::of(&Amt::certificate());

    let info = client(&amt, PASSWORD, None)?.info().await?;
    assert_eq!((info.fingerprint, info.trust), (pin, Trust::FirstUse));
    assert_eq!(client(&amt, PASSWORD, Some(pin))?.info().await?.trust, Trust::Pinned);

    let old = Fingerprint::of(b"the certificate before reprovisioning");
    match client(&amt, PASSWORD, Some(old))?.info().await {
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

    client.set_power(PowerAction::Reset).await?;
    assert!(amt.requests.lock().unwrap().last().context("no request")?.contains("<h:PowerState>10</h:PowerState>"));

    assert!(power.actions.contains(&PowerAction::ResetToBios));
    amt.requests.lock().unwrap().clear();
    client.set_power(PowerAction::ResetToBios).await?;
    let requests = amt.requests.lock().unwrap();
    let steps = ["ChangeBootOrder_INPUT", "<h:BIOSSetup>true", "<h:Role>1", "<h:PowerState>10"];
    let sent: Vec<_> = steps.iter().map(|step| requests.iter().position(|request| request.contains(step))).collect();
    assert!(sent.is_sorted() && !sent.contains(&None), "in order: {sent:?}");
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
    assert_eq!((&*admin.name, &*admin.role), ("admin", "Administrator"));
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
    assert_eq!(processor.summary, "Intel(R) Core(TM) Ultra 9 285HX @ 4600 / 8500 MHz");
    assert_eq!((&*hardware.memory[0].size, &*hardware.disks[0].size), ("32 GB", "2 TB"));
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
