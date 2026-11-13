//! Connections and saved devices against the mock AMT, where work overlaps: logins at once, a login cancelled or
//! overtaken while it awaits the device, a screen opening while the device is connected again, closing what's since
//! replaced. `join!` starts its futures in order, so each overlap is set up before anything comes back from the mock,
//! and the outcome holds whichever answer comes first. Then a Supermicro BMC against its mock, through the same
//! connections. Saved devices go to a temporary directory, passwords never to the credential store.

use std::sync::Arc;

use mock::{Amt, Bmc, PASSWORD, USER};
use ooblin_core::{Flavor, Target};
use tauri::async_runtime::JoinHandle;
use tokio::sync::oneshot;

use super::{Change, Connections, Serving, Task};
use crate::hosts::Hosts;

async fn mock() -> Arc<Amt> {
    Amt::start("127.0.0.1:0", "127.0.0.1:0", &mock::fixture("ms-02")).await
}

/// The mock answers at both names, which are two devices to us.
fn target(amt: &Amt, host: &str) -> Target {
    Target { wsman_port: amt.port, redirection_port: amt.redirection_port, ..Target::new(host, USER, PASSWORD) }
}

fn connections(dir: &tempfile::TempDir) -> Connections {
    Connections::new(dir.path().join("hosts.json"), "ooblin.test".into())
}

fn saved(connections: &Connections) -> Vec<String> {
    connections.hosts().unwrap().devices().into_iter().map(|device| device.host).collect()
}

#[tokio::test]
async fn logins_at_once_are_both_saved() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    let (a, b) = tokio::join!(
        connections.connect(target(&amt, "127.0.0.1"), None, false),
        connections.connect(target(&amt, "localhost"), None, false),
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(saved(&connections), ["127.0.0.1", "localhost"]);
}

#[tokio::test]
async fn a_rename_during_a_login_sticks() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    let (login, renamed) = tokio::join!(connections.connect(target(&amt, "localhost"), None, false), async {
        connections.edit(|hosts| hosts.renamed("127.0.0.1", "renamed"))
    });
    login.unwrap();
    renamed.unwrap();
    assert_eq!(saved(&connections), ["127.0.0.1", "localhost"]);
    assert_eq!(connections.hosts().unwrap().device("127.0.0.1").unwrap().name, "renamed");
}

#[tokio::test]
async fn a_device_forgotten_during_its_login_stays_forgotten() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    let (connected, forgot) =
        tokio::join!(connections.connect(target(&amt, "127.0.0.1"), None, false), connections.forget("127.0.0.1"));
    forgot.unwrap();
    assert!(connected.is_err());
    assert!(connections.get("127.0.0.1").is_err());
    assert!(saved(&connections).is_empty());
}

#[tokio::test]
async fn an_overtaken_login_leaves_the_newer_one_standing() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    let (first, second) = tokio::join!(
        connections.connect(target(&amt, "127.0.0.1"), None, false),
        connections.connect(target(&amt, "127.0.0.1"), None, false),
    );
    assert!(first.is_err());
    second.unwrap();
    assert_eq!(connections.get("127.0.0.1").unwrap().id, 2);
}

/// Saved devices we can't read stay as they are, and so do those we can't write: neither is an empty list to save over.
#[tokio::test]
async fn saved_devices_we_cant_read_or_write_fail_the_login() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    std::fs::write(dir.path().join("hosts.json"), "{").unwrap();
    assert!(connections(&dir).connect(target(&amt, "127.0.0.1"), None, false).await.is_err());
    assert_eq!(std::fs::read_to_string(dir.path().join("hosts.json")).unwrap(), "{");

    // A file where their folder should be.
    let unwritable = Connections::new(dir.path().join("hosts.json/hosts.json"), "ooblin.test".into());
    assert!(unwritable.connect(target(&amt, "127.0.0.1"), None, false).await.is_err());
    assert!(unwritable.get("127.0.0.1").is_err());
}

#[tokio::test]
async fn a_screen_opening_across_a_reconnect_stays_out_of_both_connections() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    // The old connection's client is busy, so its screen opens only once it's closed and the new connection is in.
    let old = connections.get("127.0.0.1").unwrap();
    let busy = old.client.lock().await;
    let (opened, (), ()) = tokio::join!(
        connections.open("127.0.0.1", |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new)),
        connections.close("127.0.0.1"),
        async {
            connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
            drop(busy);
        },
    );
    assert!(opened.is_err());
    assert!(old.kvm.0.lock().await.is_none());
    assert!(connections.get("127.0.0.1").unwrap().kvm.0.lock().await.is_none());
}

#[tokio::test]
async fn closing_a_screen_since_replaced_leaves_the_new_one_open() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    let open =
        || connections.open("127.0.0.1", |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new));
    let (first, second) = (open().await.unwrap(), open().await.unwrap());
    let kvm = &connections.get("127.0.0.1").unwrap().kvm;
    kvm.close(Some(first)).await;
    assert!(kvm.get(first).await.is_err());
    kvm.get(second).await.unwrap().frame().await.unwrap();

    // Disconnecting closes it.
    let connection = connections.get("127.0.0.1").unwrap();
    connections.close("127.0.0.1").await;
    assert!(connection.kvm.get(second).await.is_err());
}

/// A disk image replaced or taken out has stopped being served by then, so the drive is free for the next, and taking
/// out one since replaced leaves the new one in. Ours aren't read: the mock's device sits idle once the drive is open.
/// They're served on this test's runtime, which has one thread: what's stopped without waiting is still served.
#[tokio::test]
async fn a_disk_image_replaced_stops_being_served() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    let image = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(image.path(), [0; 4096]).unwrap();
    // Served for as long as `serving` is kept.
    let mount = async |serving: oneshot::Sender<()>| {
        let file = std::fs::File::open(image.path()).unwrap();
        let open = async |client: &mut ooblin_core::Client| {
            let media = client.mount(file).await?;
            let (eject, mut ejected) = tauri::async_runtime::channel(1);
            let task = Task(JoinHandle::Tokio(tokio::spawn(async move {
                let _serving = serving;
                let _ = media.serve(|_| {}, async move { ejected.recv().await.unwrap_or_default() }).await;
            })));
            Ok(Serving { task, eject })
        };
        connections.open("127.0.0.1", |connection| &connection.image, open).await
    };
    let (serving, mut first_served) = oneshot::channel();
    let first = mount(serving).await.unwrap();
    let (serving, mut second_served) = oneshot::channel();
    let second = mount(serving).await.unwrap();
    assert_eq!(first_served.try_recv(), Err(oneshot::error::TryRecvError::Closed));

    let image = &connections.get("127.0.0.1").unwrap().image;
    image.close(Some(first)).await;
    assert_eq!(second_served.try_recv(), Err(oneshot::error::TryRecvError::Empty));
    image.close(Some(second)).await;
    assert_eq!(second_served.try_recv(), Err(oneshot::error::TryRecvError::Closed));
    assert!(!image.serving().await);
}

/// The settings are read again after a change, whether it went through or not.
#[tokio::test]
async fn a_change_says_how_it_went_and_the_settings_after() {
    let (amt, dir) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&dir);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();

    let changed = connections.change("127.0.0.1", Change::Clock).await.unwrap();
    assert!(changed.error.is_none() && changed.settings.is_ok());

    let missing = Change::AddCertificate { path: dir.path().join("missing.pem"), trusted_root: false };
    let changed = connections.change("127.0.0.1", missing).await.unwrap();
    assert!(changed.error.is_some() && changed.settings.is_ok());
}

/// Devices saved before BMCs carry no flavor, they're AMT's.
#[test]
fn an_old_hosts_json_reads_as_amt() {
    let dir = tempfile::tempdir().unwrap();
    let saved = r#"{"127.0.0.1": {"pin": "AB", "name": "", "version": "19.0", "user": "admin", "remembered": false}}"#;
    std::fs::write(dir.path().join("hosts.json"), saved).unwrap();
    let hosts = Hosts::open(&dir.path().join("hosts.json"), "ooblin.test").unwrap();
    assert_eq!(hosts.flavor("127.0.0.1"), Some(Flavor::Amt));
}

/// A BMC as typed, its port in the host, and connected to.
async fn bmc(connections: &Connections) -> (Arc<Bmc>, String) {
    let bmc = Bmc::start("127.0.0.1:0", &mock::fixture("x10")).await;
    let host = format!("127.0.0.1:{}", bmc.port);
    let target = Target { ipmi_port: bmc.ipmi_port, ..Target::new(&host, USER, PASSWORD) };
    connections.connect(target, None, false).await.unwrap();
    (bmc, host)
}

fn requests(bmc: &Bmc) -> Vec<String> {
    std::mem::take(&mut bmc.requests.lock().unwrap())
}

/// Only an unsaved device is asked whether it speaks Redfish, before it gets a password.
#[tokio::test]
async fn a_bmc_is_found_once_and_saved_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let connections = connections(&dir);
    let (bmc, host) = bmc(&connections).await;
    assert_eq!(connections.hosts().unwrap().flavor(&host), Some(Flavor::Supermicro));
    assert_eq!(requests(&bmc)[0], "GET /redfish/v1/ (no login)");

    connections.connect(Target::new(&host, USER, PASSWORD), None, false).await.unwrap();
    assert!(!requests(&bmc).iter().any(|request| request.ends_with("(no login)")));
}

/// A BMC's screen comes and goes through the slot as AMT's does, each in a web session of its own: the one before has
/// logged out before the next logs in.
#[tokio::test]
async fn a_bmc_screen_logs_out_before_the_next_logs_in() {
    let dir = tempfile::tempdir().unwrap();
    let connections = connections(&dir);
    let (bmc, host) = bmc(&connections).await;
    requests(&bmc);
    let open =
        || connections.open(&host, |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new));
    let (first, second) = (open().await.unwrap(), open().await.unwrap());
    let kvm = &connections.get(&host).unwrap().kvm;
    assert!(kvm.get(first).await.is_err());
    kvm.get(second).await.unwrap().frame().await.unwrap();
    kvm.close(Some(second)).await;

    let web = |request: &String| !request.starts_with("ikvm ");
    let session = ["POST /cgi/login.cgi", "GET /cgi/url_redirect.cgi?url_name=man_ikvm_html5_bootstrap", "WS /"];
    let session = [&session[..], &["WS closed", "GET /cgi/logout.cgi"]].concat();
    assert_eq!(requests(&bmc).into_iter().filter(web).collect::<Vec<_>>(), [&session[..], &session].concat());
}

/// What only AMT does fails on a BMC without asking it, its serial console goes over IPMI.
#[tokio::test]
async fn a_bmc_doesnt_do_what_only_amt_does() {
    let dir = tempfile::tempdir().unwrap();
    let connections = connections(&dir);
    let (bmc, host) = bmc(&connections).await;
    requests(&bmc);
    let changed = connections.change(&host, Change::Clock).await;
    assert_eq!(changed.err().unwrap().to_string(), "this device doesn't support that");
    assert!(requests(&bmc).is_empty());

    let open =
        || connections.open(&host, |connection| &connection.sol, async |client| client.sol().await.map(Arc::new));
    let id = open().await.unwrap();
    let sol = &connections.get(&host).unwrap().sol;
    sol.close(Some(id)).await;
    assert_eq!(requests(&bmc)[0], "IPMI 06 48 [01, 01, c4, 00, 00, 00]");
}
