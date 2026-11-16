use std::sync::Arc;

use mock::{Amt, Bmc, PASSWORD};
use ooblin_core::amt::{Change, Settings};
use ooblin_core::{Client, Flavor, Target};
use tauri::async_runtime::JoinHandle;
use tokio::sync::oneshot;

use super::{Changed, Connections, Serving, Task};
use crate::CommandError;
use crate::hosts::Hosts;

async fn mock() -> Arc<Amt> {
    Amt::start("127.0.0.1:0", "127.0.0.1:0", &mock::fixture("ms-02")).await
}

// The mock answers at both names, which are two devices to us.
fn target(amt: &Amt, host: &str) -> Target {
    Target { host: host.into(), ..amt.target(PASSWORD) }
}

// The saved devices go to a temporary directory and passwords never to the credential store, as no test remembers one.
fn connections(directory: &tempfile::TempDir) -> Connections {
    Connections::new(directory.path().join("hosts.json"), "ooblin.test".into())
}

fn saved(connections: &Connections) -> Vec<String> {
    connections.hosts().unwrap().devices().into_iter().map(|device| device.host).collect()
}

// NOTE: `join!` starts its futures in order, so each overlap in these tests is set up before anything comes back from
// the mock, and the outcome holds whichever answer comes first.
#[tokio::test]
async fn logins_at_once_are_both_saved() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    let (first, second) = tokio::join!(
        connections.connect(target(&amt, "127.0.0.1"), None, false),
        connections.connect(target(&amt, "localhost"), None, false),
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(saved(&connections), ["127.0.0.1", "localhost"]);
}

#[tokio::test]
async fn a_rename_during_a_login_sticks() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
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
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    let (connected, forgot) =
        tokio::join!(connections.connect(target(&amt, "127.0.0.1"), None, false), connections.forget("127.0.0.1"));
    forgot.unwrap();
    assert!(connected.is_err());
    assert!(connections.get("127.0.0.1").is_err());
    assert!(saved(&connections).is_empty());
}

#[tokio::test]
async fn an_overtaken_login_leaves_the_newer_one_standing() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    let (first, second) = tokio::join!(
        connections.connect(target(&amt, "127.0.0.1"), None, false),
        connections.connect(target(&amt, "127.0.0.1"), None, false),
    );
    assert!(first.is_err());
    second.unwrap();
    assert_eq!(connections.get("127.0.0.1").unwrap().id, 2);
}

/// Saved devices we cannot read stay as they are, and so do those we cannot write. Neither is an empty list to save
/// over!
#[tokio::test]
async fn saved_devices_we_cant_read_or_write_fail_the_login() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    std::fs::write(directory.path().join("hosts.json"), "{").unwrap();
    assert!(connections(&directory).connect(target(&amt, "127.0.0.1"), None, false).await.is_err());
    assert_eq!(std::fs::read_to_string(directory.path().join("hosts.json")).unwrap(), "{");
    assert!(connections(&directory).hosts().err().unwrap().to_string().contains("hosts.json"));

    // We cannot write them here, as a file sits where their folder should be.
    let unwritable = Connections::new(directory.path().join("hosts.json/hosts.json"), "ooblin.test".into());
    assert!(unwritable.connect(target(&amt, "127.0.0.1"), None, false).await.is_err());
    assert!(unwritable.get("127.0.0.1").is_err());
}

/// A certificate other than the pinned one changes nothing we saved, until the user trusts it as `trust` does.
#[tokio::test]
async fn a_changed_certificate_is_refused_until_trusted() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    let pin = connections.hosts().unwrap().pin("127.0.0.1").unwrap();
    connections.close("127.0.0.1").await;
    connections.edit(|hosts| hosts.trust("127.0.0.1", "AB".into())).unwrap();
    let saved = std::fs::read(directory.path().join("hosts.json")).unwrap();

    let refused = connections.connect(target(&amt, "127.0.0.1"), None, false).await;
    let Err(CommandError::PinMismatch { expected, actual, .. }) = refused else { panic!("the certificate passed") };
    assert_eq!((expected.as_str(), &actual), ("AB", &pin));
    assert!(connections.get("127.0.0.1").is_err());
    assert_eq!(std::fs::read(directory.path().join("hosts.json")).unwrap(), saved);

    connections.edit(|hosts| hosts.trust("127.0.0.1", actual)).unwrap();
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
}

/// Quitting closes what is open on every device, and a login still underway comes to nothing.
#[tokio::test]
async fn quitting_closes_every_connection() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    let open = async |client: &mut Client| client.kvm().await.map(Arc::new);
    let screen = connections.open("127.0.0.1", |connection| &connection.kvm, open).await.unwrap();
    let connection = connections.get("127.0.0.1").unwrap();

    let (login, ()) =
        tokio::join!(connections.connect(target(&amt, "localhost"), None, false), connections.close_all());
    assert!(login.is_err());
    assert!(connection.kvm.get(screen).await.is_err());
    assert!(connections.get("127.0.0.1").is_err() && connections.get("localhost").is_err());
    assert_eq!(saved(&connections), ["127.0.0.1"]);
}

#[tokio::test]
async fn a_screen_opening_across_a_reconnect_stays_out_of_both_connections() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();

    // We keep the old connection's client busy, so its screen opens only once it is closed and the new connection is
    // in.
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
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    let open =
        || connections.open("127.0.0.1", |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new));
    let (first, second) = (open().await.unwrap(), open().await.unwrap());
    let kvm = &connections.get("127.0.0.1").unwrap().kvm;
    kvm.close(Some(first)).await;
    assert!(kvm.get(first).await.is_err());
    kvm.get(second).await.unwrap().frame().await.unwrap();

    // Disconnecting closes the one still open.
    let connection = connections.get("127.0.0.1").unwrap();
    connections.close("127.0.0.1").await;
    assert!(connection.kvm.get(second).await.is_err());
}

/// A disk image replaced or taken out has stopped being served by then, so the drive is free for the next, and taking
/// out one since replaced leaves the new one in. Ours are not read, because the mock's device sits idle once the drive
/// is open.
///
/// NOTE: They are served on this test's runtime, which has one thread. So what is stopped without waiting is still
/// served.
#[tokio::test]
async fn a_disk_image_replaced_stops_being_served() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();
    let image = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(image.path(), [0; 4096]).unwrap();

    // An image is served for as long as its `serving` is kept.
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
    assert!(!image.is_serving().await);
}

// We make a change to AMT's settings the way the `change` command does.
async fn change(connections: &Connections, host: &str, change: Change) -> Result<Changed<Settings>, CommandError> {
    let make = async |client: &mut Client, change| client.amt()?.change(change).await;
    connections.change(host, change, make, async |client| client.amt()?.settings().await).await
}

/// The settings are read again after a change, whether it went through or not.
#[tokio::test]
async fn a_change_says_how_it_went_and_the_settings_after() {
    let (amt, directory) = (mock().await, tempfile::tempdir().unwrap());
    let connections = connections(&directory);
    connections.connect(target(&amt, "127.0.0.1"), None, false).await.unwrap();

    let changed = change(&connections, "127.0.0.1", Change::Clock).await.unwrap();
    assert!(changed.error.is_none() && changed.settings.is_ok());

    let missing = Change::AddCertificate { path: directory.path().join("missing.pem"), is_trusted_root: false };
    let changed = change(&connections, "127.0.0.1", missing).await.unwrap();
    assert!(matches!(changed.error, Some(CommandError::Failed { .. })) && changed.settings.is_ok());
}

/// A device saved before we knew BMCs carries no flavor, so it is one of AMT's.
#[test]
fn an_old_hosts_json_reads_as_amt() {
    let directory = tempfile::tempdir().unwrap();
    let saved = r#"{"127.0.0.1": {"pin": "AB", "name": "", "version": "19.0", "user": "admin", "remembered": false}}"#;
    std::fs::write(directory.path().join("hosts.json"), saved).unwrap();
    let hosts = Hosts::open(&directory.path().join("hosts.json"), "ooblin.test").unwrap();
    assert_eq!(hosts.flavor("127.0.0.1"), Some(Flavor::Amt));
}

/// A device whose password we do not keep has no login of ours, and the credential store is not asked for one.
#[test]
fn a_device_not_remembered_has_no_login() {
    let directory = tempfile::tempdir().unwrap();
    let saved = r#"{"127.0.0.1": {"pin": "AB", "user": "admin", "remembered": false}}"#;
    std::fs::write(directory.path().join("hosts.json"), saved).unwrap();
    let hosts = Hosts::open(&directory.path().join("hosts.json"), "ooblin.test").unwrap();
    assert_eq!(hosts.target("127.0.0.1").err().unwrap().to_string(), "No saved password for 127.0.0.1.");
}

// A BMC as the user types it, with its port in the host, which we are connected to.
async fn bmc(connections: &Connections) -> (Arc<Bmc>, String) {
    let bmc = Bmc::start("127.0.0.1:0", &mock::fixture("x10")).await;
    let target = bmc.target(PASSWORD);
    let host = target.host.clone();
    connections.connect(target, None, false).await.unwrap();
    (bmc, host)
}

/// Only an unsaved device is asked whether it speaks Redfish, before it gets a password.
#[tokio::test]
async fn a_bmc_is_found_once_and_saved_as_one() {
    let directory = tempfile::tempdir().unwrap();
    let connections = connections(&directory);
    let (bmc, host) = bmc(&connections).await;
    assert_eq!(connections.hosts().unwrap().flavor(&host), Some(Flavor::Supermicro));
    assert_eq!(bmc.asked()[0], "GET /redfish/v1/ (no login)");

    connections.connect(bmc.target(PASSWORD), None, false).await.unwrap();
    assert!(!bmc.asked().iter().any(|request| request.ends_with("(no login)")));
}

/// A BMC's screen comes and goes through the slot as AMT's does, each in a web session of its own: the one before has
/// logged out before the next logs in.
#[tokio::test]
async fn a_bmc_screen_logs_out_before_the_next_logs_in() {
    let directory = tempfile::tempdir().unwrap();
    let connections = connections(&directory);
    let (bmc, host) = bmc(&connections).await;
    bmc.asked();
    let open =
        || connections.open(&host, |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new));
    let (first, second) = (open().await.unwrap(), open().await.unwrap());
    let kvm = &connections.get(&host).unwrap().kvm;
    assert!(kvm.get(first).await.is_err());
    kvm.get(second).await.unwrap().frame().await.unwrap();
    kvm.close(Some(second)).await;

    let is_web = |request: &String| !request.starts_with("ikvm ");
    let session = ["POST /cgi/login.cgi", "GET /cgi/url_redirect.cgi?url_name=man_ikvm_html5_bootstrap", "WS /"];
    let session = [&session[..], &["WS closed", "GET /cgi/logout.cgi"]].concat();
    assert_eq!(bmc.asked().into_iter().filter(is_web).collect::<Vec<_>>(), [&session[..], &session].concat());
}

/// What only AMT does fails on a BMC without asking it, its serial console goes over IPMI.
#[tokio::test]
async fn a_bmc_doesnt_do_what_only_amt_does() {
    let directory = tempfile::tempdir().unwrap();
    let connections = connections(&directory);
    let (bmc, host) = bmc(&connections).await;
    bmc.asked();
    let changed = change(&connections, &host, Change::Clock).await.unwrap();
    assert_eq!(changed.error.unwrap().to_string(), "this device doesn't support that");
    assert!(bmc.asked().is_empty());

    let open =
        || connections.open(&host, |connection| &connection.sol, async |client| client.sol().await.map(Arc::new));
    let id = open().await.unwrap();
    let sol = &connections.get(&host).unwrap().sol;
    sol.close(Some(id)).await;
    assert_eq!(bmc.asked()[0], "IPMI 06 48 [01, 01, c4, 00, 00, 00]");
}

/// A login to a device we are connected to closes what is open on the connection it replaces, here a screen's web
/// session.
#[tokio::test]
async fn a_connection_replaced_is_closed() {
    let directory = tempfile::tempdir().unwrap();
    let connections = connections(&directory);
    let (bmc, host) = bmc(&connections).await;
    connections
        .open(&host, |connection| &connection.kvm, async |client| client.kvm().await.map(Arc::new))
        .await
        .unwrap();
    let old = connections.get(&host).unwrap();
    bmc.asked();

    connections.connect(bmc.target(PASSWORD), None, false).await.unwrap();
    assert!(old.kvm.0.lock().await.is_none());
    assert!(bmc.asked().contains(&"GET /cgi/logout.cgi".to_owned()));
}

/// An IPMI login has no certificate, so the one pinned over TLS stays, until the device is forgotten.
#[tokio::test]
async fn an_ipmi_login_keeps_the_pin() {
    let directory = tempfile::tempdir().unwrap();
    let connections = connections(&directory);
    let (bmc, host) = bmc(&connections).await;
    let pin = connections.hosts().unwrap().pin(&host);
    assert!(pin.is_some());

    connections.connect(bmc.target(PASSWORD), Some(Flavor::Ipmi), false).await.unwrap();
    assert_eq!(connections.hosts().unwrap().pin(&host), pin);

    connections.forget(&host).await.unwrap();
    assert_eq!(connections.hosts().unwrap().pin(&host), None);
}
