use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use mock::ipmi::Fault;
use mock::{Bmc, PASSWORD, USER, hex};
use ooblin_core::ipmi::{self, Change, Client, Fru, Network, PowerRestore};
use ooblin_core::{
    Configuration, Error, Flavor, Found, PowerAction, PowerState, SensorKind, Severity, Target, discover,
};
use tokio::net::UdpSocket;

// NOTE: The mock replays what the X10 said over IPMI, which we recorded in `fixtures/x10/ipmi.txt`
async fn x10() -> Arc<Bmc> {
    Bmc::start("127.0.0.1:0", &mock::fixture("x10")).await
}

/// Starts a mock that replays `recording` alone, which is in the layout of `ipmi.txt`.
async fn recorded(recording: &str) -> Arc<Bmc> {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(fixture.path().join("ipmi.txt"), recording).unwrap();
    Bmc::start("127.0.0.1:0", fixture.path()).await
}

/// Returns what the mock was asked over IPMI since the last call, oldest first, so calling it also forgets them.
fn asked(bmc: &Bmc) -> Vec<String> {
    bmc.asked().into_iter().filter(|request| request.starts_with("IPMI") || request.starts_with("SOL")).collect()
}

/// The X10's BMC answers a presence ping with its pong alone, which names no vendor and no version but does say it
/// speaks IPMI.
#[tokio::test]
async fn finds_the_x10_by_its_presence_pong() -> Result<()> {
    let bmc = x10().await;
    let mut found = Vec::new();
    discover("127.0.0.1", bmc.ipmi_port, |device| found.push(device)).await?;
    let x10 = Found { host: "127.0.0.1".into(), flavor: Flavor::Ipmi, version: String::new(), is_provisioned: true };
    assert_eq!(found, [x10]);

    // A range the wrong way round holds no address, and we say so rather than find nothing in it.
    let backwards = discover("127.0.0.9-127.0.0.1", bmc.ipmi_port, |_| ()).await;
    assert!(matches!(backwards, Err(Error::Invalid(_))), "{backwards:?}");

    Ok(())
}

/// OpenBMC drops presence pings unless their RMCP sequence number is 255, as ipmitool's is. So we ping the ones that
/// stay silent again with that number. Its pong has ASF's enterprise number the wrong way round.
#[tokio::test]
async fn finds_openbmc_by_a_second_ping() -> Result<()> {
    let bmc = Bmc::start("127.0.0.1:0", &mock::fixture("openbmc")).await;
    let mut found = Vec::new();
    discover("127.0.0.1", bmc.ipmi_port, |device| found.push(device)).await?;
    let openbmc =
        Found { host: "127.0.0.1".into(), flavor: Flavor::Ipmi, version: String::new(), is_provisioned: true };
    assert_eq!(found, [openbmc]);

    Ok(())
}

/// Each login is a session of its own that we close again, because a BMC has only a few. A wrong user or password is
/// refused, as is a session sealed with keys from the wrong one.
#[tokio::test]
async fn logs_in_with_rakp_and_closes_the_session() -> Result<()> {
    let bmc = x10().await;
    let chassis = ipmi::with_session(&bmc.target(PASSWORD), ipmi::chassis).await?;
    assert_eq!((chassis.is_identifying, chassis.restore), (Some(false), Some(PowerRestore::Previous)));
    assert_eq!(asked(&bmc), ["IPMI 00 01 []", "IPMI 06 3c [12, 04, 00, 00]"]);

    for (user, password) in [("root", PASSWORD), (USER, "wrong")] {
        let refused = ipmi::with_session(&Target { user: user.into(), ..bmc.target(password) }, ipmi::chassis).await;
        assert!(matches!(refused, Err(Error::Unauthorized)), "{user}: {refused:?}");
    }
    assert_eq!(asked(&bmc), Vec::<String>::new());

    // A name IPMI has no room for cannot be a user's, and we say so rather than send its start.
    let long = Target { user: "a".repeat(17), ..bmc.target(PASSWORD) };
    assert!(matches!(ipmi::with_session(&long, ipmi::chassis).await, Err(Error::Invalid(_))));

    Ok(())
}

#[tokio::test]
async fn resends_a_request_the_bmc_lost() -> Result<()> {
    let bmc = x10().await;
    bmc.fault("IPMI 00 01", Fault::Lost);
    assert!(ipmi::with_session(&bmc.target(PASSWORD), ipmi::chassis).await?.is_on);
    assert_eq!(asked(&bmc), ["IPMI 00 01 []", "IPMI 00 01 []", "IPMI 06 3c [12, 04, 00, 00]"]);

    Ok(())
}

/// A BMC that is still at a request when we send it again says the resend is a duplicate (0xCF) and then answers. We
/// take that answer and NOT the duplicate's nothing, which would have the server off.
#[tokio::test]
async fn waits_for_the_answer_past_a_duplicate() -> Result<()> {
    let bmc = x10().await;
    bmc.fault("IPMI 00 01", Fault::Late);
    assert!(ipmi::with_session(&bmc.target(PASSWORD), ipmi::chassis).await?.is_on);

    let bmc = recorded("00 01  -> 21\n").await;
    let short = ipmi::with_session(&bmc.target(PASSWORD), ipmi::chassis).await;
    assert!(matches!(short, Err(Error::Protocol(_))), "{short:?}");

    Ok(())
}

/// A BMC that answers none of a request's tries is gone. What we would ask it next fails at once and with no goodbye,
/// rather than each request waiting its turn.
#[tokio::test]
async fn gives_up_on_a_bmc_that_stopped_answering() -> Result<()> {
    let bmc = x10().await;
    for _ in 0..3 {
        bmc.fault("IPMI 06 42 [01]", Fault::Lost);
    }
    let settings = Client::new(&bmc.target(PASSWORD)).settings().await;
    assert!(matches!(settings, Err(Error::Timeout)), "{settings:?}");
    assert_eq!(asked(&bmc), ["IPMI 06 42 [01]"; 3]);

    Ok(())
}

/// How ipmitool logs in to a BMC, as `ipmitool -vvvvv` showed it: Open Session, RAKP 1 and 3 with what the BMC answered
/// them, and a sealed Set Session Privilege Level.
struct Login {
    open: &'static str,
    rakp: [(&'static str, &'static str); 2],
    privilege: &'static str,
}

// OpenIPMI's ipmi_sim with cipher suite 3, which is SHA-1
const IPMI_SIM: Login = Login {
    open: "0600ff0706100000000000000000200000000000a4a3a2a0000000080100000001000008010000000200000801000000",
    rakp: [
        (
            "0600ff0706120000000000000000210000000000120400001ae75116735fe550410a8c8e15bb804b1400000561646d696e",
            "0600ff07061300000000000000003c0000000000a4a3a2a0202a7304a2a2c470a53651349e49a82da123456789abcdefa123\
             456789abcdef768a5b06e62121cde03f8adf8810385f77671bba",
        ),
        (
            "0600ff07061400000000000000001c0000000000120400005946c4d1ee06fdb683d38d2260a4a929c1ed04be",
            "0600ff0706150000000000000000140000000000a4a3a2a0f7c4ecf5e196834878952d70",
        ),
    ],
    privilege: "0600ff0706c012040000030000002000b28bff041fc06d595bde0f2faeae50341b48ea752cc33a47c8\
                bddacc574fd13effff0207aedb0da6383aea02d23348d8",
};

// OpenBMC with cipher suite 17, which is SHA-256
const OPENBMC: Login = Login {
    open: "0600ff0706100000000000000000200000000000a4a3a2a0000000080300000001000008040000000200000801000000",
    rakp: [
        (
            "0600ff0706120000000000000000210000000000c21c5b1c66c791aec87246e9973fe84eef8bc7bf1400000561646d696e",
            "0600ff0706130000000000000000480000000000a4a3a2a02116811ebd79bb71398355ba2c86e4340102030405060708090a0b0c0d\
             0e0f10530ea7ea9d8183764a5e0916c509900b7b3816cd3ab2845e5296b71a3b589e6a",
        ),
        (
            "0600ff0706140000000000000000280000000000c21c5b1c787307a40e518f3fdf220879d66e3631a17dbcc5d6d7abef8f32b18b0d\
             f320af",
            "0600ff0706150000000000000000180000000000a4a3a2a009dc0b1d67cf94652213936fa07f95e2",
        ),
    ],
    privilege: "0600ff0706c0c21c5b1c03000000200029690f5b815886a3816b4b2b3440b1ceea91fd1bec61cf8fe1c9f9e4c8189131ffff020\
                79d8abf1537a00853f5ea2e3e339f224d",
};

/// We check RAKP and its sealing against two other implementations. The mock, with the BMC's random number, GUID and
/// session id, answers ipmitool's RAKP messages as the BMC did and opens its sealed request. So what logs in to the
/// mock, as ours does above, logs in to a BMC.
async fn answers_ipmitool_as(fixture: &str, login: Login) -> Result<()> {
    let bmc = Bmc::start("127.0.0.1:0", &mock::fixture(fixture)).await;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    socket.connect(("127.0.0.1", bmc.ipmi_port)).await?;

    let exchange = async |sent: &str| -> Result<Vec<u8>> {
        socket.send(&hex(sent)).await?;
        let mut answer = [0; 512];
        let length = tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut answer)).await??;
        Ok(answer[..length].to_vec())
    };

    exchange(login.open).await?;
    for (sent, answered) in login.rakp {
        assert_eq!(exchange(sent).await?, hex(answered));
    }
    exchange(login.privilege).await?;
    assert_eq!(asked(&bmc), ["IPMI 06 3b [04]"]);

    Ok(())
}

#[tokio::test]
async fn answers_ipmitool_as_ipmi_sim_did() -> Result<()> {
    answers_ipmitool_as("ipmi-sim", IPMI_SIM).await
}

#[tokio::test]
async fn answers_ipmitool_as_openbmc_did() -> Result<()> {
    answers_ipmitool_as("openbmc", OPENBMC).await
}

/// A session whose work is dropped half way, as `try_join!` does when the other half fails, still ends at the BMC.
#[tokio::test]
async fn closes_a_dropped_session() -> Result<()> {
    let bmc = x10().await;
    let target = bmc.target(PASSWORD);

    let stuck = ipmi::with_session(&target, async |session| {
        ipmi::chassis(session).await?;
        std::future::pending::<ooblin_core::Result<()>>().await
    });
    assert!(tokio::time::timeout(Duration::from_secs(1), stuck).await.is_err());

    tokio::time::timeout(Duration::from_secs(5), async {
        while !bmc.requests.lock().unwrap().iter().any(|asked| asked == "IPMI 06 3c [12, 04, 00, 00]") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    Ok(())
}

/// On the serial console what the server writes comes, what we type goes and is acknowledged, and closing deactivates
/// SOL before the session ends.
#[tokio::test]
async fn opens_the_serial_console() -> Result<()> {
    let bmc = x10().await;
    let sol = ipmi::Sol::open(&bmc.target(PASSWORD)).await?;
    let read = async |until: &str| -> Result<String> {
        let mut text = String::new();
        for _ in 0..5 {
            text += &String::from_utf8_lossy(&sol.read().await?);
            if text.contains(until) {
                return Ok(text);
            }
        }
        bail!("no {until:?} in {text:?}")
    };

    read("mock login: ").await?;
    sol.write(b"root\r").await?;
    assert_eq!(read("\r\n").await?, "root\r\n");

    // Typing waits for the BMC to take what was typed before, which `read` hears.
    tokio::time::timeout(Duration::from_secs(5), async {
        let typing = sol.write(b"ls\r");
        let reading = read("ls\r\n");
        let (typed, echoed) = tokio::join!(typing, reading);
        typed.and(echoed.map(drop).map_err(|error| Error::Protocol(error.to_string())))
    })
    .await??;
    sol.close().await;

    // We acknowledge each packet of the BMC's by its number and length: the login prompt and both echoes.
    assert_eq!(
        asked(&bmc),
        [
            "IPMI 06 48 [01, 01, c4, 00, 00, 00]",
            "SOL ack [01, 2d]",
            r#"SOL "root\r""#,
            "SOL ack [02, 06]",
            r#"SOL "ls\r""#,
            "SOL ack [03, 04]",
            "IPMI 06 49 [01, 01, 00, 00, 00, 00]",
            "IPMI 06 3c [12, 04, 00, 00]"
        ]
    );

    Ok(())
}

/// Opens the serial console and reads the login prompt.
async fn console(bmc: &Bmc) -> Result<ipmi::Sol> {
    let sol = ipmi::Sol::open(&bmc.target(PASSWORD)).await?;
    assert!(sol.read().await?.ends_with(b"login: "));
    Ok(sol)
}

/// A BMC sends a packet again until we acknowledge it, with what the server wrote meanwhile after it. So we read each
/// character once and acknowledge every time. When it says SOL is over, so do we.
#[tokio::test]
async fn reads_a_packet_sent_again_once() -> Result<()> {
    let bmc = x10().await;
    let sol = console(&bmc).await?;

    let packet = |text: &str| [&[9, 0, 0, 0][..], text.as_bytes()].concat();
    bmc.fault("SOL", Fault::Answered(vec![vec![0, 1, 1, 0], packet("abc"), packet("abc"), packet("abcde")]));
    sol.write(b"x").await?;
    assert_eq!((sol.read().await?, sol.read().await?), (b"abc".to_vec(), b"de".to_vec()));

    bmc.fault("SOL", Fault::Answered(vec![vec![0, 2, 1, 0x10]]));
    sol.write(b"y").await?;
    assert!(matches!(sol.read().await, Err(Error::Ended(_))));

    let acknowledged = ["SOL ack [09, 03]", "SOL ack [09, 03]", "SOL ack [09, 05]"];
    assert_eq!(asked(&bmc)[1..], [&["SOL ack [01, 2d]", r#"SOL "x""#][..], &acknowledged, &[r#"SOL "y""#]].concat());

    Ok(())
}

/// What the BMC took only part of goes on from there as a packet of its own. What it took none of, refused (NACK) or
/// not, goes again whole after a second.
#[tokio::test]
async fn types_again_what_the_bmc_did_not_take() -> Result<()> {
    let bmc = x10().await;
    let sol = console(&bmc).await?;
    let read = async |until: &str| -> Result<()> {
        let mut text = String::new();
        while !text.contains(until) {
            text += &String::from_utf8_lossy(&sol.read().await?);
        }
        Ok(())
    };

    bmc.fault("SOL", Fault::Answered(vec![vec![0, 1, 2, 0]]));
    sol.write(b"abcd").await?;
    read("cd").await?;

    for answer in [[0, 3, 0, 0x40], [0, 3, 0, 0], [0, 3, 1, 0]] {
        bmc.fault("SOL", Fault::Answered(vec![answer.to_vec()]));
    }
    sol.write(b"e").await?;

    // Typing on waits for the BMC to take it, which `read` hears.
    let typing = async { tokio::join!(sol.write(b"f"), read("f")) };
    let (typed, echoed) = tokio::time::timeout(Duration::from_secs(5), typing).await?;
    typed?;
    echoed?;

    let typed: Vec<_> = asked(&bmc).into_iter().filter(|asked| asked.starts_with("SOL \"")).collect();
    let again = [r#"SOL "e""#; 3];
    assert_eq!(typed, [&[r#"SOL "abcd""#, r#"SOL "cd""#][..], &again, &[r#"SOL "f""#]].concat());

    Ok(())
}

/// Logging in as IPMI says what Get Device ID does, which is the X10's firmware and no certificate.
#[tokio::test]
async fn logs_in_as_ipmi() -> Result<()> {
    let bmc = x10().await;
    let (client, info) = ooblin_core::Client::login(&bmc.target(PASSWORD), None, Some(Flavor::Ipmi)).await?;
    assert!(matches!(client, ooblin_core::Client::Ipmi(_)));
    assert_eq!((info.flavor, &*info.version, &*info.fingerprint), (Flavor::Ipmi, "4.07", ""));
    assert_eq!((info.remote_access.is_kvm, info.remote_access.is_sol), (None, true));

    Ok(())
}

/// We expect what IPMI says of the X10: Supermicro's board 0x086D, IPMI 2.0, its port as its LAN parameters say, its
/// sensors as the SDR and readings do, and its event log as Redfish words it too (`fixtures/x10/redfish`).
#[tokio::test]
async fn reads_what_the_x10_says_over_ipmi() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&bmc.target(PASSWORD));

    let status = client.status().await?;
    assert_eq!((&*status.manufacturer, status.product_id, &*status.ipmi_version), ("Supermicro", 0x086D, "2.0"));
    assert_eq!(status.power, PowerState::On);

    // Get System GUID says 433030314d535ca11fa0cbd500000000 and we swap its first three fields, as SMBIOS does.
    assert_eq!(status.system_id, "31303043-534D-A15C-1FA0-CBD500000000");

    let [port] = &*status.ports else { bail!("{} ports", status.ports.len()) };
    assert_eq!((&*port.mac, port.ipv4.configuration), ("5C:A1:1F:A0:CB:D5", Configuration::Dhcp));
    assert_eq!((&*port.ipv4.address, &*port.ipv4.gateway), ("192.0.2.224", "192.0.2.241"));

    // This is the X10's SLAAC address, which we scrubbed to the documentation prefix.
    assert_eq!(
        port.ipv6.as_ref().map(|ipv6| ipv6.addresses.clone()),
        Some(vec!["2001:db8::5ea1:1fff:fea0:cbd5".into()])
    );

    let sensors = client.sensors().await?;
    assert_eq!(sensors.len(), 20);
    let cpu = sensors.iter().find(|sensor| sensor.name == "CPU Temp").unwrap();
    assert_eq!((cpu.kind, &*cpu.unit, cpu.high), (SensorKind::Temperature, "°C", Some(96.0)));

    // A sensor of states with none asserted has no reading and is OK.
    let intrusion = sensors.iter().find(|sensor| sensor.name == "Chassis Intru").unwrap();
    assert_eq!((intrusion.reading, &*intrusion.state), (None, "OK"));

    let events = client.event_log().await?;
    assert_eq!(events.len(), 31);
    let hot = events.iter().find(|event| event.description == "Upper Critical going high, 96 °C ≥ 96 °C");
    assert_eq!(hot.map(|event| (&*event.source, event.severity)), Some(("CPU Temp", Severity::Critical)));

    let settings = client.settings().await?;
    let admin = settings.users.iter().find(|user| user.name == "ADMIN").unwrap();
    assert_eq!((&*admin.privilege, admin.is_enabled), ("Administrator", Some(true)));
    assert_eq!(settings.power_restore, Some(PowerRestore::Previous));

    Ok(())
}

/// We read the SDR repository in parts. When the BMC says our reservation is gone (0xC5) we reserve again and go on,
/// and when a part is more than it sends at once (0xCA) we ask for less.
#[tokio::test]
async fn reads_sensor_records_a_bmc_makes_hard_to_read() -> Result<()> {
    let bmc = x10().await;
    let part = |offset: u8, count: u8| format!("IPMI 0a 23 [14, 00, 00, 00, {offset:02x}, {count:02x}]");
    bmc.fault(&part(0x05, 0x10), Fault::Refused(0xCA));
    bmc.fault(&part(0x11, 0x0C), Fault::Refused(0xC5));
    assert_eq!(Client::new(&bmc.target(PASSWORD)).sensors().await?.len(), 20);

    let asked = asked(&bmc);
    let reads = [part(0x05, 0x10), part(0x05, 0x0C), part(0x11, 0x0C), "IPMI 0a 22 []".into(), part(0x11, 0x0C)];
    assert!(asked.windows(5).any(|window| window == reads), "{asked:?}");

    Ok(())
}

/// We read the FRU as the X10's own pages show it and then change it. Its product area grows into the room after it,
/// its board area stays, and an area it has none of cannot be set.
#[tokio::test]
async fn changes_the_fru() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&bmc.target(PASSWORD));

    let mut fru = client.fru().await?;
    let board = fru.board.clone().unwrap();
    assert_eq!((&*board.manufacturer, &*board.serial_number, fru.chassis.is_none()), ("Supermicro", "", true));

    let product = fru.product.as_mut().unwrap();
    (product.name, product.asset_tag) = ("Homelab".into(), "rack-2".into());
    client.change(Change::Fru(fru.clone())).await?;
    assert_eq!(client.fru().await?, fru);

    fru.chassis = Some(Default::default());
    match client.change(Change::Fru(fru)).await {
        Err(Error::Invalid(message)) => assert_eq!(message, "This FRU has no chassis area."),
        other => bail!("expected a refusal, got {other:?}"),
    }

    Ok(())
}

/// A FRU that ends before the size it says, with a refusal, still reads. And an area whose write fails half way gets
/// back what it held, because half of it would fail its checksum and we would leave that alone ever after.
#[tokio::test]
async fn copes_with_a_fru_that_refuses() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&bmc.target(PASSWORD));

    let read = client.fru().await?;
    bmc.fault("IPMI 0a 11 [00, e0, 00", Fault::Refused(0xC9));
    assert_eq!(client.fru().await?, read);

    let mut fru = read.clone();
    let product = fru.product.as_mut().unwrap();
    (product.name, product.asset_tag) = ("Homelab".into(), "rack-2".into());
    asked(&bmc);
    client.change(Change::Fru(fru.clone())).await?;
    let writes: Vec<_> = asked(&bmc).into_iter().filter(|asked| asked.starts_with("IPMI 0a 12")).collect();
    assert!(writes.len() > 1, "{writes:?}");

    // We make the same change to a BMC that refuses its second write, which we name by where it starts.
    let bmc = x10().await;
    let client = Client::new(&bmc.target(PASSWORD));
    bmc.fault(&writes[1][..22], Fault::Refused(0xFF));
    assert!(matches!(client.change(Change::Fru(fru)).await, Err(Error::Refused(0xFF))));
    assert_eq!(client.fru().await?, read);

    Ok(())
}

/// What IPMI does to the power is one Chassis Control that we send once, and the chassis says what came of it.
#[tokio::test]
async fn changes_the_power_over_ipmi() -> Result<()> {
    use PowerAction::*;
    let bmc = x10().await;
    let client = Client::new(&bmc.target(PASSWORD));

    let controls = [(Off, 0), (On, 1), (Cycle, 2), (Reset, 3), (Nmi, 4), (SoftOff, 5)];
    let boots = [(OnToBios, 1), (ResetToBios, 3), (OnToPxe, 1), (ResetToPxe, 3)];
    for (action, control) in controls.into_iter().chain(boots) {
        asked(&bmc);
        client.set_power(action).await?;
        let sent: Vec<_> = asked(&bmc).into_iter().filter(|asked| asked.starts_with("IPMI 00 02")).collect();
        assert_eq!(sent, [format!("IPMI 00 02 [{control:02x}]")], "{action:?}");
        match action {
            Off => assert_eq!(client.power().await?.state, PowerState::Off),
            On => assert_eq!(client.power().await?.state, PowerState::On),
            _ => {}
        }
    }

    assert!(matches!(client.set_power(Sleep).await, Err(Error::Unsupported)));

    Ok(())
}

/// We do not send a Chassis Control again to a BMC that does not answer it, because it may well have done it and would
/// then do it twice.
#[tokio::test]
async fn sends_a_chassis_control_once() -> Result<()> {
    let bmc = x10().await;
    bmc.fault("IPMI 00 02", Fault::Lost);
    let reset = Client::new(&bmc.target(PASSWORD)).set_power(PowerAction::Reset).await;
    assert!(matches!(reset, Err(Error::Timeout)), "{reset:?}");
    assert_eq!(asked(&bmc), ["IPMI 00 02 [03]"]);

    Ok(())
}

/// Booting once is a boot override, the way ipmitool does it, and then the power change.
#[tokio::test]
async fn boots_once_into_pxe() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&bmc.target(PASSWORD));
    assert!(client.power().await?.actions.contains(&PowerAction::ResetToPxe));

    asked(&bmc);
    client.set_power(PowerAction::ResetToPxe).await?;
    let asked = asked(&bmc);
    let sent = ["IPMI 00 08 [05, 80, 04, 00, 00, 00]", "IPMI 00 02 [03]"];
    let position = |request| asked.iter().position(|asked| asked == request).unwrap();
    assert!(position(sent[0]) < position(sent[1]), "{asked:?}");

    Ok(())
}

/// We read the network as IPMI's LAN parameters say it and change it the same way. There is no host name or DNS, as
/// IPMI has none of either.
#[tokio::test]
async fn changes_the_network() -> Result<()> {
    let bmc = x10().await;
    let client = Client::new(&bmc.target(PASSWORD));

    let network = client.settings().await?.network.unwrap();
    assert_eq!(
        (network.is_dhcp, &*network.address, network.vlan, &network.hostname),
        (true, "192.0.2.224", None, &None)
    );

    let mut network = Network { is_dhcp: false, address: "192.0.2.10".into(), vlan: Some(42), ..network };
    client.change(Change::Network(network.clone())).await?;
    assert_eq!(client.settings().await?.network, Some(network.clone()));

    // We set in progress, then its source, mask and gateway and its address last, as the BMC may answer no more once
    // it changes, then committed and complete. We set its VLAN only where it changes, as not every BMC has one.
    let set = |bmc: &Bmc| -> Vec<_> {
        asked(bmc).into_iter().filter_map(|asked| Some(asked.strip_prefix("IPMI 0c 01 [01, ")?.to_owned())).collect()
    };
    network.address = "192.0.2.11".into();
    asked(&bmc);
    client.change(Change::Network(network.clone())).await?;
    let sent = ["00, 01]", "04, 01]", "06, ff, ff, ff, 00]", "0c, c0, 00, 02, f1]", "03, c0, 00, 02, 0b]"];
    assert_eq!(set(&bmc), [&sent[..], &["00, 02]", "00, 00]"]].concat());

    // DHCP sets no address, mask or gateway, whatever they say.
    client.change(Change::Network(Network { is_dhcp: true, ..network.clone() })).await?;
    assert_eq!(set(&bmc), ["00, 01]", "04, 02]", "00, 02]", "00, 00]"]);

    network.vlan = Some(4095);
    assert!(matches!(client.change(Change::Network(network.clone())).await, Err(Error::Invalid(_))));
    (network.vlan, network.gateway) = (None, "the router".into());
    assert!(matches!(client.change(Change::Network(network)).await, Err(Error::Invalid(_))));

    Ok(())
}

/// We expect ipmi_sim's sensors, events, FRU, users and network as `ipmitool sdr elist`, `sensor list`, `sel elist`,
/// `fru print`, `user list` and `lan print` show them, in a session we raise once it is refused. It knows no SHA-256,
/// so we log in with SHA-1.
#[tokio::test]
async fn reads_ipmi_sim_as_ipmitool_does() -> Result<()> {
    let bmc = Bmc::start("127.0.0.1:0", &mock::fixture("ipmi-sim")).await;
    let client = Client::new(&bmc.target(PASSWORD));

    let sensors = client.sensors().await?;
    let sensors: Vec<_> = sensors.iter().map(|s| (&*s.name, s.reading, &*s.unit, &*s.state, s.low, s.high)).collect();
    assert_eq!(
        sensors,
        [
            ("CPU Temp", Some(45.0), "°C", "", None, Some(144.0)),
            ("FAN1", Some(2400.0), "RPM", "", Some(400.0), None),
            ("12V", Some(12.0), "V", "", Some(11.4), Some(12.6)),
            ("DIMM A1", None, "", "Present", None, None),
            ("CPU Status", None, "", "Presence detected", None, None),
        ]
    );

    let asked = asked(&bmc);
    assert!(asked.contains(&"IPMI 06 3b [04]".into()), "{asked:?}");
    assert_eq!(asked.last().map(String::as_str), Some("IPMI 06 3c [12, 04, 00, 00]"));

    // These are stamped before the BMC knew the time, which ipmitool shows as "Pre-Init".
    let events = client.event_log().await?;
    let events: Vec<_> = events.iter().map(|e| (e.occurred_at, &*e.source, &*e.description, e.severity)).collect();
    let present = (0, "CPU Status", "Presence detected", Severity::Info);
    assert_eq!(
        events,
        [
            (0, "OEM", "OEM record C1h, data 57 01 00 01 02 03 04 05 06", Severity::Info),
            (0, "System Firmware", "System Firmware Progress: System boot initiated", Severity::Info),
            present,
            (0, "FAN1", "Lower Critical going low, 300 RPM ≤ 400 RPM", Severity::Critical),
            (0, "CPU Temp", "Upper Critical going high deasserted, 110 °C < 144 °C", Severity::Warning),
            (0, "CPU Temp", "Upper Critical going high, 146 °C ≥ 144 °C", Severity::Critical),
            present,
            (0, "DIMM A1", "Absent deasserted", Severity::Critical),
            (0, "DIMM A1", "Present", Severity::Info),
        ]
    );

    let fru = client.fru().await?;
    let (chassis, board, product) = (fru.chassis.unwrap(), fru.board.unwrap(), fru.product.unwrap());
    assert_eq!((chassis.kind, &*chassis.part_number, &*chassis.serial_number), (0x17, "CSE-813M", "C813MLB12AB3456"));
    assert_eq!(
        (board.manufactured_at, &*board.manufacturer, &*board.product),
        (Some(1_699_963_200), "Supermicro", "X11SSH-F")
    );
    assert_eq!((&*board.serial_number, &*board.part_number), ("ZM1234567890", "MBD-X11SSH-F"));
    assert_eq!((&*product.name, &*product.part_number, &*product.version), ("SYS-5019S", "SYS-5019S-M", "1.0"));
    assert_eq!((&*product.serial_number, &*product.asset_tag), ("S123456789", "rack-7"));

    let settings = client.settings().await?;
    let users: Vec<_> = settings.users.iter().map(|u| (u.id, &*u.name, &*u.privilege, u.is_enabled)).collect();
    assert_eq!(users, [(2, "admin", "Administrator", None)]);
    let network = settings.network.unwrap();
    assert_eq!((network.is_dhcp, &*network.address, &*network.subnet_mask), (false, "192.0.2.50", "255.255.255.0"));
    assert_eq!((&*network.gateway, network.vlan), ("192.0.2.1", None));

    Ok(())
}

/// We expect OpenBMC with the server off as ipmitool shows it (`chassis status`, `sdr elist`, `sel elist`, `fru print`,
/// `user list`, `lan print`, scrubbed), with SHA-256 alone. All but three of its sensors are unread and it has two
/// events, no FRU, no manufacturer and no SOL settings. We read its identify light as off, although it says the state
/// is reserved until the light is first used.
#[tokio::test]
async fn reads_openbmc_as_ipmitool_does() -> Result<()> {
    let bmc = Bmc::start("127.0.0.1:0", &mock::fixture("openbmc")).await;
    let client = Client::new(&bmc.target(PASSWORD));

    let status = client.status().await?;
    assert_eq!((status.power, &*status.manufacturer, &*status.system_id), (PowerState::Off, "", ""));
    let chassis = ipmi::with_session(&bmc.target(PASSWORD), ipmi::chassis).await?;
    assert_eq!((chassis.is_identifying, chassis.restore), (Some(false), Some(PowerRestore::StayOff)));

    let sensors = client.sensors().await?;
    assert_eq!(sensors.len(), 135);
    let read: Vec<_> = sensors.iter().filter(|s| !s.state.is_empty()).map(|s| (&*s.name, &*s.state)).collect();
    assert_eq!(read, [("BootProgress", "OK"), ("AttemptsLeft", "State 0, State 1"), ("OperatingSystemS", "OK")]);
    assert!(sensors.iter().all(|sensor| sensor.reading.is_none()));

    let events = client.event_log().await?;
    let events: Vec<_> = events.iter().map(|e| (e.occurred_at, &*e.source, &*e.description, e.severity)).collect();
    let failure = |time| (time, "System Event", "Undetermined system hardware failure", Severity::Critical);
    assert_eq!(events, [failure(1_794_645_894), failure(1_794_645_886)]);

    assert_eq!(client.fru().await?, Fru::default());

    let settings = client.settings().await?;
    let users: Vec<_> = settings.users.iter().map(|u| (u.id, &*u.name, &*u.privilege, u.is_enabled)).collect();
    assert_eq!(users, [(1, "zpgu", "Administrator", Some(true))]);
    let network = settings.network.unwrap();
    assert_eq!(
        (network.is_dhcp, &*network.address, &*network.gateway, network.vlan),
        (true, "192.0.2.199", "192.0.2.55", None)
    );
    assert!(settings.sol.is_none());

    Ok(())
}

/// ipmi_sim before anything is in it has an empty SDR repository and SEL and no FRU 0, which ipmitool copes with ("SEL
/// has no entries", "Device not present"). A BMC without an SDR repository still has its event log.
#[tokio::test]
async fn copes_with_an_empty_bmc() -> Result<()> {
    let bmc = recorded("0a 22  -> 0900\n0a 10 00 -> ERR cc\n").await;
    let client = Client::new(&bmc.target(PASSWORD));
    assert!(client.sensors().await?.is_empty());
    assert!(client.event_log().await?.is_empty());
    assert_eq!(client.fru().await?, Fru::default());

    let bmc = recorded("").await;
    let client = Client::new(&bmc.target(PASSWORD));
    assert!(matches!(client.sensors().await, Err(Error::Refused(0xC1))));
    assert!(client.event_log().await?.is_empty());

    // A full sensor record that is cut off after two bytes is no sensor, rather than one that reads zero.
    let bmc = recorded("0a 22  -> 0100\n0a 23 0000000000ff -> ffff0000510102aabb\n").await;
    assert!(Client::new(&bmc.target(PASSWORD)).sensors().await?.is_empty());

    Ok(())
}
