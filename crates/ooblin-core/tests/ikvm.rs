use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use mock::{Bmc, Direction, PASSWORD, Transcript};
use ooblin_core::supermicro::{Client, Kvm};
use ooblin_core::{Error, Input};

async fn start(fixture: &Path) -> Arc<Bmc> {
    Bmc::start("127.0.0.1:0", fixture).await
}

fn client(bmc: &Bmc, password: &str) -> Client {
    Client::new(&bmc.target(password), None).unwrap()
}

fn requests(bmc: &Bmc) -> Vec<String> {
    bmc.requests.lock().unwrap().clone()
}

/// Returns what the client sent on the KVM's socket after the handshake, as hex.
fn sent(bmc: &Bmc) -> Vec<String> {
    requests(bmc).iter().filter_map(|line| line.strip_prefix("ikvm ")).map(str::to_owned).collect()
}

/// Returns a frame's size, its rectangles and their pixels one after the other.
fn parse(frame: &[u8]) -> ([u16; 2], Vec<[u16; 4]>, Vec<u8>) {
    let number = |offset: usize| u16::from_le_bytes([frame[offset], frame[offset + 1]]);

    let (mut rectangles, mut pixels, mut offset) = (Vec::new(), Vec::new(), 4);
    while offset < frame.len() {
        let rectangle = [0, 2, 4, 6].map(|i| number(offset + i));
        let length = 4 * usize::from(rectangle[2]) * usize::from(rectangle[3]);
        rectangles.push(rectangle);
        pixels.extend_from_slice(&frame[offset + 8..offset + 8 + length]);
        offset += 8 + length;
    }

    ([number(0), number(2)], rectangles, pixels)
}

/// Returns the rectangles of the X10's 1024 by 768 screen, which are 48 rows of 16 by 16 blocks.
fn rows() -> Vec<[u16; 4]> {
    (0..48).map(|row| [0, row * 16, 1024, 16]).collect()
}

/// Returns a fixture of a screen that answers the first requests with `first`, one each, and then the next with the
/// X10's first picture (in three messages).
fn fixture(first: &[&[u8]]) -> Result<tempfile::TempDir> {
    let Transcript(x10) = Transcript::load(&mock::fixture("x10").join("kvm.transcript"))?;
    let asked = (Direction::Sent, vec![]);
    let first = first.iter().flat_map(|answer| [asked.clone(), (Direction::Received, answer.to_vec())]);
    let records = first.chain([asked.clone()]).chain(x10[2..5].to_vec());

    let directory = tempfile::tempdir()?;
    Transcript(records.collect()).save(&directory.path().join("kvm.transcript"))?;

    Ok(directory)
}

/// Returns a KVM's frames until one says nothing changed.
async fn frames(bmc: &Bmc) -> Result<Vec<Vec<u8>>> {
    let kvm = client(bmc, PASSWORD).kvm().await?;

    let mut frames = vec![kvm.frame().await?];
    while frames.last().unwrap().len() > 4 {
        frames.push(kvm.frame().await?);
    }
    kvm.close().await;

    Ok(frames)
}

/// Returns a KVM's next frame that paints something.
async fn painted(kvm: &Kvm) -> Result<Vec<u8>> {
    loop {
        let frame = kvm.frame().await?;
        if frame.len() > 4 {
            return Ok(frame);
        }
    }
}

/// The socket must have closed before the web session logged out.
fn assert_closed(bmc: &Bmc) {
    let requests = requests(bmc);
    assert!(requests.join("\n").ends_with("WS closed\nGET /cgi/logout.cgi"), "{requests:#?}");
}

#[tokio::test]
async fn shows_the_x10s_screen() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    let opened = ["POST /cgi/login.cgi", "GET /cgi/url_redirect.cgi?url_name=man_ikvm_html5_bootstrap", "WS /"];
    assert_eq!(requests(&bmc), opened);

    // The whole screen comes as the BMC's own viewer paints it, and then once more as the new size asks for it.
    let whole = [kvm.frame().await?, kvm.frame().await?].map(|frame| parse(&frame));
    assert!(whole.iter().all(|(size, rectangles, _)| (*size, rectangles.clone()) == ([1024, 768], rows())));
    let hash = ::aws_lc_rs::digest::digest(&::aws_lc_rs::digest::SHA256, &whole[0].2)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(hash, "93e8a7f3344b8737a3abd3f7756108ead6409648874ca174f3f088afaa8762fa");

    // Then only the text cursor blinks, 12 times, until the recording ends and nothing changes for a second. That is a
    // second of its own and not the 20 a BMC that hung gets, however loaded this machine is.
    for _ in 0..12 {
        let (size, rectangles, _) = parse(&painted(&kvm).await?);
        assert_eq!((size, rectangles), ([1024, 768], vec![[0, 752, 16, 16]]));
    }
    let started = Instant::now();
    assert_eq!(parse(&kvm.frame().await?), ([1024, 768], vec![], vec![]));
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());

    // The first request is for the size the handshake said, and MouseSync goes once the first update says the real one.
    kvm.close().await;
    assert_closed(&bmc);
    let first = ["03000000000001e00280", "070780", "03000000000004000300", "03010000000004000300"];
    assert_eq!(sent(&bmc)[..4], first);
    assert_eq!(sent(&bmc).iter().filter(|message| *message == "070780").count(), 1);

    Ok(())
}

#[tokio::test]
async fn types_and_points_as_the_viewer_does() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;

    let key = |code: &str, is_down| Input::Key { code: code.into(), is_down };
    let pointer = |buttons, scroll| Input::Pointer { x: 300, y: 200, buttons, scroll };
    let keys = [("KeyA", true), ("KeyA", false), ("ShiftLeft", true), ("Digit1", true), ("Digit1", false)];
    let keys = keys.into_iter().chain([("ShiftLeft", false), ("MetaLeft", true), ("MetaLeft", false)]);
    let caps = [("CapsLock", true), ("CapsLock", false)];
    kvm.input(keys.chain(caps).chain(caps).map(|(code, is_down)| key(code, is_down)).collect()).await?;
    let clicks = [pointer(1, 0), pointer(0, 0), pointer(2, 0), pointer(0, 0), pointer(0, -1), pointer(0, 1)];
    kvm.input(clicks.into_iter().chain([Input::Text { text: "aB!\n".into() }]).collect()).await?;
    kvm.close().await;

    // ATEN's keys carry the USB usage, and Caps Lock goes down as 0xFF39 every other time.
    let key = |usage: u32, is_down| format!("0400{:02x}0000{usage:08x}{}", u8::from(is_down), "00".repeat(9));
    let pointer = |buttons: u8| format!("0500{buttons:02x}012c00c8{}", "00".repeat(11));
    let typed = |usage, has_shift| {
        let keys = [key(usage, true), key(usage, false)];
        if has_shift { [vec![key(225, true)], keys.into(), vec![key(225, false)]].concat() } else { keys.into() }
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

/// Nothing may go out on the KVM's socket but update requests, keys, the pointer and MouseSync.
#[tokio::test]
async fn sends_nothing_but_what_the_viewer_may() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;

    // We send every DOM code a webview may send, and some it does not.
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
        codes.iter().flat_map(|code| [true, false].map(|is_down| Input::Key { code: code.clone(), is_down })).collect();
    inputs.extend(
        (0..8).flat_map(|buttons| [-1, 0, 1].map(|scroll| Input::Pointer { x: 1023, y: 767, buttons, scroll })),
    );
    inputs.push(Input::Text { text: (0..=127u8).map(char::from).chain("éß€😀".chars()).collect() });
    inputs.extend([
        Input::Key { code: "ControlLeft".into(), is_down: true },
        Input::Pointer { x: 0, y: 0, buttons: 7, scroll: 0 },
    ]);
    inputs.push(Input::Release);

    let (framed, typed) = tokio::join!(async { kvm.frame().await.and(kvm.frame().await) }, kvm.input(inputs));
    framed.and(typed)?;
    kvm.close().await;

    // We expect the opening, what the mock lets through and the goodbye. So nothing forbidden went out, which the mock
    // would hang up on, and no `/vm`.
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
    kvm.close().await;

    // We close long before all of it is typed, which is a key down and up each and takes 10 seconds at the least. The
    // rest of it goes nowhere, as the socket is closed before we log out and stays so.
    let typed = sent(&bmc).len();
    assert!(typed < 2000, "{typed}");
    assert!(typing.await?.is_err());
    assert_closed(&bmc);
    assert_eq!(sent(&bmc).len(), typed);

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

    // The socket is gone but the goodbye is not.
    kvm.close().await;
    assert_closed(&bmc);

    Ok(())
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
    // The X10 says no signal twice, which is a size it cannot have and no picture, and then the first picture again.
    let dark = [&[0, 0, 0, 1, 0, 0, 0, 0, 0xFD, 0x80, 0xFE, 0x20, 0, 0, 0, 87][..], &[0; 8]].concat();
    let directory = fixture(&[&dark, &dark])?;
    let bmc = start(directory.path()).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;

    let started = Instant::now();
    let (size, rectangles, pixels) = parse(&kvm.frame().await?);
    assert_eq!((size, rectangles), ([480, 640], vec![[0, 0, 480, 640]]));
    assert!(pixels.chunks(4).all(|pixel| pixel == [0, 0, 0, 255]), "black");

    // We paint black once and then ask a second after each no-signal, as the viewer does. The picture comes back with
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
    // An empty fixture is a screen on which nothing changes, and every request is answered so.
    let directory = tempfile::tempdir()?;
    let bmc = start(directory.path()).await;
    let kvm = client(&bmc, PASSWORD).kvm().await?;
    for _ in 0..3 {
        assert_eq!(kvm.frame().await?.len(), 4);
    }

    // Once we stop asking the mock reads at most the request on its way and none queued behind it.
    let asked = sent(&bmc).len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sent(&bmc).len() <= asked + 1, "{} queued", sent(&bmc).len() - asked);
    kvm.close().await;

    Ok(())
}

#[tokio::test]
async fn drops_a_message_it_doesnt_know() -> Result<()> {
    let directory = fixture(&[&[0x41, 1, 2, 3]])?;
    let bmc = start(directory.path()).await;
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
    let directory = tempfile::tempdir()?;
    mock::ikvm::record(&x10.target(PASSWORD), directory.path()).await?;
    assert_closed(&x10);

    let recorded = start(directory.path()).await;
    let (x10, recorded) = (frames(&x10).await?, frames(&recorded).await?);
    assert_eq!(x10.len(), recorded.len());
    assert!(x10 == recorded, "the frames differ");

    Ok(())
}

/// Virtual media plugs a CD-ROM drive in with the KVM token, in the first free slot, and answers what the host asks
/// from the image, which reads on after its first block. When ejected it plugs the drive out and logs out of its web
/// session.
#[tokio::test]
async fn virtual_media_serves_the_image_until_ejected() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let image = tempfile::NamedTempFile::new()?;
    std::fs::write(image.path(), [[0; 2048], [1; 2048], [2; 2048]].concat())?;
    let media = client(&bmc, PASSWORD).media(std::fs::File::open(image.path())?).await?;

    let (reads, read) = (std::sync::Mutex::new(vec![]), tokio::sync::Notify::new());
    let serve = media.serve(
        |range| {
            reads.lock().unwrap().push(range);
            read.notify_one();
        },
        read.notified(),
    );
    tokio::time::timeout(Duration::from_secs(5), serve).await??;
    assert_eq!(reads.into_inner()?.first(), Some(&(2048..4096)));

    let requests = requests(&bmc);
    let media: Vec<_> = requests.iter().filter(|line| line.starts_with("vm ") || line.contains("/vm")).collect();
    let expected =
        ["WS /vm", "WS /vm", "vm 12 058004021f000000", "vm 25 0000000200000800", "vm 28 0101010101010101", "vm out"];
    assert_eq!(media, expected);
    assert_eq!(requests.last().unwrap(), "GET /cgi/logout.cgi");

    Ok(())
}

/// A drive the BMC refuses closes its socket again and logs out of its web session.
#[tokio::test]
async fn virtual_media_logs_out_when_refused() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let image = tempfile::NamedTempFile::new()?;
    std::fs::write(image.path(), [0; 2048])?;

    bmc.refuse_kvm();
    let refused = client(&bmc, PASSWORD).media(std::fs::File::open(image.path())?).await;
    let Err(Error::Protocol(message)) = refused else { bail!("the drive went in") };
    assert!(message.contains("refused the KVM token"), "{message}");
    assert_closed(&bmc);

    Ok(())
}

/// A drive that is ejected before the host is done with it goes out as at any other time.
#[tokio::test]
async fn virtual_media_ejects_at_once() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let image = tempfile::NamedTempFile::new()?;
    std::fs::write(image.path(), [0; 3 * 2048])?;

    let media = client(&bmc, PASSWORD).media(std::fs::File::open(image.path())?).await?;
    tokio::time::timeout(Duration::from_secs(5), media.serve(|_| {}, async {})).await??;
    let requests = requests(&bmc);
    assert!(requests.iter().any(|line| line == "vm out"), "{requests:?}");
    assert_eq!(requests.last().unwrap(), "GET /cgi/logout.cgi");

    // The drive goes out too where the BMC hangs up on us and never says so.
    bmc.hang_up_on_plug_out();
    let media = client(&bmc, PASSWORD).media(std::fs::File::open(image.path())?).await?;
    tokio::time::timeout(Duration::from_secs(5), media.serve(|_| {}, async {})).await??;

    Ok(())
}

/// A file too small to hold a block is no disc, so we say so and log in nowhere.
#[tokio::test]
async fn virtual_media_wants_a_disc() -> Result<()> {
    let bmc = start(&mock::fixture("x10")).await;
    let image = tempfile::NamedTempFile::new()?;
    std::fs::write(image.path(), [0; 2047])?;

    let refused = client(&bmc, PASSWORD).media(std::fs::File::open(image.path())?).await;
    assert!(matches!(refused, Err(Error::Invalid(_))));
    assert!(requests(&bmc).is_empty());

    Ok(())
}
