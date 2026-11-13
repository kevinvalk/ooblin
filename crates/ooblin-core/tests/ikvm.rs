//! A Supermicro BMC's KVM against our mock, which replays the X10's screen: the web session around it, the handshake,
//! the picture, and that nothing goes out but update requests, keys, the pointer and MouseSync.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use futures_util::{SinkExt, StreamExt};
use mock::{Bmc, Direction, PASSWORD, Transcript, USER};
use ooblin_core::supermicro::{Client, Kvm};
use ooblin_core::{Error, Input, Target};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

async fn start(fixture: &Path) -> Arc<Bmc> {
    Bmc::start("127.0.0.1:0", fixture).await
}

fn client(bmc: &Bmc, password: &str) -> Client {
    Client::new(&Target::new(format!("127.0.0.1:{}", bmc.port), USER, password), None).unwrap()
}

fn requests(bmc: &Bmc) -> Vec<String> {
    bmc.requests.lock().unwrap().clone()
}

/// What the client sent on the KVM's socket after the handshake, as hex.
fn sent(bmc: &Bmc) -> Vec<String> {
    requests(bmc).iter().filter_map(|line| line.strip_prefix("ikvm ")).map(str::to_owned).collect()
}

/// A frame's size, its rectangles and their pixels one after the other.
fn parse(frame: &[u8]) -> ([u16; 2], Vec<[u16; 4]>, Vec<u8>) {
    let number = |at: usize| u16::from_le_bytes([frame[at], frame[at + 1]]);
    let (mut rects, mut pixels, mut at) = (Vec::new(), Vec::new(), 4);
    while at < frame.len() {
        let rect = [0, 2, 4, 6].map(|i| number(at + i));
        let len = 4 * usize::from(rect[2]) * usize::from(rect[3]);
        rects.push(rect);
        pixels.extend_from_slice(&frame[at + 8..at + 8 + len]);
        at += 8 + len;
    }
    ([number(0), number(2)], rects, pixels)
}

/// The rectangles of the X10's 1024 by 768 screen in 48 rows of 16 by 16 blocks.
fn rows() -> Vec<[u16; 4]> {
    (0..48).map(|row| [0, row * 16, 1024, 16]).collect()
}

/// A fixture of a screen that answers the first requests with `first`, one each, then the next with the X10's first
/// picture (in three messages).
fn fixture(first: &[&[u8]]) -> Result<tempfile::TempDir> {
    let Transcript(x10) = Transcript::load(&mock::fixture("x10").join("kvm.transcript"))?;
    let asked = (Direction::Sent, vec![]);
    let first = first.iter().flat_map(|answer| [asked.clone(), (Direction::Received, answer.to_vec())]);
    let records = first.chain([asked.clone()]).chain(x10[2..5].to_vec());
    let dir = tempfile::tempdir()?;
    Transcript(records.collect()).save(&dir.path().join("kvm.transcript"))?;
    Ok(dir)
}

/// A KVM's frames until one says nothing changed.
async fn frames(bmc: &Bmc) -> Result<Vec<Vec<u8>>> {
    let kvm = client(bmc, PASSWORD).kvm().await?;
    let mut frames = vec![kvm.frame().await?];
    while frames.last().unwrap().len() > 4 {
        frames.push(kvm.frame().await?);
    }
    kvm.close().await;
    Ok(frames)
}

/// A KVM's next frame that paints something.
async fn painted(kvm: &Kvm) -> Result<Vec<u8>> {
    loop {
        let frame = kvm.frame().await?;
        if frame.len() > 4 {
            return Ok(frame);
        }
    }
}

#[tokio::test]
async fn shows_the_x10s_screen() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    let opened = ["POST /cgi/login.cgi", "GET /cgi/url_redirect.cgi?url_name=man_ikvm_html5_bootstrap", "WS /"];
    assert_eq!(requests(&bmc), opened);

    // The whole screen, as the BMC's own viewer paints it, then once more as the new size asks for it.
    let whole = [kvm.frame().await?, kvm.frame().await?].map(|frame| parse(&frame));
    assert!(whole.iter().all(|(size, rects, _)| (*size, rects.clone()) == ([1024, 768], rows())));
    let hash = ::aws_lc_rs::digest::digest(&::aws_lc_rs::digest::SHA256, &whole[0].2)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(hash, "93e8a7f3344b8737a3abd3f7756108ead6409648874ca174f3f088afaa8762fa");
    // Then only the text cursor blinking, until the recording ends and nothing changes for a second.
    let mut blinks = 0;
    loop {
        let started = Instant::now();
        let frame = kvm.frame().await?;
        let (size, rects, _) = parse(&frame);
        assert_eq!(size, [1024, 768]);
        if rects.is_empty() {
            assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
            break;
        }
        assert_eq!(rects, [[0, 752, 16, 16]]);
        blinks += 1;
    }
    assert_eq!(blinks, 12);

    // The first request for the size the handshake said, MouseSync once the first update says the real one.
    kvm.close().await;
    assert_closed(&bmc);
    let first = ["03000000000001e00280", "070780", "03000000000004000300", "03010000000004000300"];
    assert_eq!(sent(&bmc)[..4], first);
    assert_eq!(sent(&bmc).iter().filter(|message| *message == "070780").count(), 1);
    Ok(())
}

/// The socket closed, then the web session logged out.
fn assert_closed(bmc: &Bmc) {
    let requests = requests(bmc);
    assert!(requests.join("\n").ends_with("WS closed\nGET /cgi/logout.cgi"), "{requests:#?}");
}

#[tokio::test]
async fn types_and_points_as_the_viewer_does() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    let key = |code: &str, down| Input::Key { code: code.into(), down };
    let pointer = |buttons, scroll| Input::Pointer { x: 300, y: 200, buttons, scroll };
    let keys = [("KeyA", true), ("KeyA", false), ("ShiftLeft", true), ("Digit1", true), ("Digit1", false)];
    let keys = keys.into_iter().chain([("ShiftLeft", false), ("MetaLeft", true), ("MetaLeft", false)]);
    let caps = [("CapsLock", true), ("CapsLock", false)];
    kvm.input(keys.chain(caps).chain(caps).map(|(code, down)| key(code, down)).collect()).await?;
    let clicks = [pointer(1, 0), pointer(0, 0), pointer(2, 0), pointer(0, 0), pointer(0, -1), pointer(0, 1)];
    kvm.input(clicks.into_iter().chain([Input::Text { text: "aB!\n".into() }]).collect()).await?;
    kvm.close().await;

    // ATEN's keys carry the USB usage, Caps Lock going down as 0xFF39 every other time.
    let key = |usage: u32, down| format!("0400{:02x}0000{usage:08x}{}", u8::from(down), "00".repeat(9));
    let pointer = |buttons: u8| format!("0500{buttons:02x}012c00c8{}", "00".repeat(11));
    let typed = |usage, shift| {
        let keys = [key(usage, true), key(usage, false)];
        if shift { [vec![key(225, true)], keys.into(), vec![key(225, false)]].concat() } else { keys.into() }
    };
    let expected = [
        vec![key(4, true), key(4, false), key(225, true), key(30, true), key(30, false), key(225, false)],
        vec![key(227, true), key(227, false), key(0x39, true), key(0x39, false), key(0xFF39, true), key(0x39, false)],
        vec![pointer(1), pointer(0), pointer(4), pointer(0), pointer(8), pointer(0), pointer(16), pointer(0)],
        [typed(4, false), typed(5, true), typed(30, true), typed(40, false)].concat(),
    ];
    assert_eq!(sent(&bmc), expected.concat());
    assert_closed(&bmc);
    Ok(())
}

#[tokio::test]
async fn sends_nothing_but_what_the_viewer_may() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    // Every DOM code a webview may send, and some it doesn't.
    let named = "Backquote Backslash BracketLeft BracketRight Comma Equal IntlBackslash IntlRo IntlYen Minus Period \
        Quote Semicolon Slash AltLeft AltRight Backspace CapsLock ContextMenu ControlLeft ControlRight Enter MetaLeft \
        MetaRight ShiftLeft ShiftRight Space Tab Convert KanaMode Lang1 Lang2 NonConvert Delete End Help Home Insert \
        PageDown PageUp ArrowDown ArrowLeft ArrowRight ArrowUp NumLock NumpadAdd NumpadComma NumpadDecimal \
        NumpadDivide NumpadEnter NumpadEqual NumpadMultiply NumpadSubtract NumpadParenLeft Escape Fn PrintScreen \
        ScrollLock Pause AudioVolumeMute Power Unidentified";
    let codes = (b'A'..=b'Z').map(|letter| format!("Key{}", letter as char));
    let codes = codes.chain((0..10).flat_map(|digit| [format!("Digit{digit}"), format!("Numpad{digit}")]));
    let codes = codes.chain((1..=24).map(|n| format!("F{n}"))).chain(named.split_whitespace().map(str::to_owned));
    let codes: Vec<_> = codes.chain(["".into(), "\0".into()]).collect();
    let mut inputs: Vec<_> =
        codes.iter().flat_map(|code| [true, false].map(|down| Input::Key { code: code.clone(), down })).collect();
    inputs.extend(
        (0..8).flat_map(|buttons| [-1, 0, 1].map(|scroll| Input::Pointer { x: 1023, y: 767, buttons, scroll })),
    );
    inputs.push(Input::Text { text: (0..=127u8).map(char::from).chain("éß€😀".chars()).collect() });
    inputs.extend([
        Input::Key { code: "ControlLeft".into(), down: true },
        Input::Pointer { x: 0, y: 0, buttons: 7, scroll: 0 },
    ]);
    inputs.push(Input::Release);
    let (framed, typed) = tokio::join!(async { kvm.frame().await.and(kvm.frame().await) }, kvm.input(inputs));
    framed.and(typed)?;
    kvm.close().await;

    // Opening, what the mock lets through and the goodbye: nothing forbidden, which it would hang up on, and no `/vm`.
    let requests = requests(&bmc);
    assert_eq!(requests.len(), 3 + sent(&bmc).len() + 2, "{requests:#?}");
    assert_closed(&bmc);
    Ok(())
}

#[tokio::test]
async fn closing_cuts_a_paste_short() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = Arc::new(client(&bmc, PASSWORD).kvm().await?);
    let typing = tokio::spawn({
        let kvm = kvm.clone();
        async move { kvm.input(vec![Input::Text { text: "a".repeat(1000) }]).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    kvm.close().await;
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
    // The rest of it goes nowhere: the socket's closed before we log out, and stays so.
    assert!(typing.await?.is_err());
    assert_closed(&bmc);
    assert!(sent(&bmc).len() < 200, "{}", sent(&bmc).len());
    Ok(())
}

#[tokio::test]
async fn ends_with_its_web_session() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    kvm.frame().await?;
    bmc.log_out();
    let ended = async {
        loop {
            if let Err(error) = kvm.frame().await {
                break error;
            }
        }
    };
    let error = tokio::time::timeout(Duration::from_secs(5), ended).await?;
    assert!(matches!(error, Error::Ended("its web session was logged out")), "{error}");
    // The socket's gone, the goodbye isn't.
    kvm.close().await;
    assert_closed(&bmc);
    // Nor does its token, the mock's first, open another.
    let before = sent(&bmc).len();
    assert!(!shakes_hands(&bmc, &"0".repeat(24)).await?);
    assert_eq!(sent(&bmc).len(), before);
    Ok(())
}

/// Whether the mock's KVM takes `token` in ATEN's handshake.
async fn shakes_hands(bmc: &Bmc, token: &str) -> Result<bool> {
    let tls = ClientConfig::builder_with_provider(aws_lc_rs::default_provider().into());
    let tls = tls.with_safe_default_protocol_versions()?.dangerous().with_custom_certificate_verifier(Arc::new(Any));
    let tcp = TcpStream::connect(("127.0.0.1", bmc.port)).await?;
    let tls = TlsConnector::from(Arc::new(tls.with_no_client_auth()));
    let stream = tls.connect(ServerName::try_from("127.0.0.1")?, tcp).await?;
    let (mut socket, _) = client_async(format!("wss://127.0.0.1:{}/", bmc.port), stream).await?;
    // Its version, security types and challenge, each answered.
    for answer in [&b"RFB 055.008\n"[..], &[16], &[token.as_bytes(), &[0; 24]].concat()] {
        socket.next().await;
        socket.send(Message::binary(answer.to_vec())).await?;
    }
    Ok(matches!(socket.next().await, Some(Ok(Message::Binary(result))) if *result == [0; 4]))
}

/// The mock's certificate, whichever it is.
#[derive(Debug)]
struct Any;

impl ServerCertVerifier for Any {
    fn verify_server_cert(
        &self,
        _: &CertificateDer,
        _: &[CertificateDer],
        _: &ServerName,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        aws_lc_rs::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

#[tokio::test]
async fn logs_out_when_the_token_is_refused() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    bmc.refuse_kvm();
    let Err(Error::Transport(_)) = client(&bmc, PASSWORD).kvm().await else { bail!("the KVM opened") };
    assert_closed(&bmc);
    Ok(())
}

#[tokio::test]
async fn tries_a_wrong_password_once() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let mut client = client(&bmc, "wrong");
    for _ in 0..2 {
        let Err(Error::Unauthorized) = client.kvm().await else { bail!("the KVM opened") };
    }
    assert_eq!(requests(&bmc), ["POST /cgi/login.cgi"]);
    Ok(())
}

#[tokio::test]
async fn goes_dark_without_a_signal() -> Result<()> {
    // No signal twice, as the X10 says it: a size it can't have and no picture. Then the first picture again.
    let dark = [&[0, 0, 0, 1, 0, 0, 0, 0, 0xFD, 0x80, 0xFE, 0x20, 0, 0, 0, 87][..], &[0; 8]].concat();
    let dir = fixture(&[&dark, &dark])?;
    let bmc = start(dir.path()).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;

    let started = Instant::now();
    let (size, rects, pixels) = parse(&kvm.frame().await?);
    assert_eq!((size, rects), ([480, 640], vec![[0, 0, 480, 640]]));
    assert!(pixels.chunks(4).all(|pixel| pixel == [0, 0, 0, 255]), "black");
    // Black once, then we ask a second after each no-signal, as the viewer does, and the picture comes back with
    // MouseSync and a request for all of it.
    assert_eq!(parse(&painted(&kvm).await?).1, rows());
    assert!(started.elapsed() >= Duration::from_secs(2), "{:?}", started.elapsed());
    kvm.close().await;
    let asked =
        ["03000000000001e00280", "03010000000001e00280", "03010000000001e00280", "070780", "03000000000004000300"];
    assert_eq!(sent(&bmc), asked);
    Ok(())
}

#[tokio::test]
async fn keeps_one_request_on_its_way() -> Result<()> {
    // A screen on which nothing changes, every request answered so.
    let dir = tempfile::tempdir()?;
    let bmc = start(dir.path()).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    for _ in 0..3 {
        assert_eq!(kvm.frame().await?.len(), 4);
    }
    // Once we stop asking the mock reads at most the request on its way, none queued behind it.
    let asked = sent(&bmc).len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sent(&bmc).len() <= asked + 1, "{} queued", sent(&bmc).len() - asked);
    kvm.close().await;
    Ok(())
}

#[tokio::test]
async fn drops_a_message_it_doesnt_know() -> Result<()> {
    let dir = fixture(&[&[0x41, 1, 2, 3]])?;
    let bmc = start(dir.path()).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    // No update came for the request, so a second later we ask for all of it again, and the picture comes.
    let started = Instant::now();
    assert_eq!(parse(&painted(&kvm).await?).1, rows());
    assert!(started.elapsed() >= Duration::from_secs(1), "{:?}", started.elapsed());
    kvm.close().await;
    let asked = ["03000000000001e00280", "03000000000001e00280", "070780", "03000000000004000300"];
    assert_eq!(sent(&bmc), asked);
    Ok(())
}

#[tokio::test]
async fn records_a_session_that_replays_alike() -> Result<()> {
    let x10 = start(&mock::fixture("x10")).await;
    let dir = tempfile::tempdir()?;
    mock::ikvm::record(&Target::new(format!("127.0.0.1:{}", x10.port), USER, PASSWORD), dir.path()).await?;
    assert_closed(&x10);
    let recorded = start(dir.path()).await;
    let (x10, recorded) = (frames(&x10).await?, frames(&recorded).await?);
    assert_eq!(x10.len(), recorded.len());
    assert!(x10 == recorded, "the frames differ");
    Ok(())
}
