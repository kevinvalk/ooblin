//! Devices that say something else than the ones we recorded: every fixture with a few of its files garbled, replayed
//! by the mock, as each flavor it can be logged in to as. Whatever a device says, we answer with an error or with what we could read, never with a panic and
//! never by waiting forever. `GARBLED=500 cargo test --test garbled` tries more of them than a push needs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mock::{Amt, Bmc, PASSWORD};
use ooblin_core::{Client, Flavor};

/// Numbers that follow from their seed alone (xorshift), so a round that fails can be had again.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, most: usize) -> usize {
        (self.next() % most.max(1) as u64) as usize
    }
}

/// Bytes as a device with a fault, or another firmware, might send them: cut short, blank, a few changed, more after
/// them, or something else altogether.
fn garble(random: &mut Random, bytes: &mut Vec<u8>) {
    match random.below(6) {
        0 => bytes.truncate(random.below(bytes.len() + 1)),
        1 => bytes.fill(0xFF),
        2 => bytes.fill(0),
        3 => {
            for _ in 0..=random.below(6) {
                let at = random.below(bytes.len());
                if let Some(byte) = bytes.get_mut(at) {
                    *byte = random.next() as u8;
                }
            }
        }
        4 => bytes.extend((0..random.below(64)).map(|_| random.next() as u8)),
        _ => *bytes = (0..random.below(200)).map(|_| random.next() as u8).collect(),
    }
}

/// A value in XML or JSON swapped for one a parser might trip over.
fn garble_value(random: &mut Random, text: &mut Vec<u8>) {
    const VALUES: [&str; 8] = ["", "0", "-1", "4294967295", "99999999999999999999999999", "null", "\u{0}", "é😀"];
    let starts = (1..text.len()).filter(|&at| b">\": ".contains(&text[at - 1]) && text[at].is_ascii_alphanumeric());
    let starts: Vec<usize> = starts.collect();
    let Some(&start) = starts.get(random.below(starts.len())) else { return };
    let end = (start..text.len()).find(|&at| b"<\",\n}".contains(&text[at])).unwrap_or(text.len());
    text.splice(start..end, VALUES[random.below(VALUES.len())].bytes());
}

/// Garbles what the device says in the file at `path`: an answer of a recording, a message of a session, or a page.
fn garble_file(random: &mut Random, path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    match path.extension().and_then(|extension| extension.to_str()) {
        // Records of a direction, a length and bytes (see `mock::Transcript`): what the device sent.
        Some("transcript") => {
            let (mut garbled, mut rest) = (Vec::new(), &bytes[..]);
            while let [direction, a, b, c, d, tail @ ..] = rest {
                let (message, tail) = tail.split_at(u32::from_le_bytes([*a, *b, *c, *d]) as usize);
                let mut message = message.to_vec();
                if *direction == b'<' && random.below(8) == 0 {
                    garble(random, &mut message);
                }
                garbled.extend([&[*direction][..], &(message.len() as u32).to_le_bytes(), &message].concat());
                rest = tail;
            }
            bytes = garbled;
        }
        // Lines of `asked -> answered` in hex (see `mock::ipmi`): one answer.
        Some("txt") => {
            let text = String::from_utf8(bytes).unwrap();
            let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
            let answers = (0..lines.len()).filter(|&at| lines[at].starts_with(|first: char| first.is_ascii_hexdigit()));
            let answers: Vec<usize> = answers.collect();
            let line = &mut lines[answers[random.below(answers.len())]];
            let (asked, answer) = line.split_once(" -> ").unwrap();
            let mut answer = if answer.starts_with("ERR") { Vec::new() } else { mock::hex(answer) };
            garble(random, &mut answer);
            *line = format!("{asked} -> {}", answer.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
            bytes = lines.join("\n").into_bytes();
        }
        _ if random.below(2) == 0 => garble_value(random, &mut bytes),
        _ => garble(random, &mut bytes),
    }
    // Not through the link, which is the fixture's own file.
    std::fs::remove_file(path).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// `fixture` in a directory of its own, as links: a round garbles a few files of hundreds.
fn copy(fixture: &Path, to: &Path, files: &mut Vec<PathBuf>) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(fixture).unwrap() {
        let (from, to) = (entry.as_ref().unwrap().path(), to.join(entry.unwrap().file_name()));
        if from.is_dir() {
            copy(&from, &to, files);
        } else {
            std::fs::hard_link(&from, &to).or_else(|_| std::fs::copy(&from, &to).map(drop)).unwrap();
            files.push(to);
        }
    }
}

/// What `work` comes to, unless it takes over `seconds`: a session waits for its device as long as the user does.
async fn within<T, E>(seconds: u64, work: impl Future<Output = Result<T, E>>) -> Option<T> {
    tokio::time::timeout(Duration::from_secs(seconds), work).await.ok()?.ok()
}

/// Everything we read of the device in `fixture`, and each session opened, used and closed. What fails, fails.
async fn read(fixture: &Path, flavor: Flavor) {
    let (_amt, _bmc, target);
    if flavor == Flavor::Amt {
        let amt = Amt::start("127.0.0.1:0", "127.0.0.1:0", fixture).await;
        (target, _amt, _bmc) = (amt.target(PASSWORD), Some(amt), None);
    } else {
        let bmc = Bmc::start("127.0.0.1:0", fixture).await;
        (target, _amt, _bmc) = (bmc.target(PASSWORD), None, Some(bmc));
    }
    let Ok((mut client, _)) = Client::login(&target, None, Some(flavor)).await else { return };

    let _ = client.power().await;
    let _ = client.hardware().await;
    let _ = client.event_log().await;
    let _ = client.sensors().await;
    let _ = client.panel().await;
    let _ = client.bmc_status().await;
    if let Ok(amt) = client.amt() {
        let _ = amt.status().await;
        let _ = amt.settings().await;
        let _ = amt.audit_log().await;
        let _ = amt.screens().await;
    }
    if let Ok(supermicro) = client.supermicro() {
        let _ = supermicro.status().await;
        let _ = supermicro.bmc_settings().await;
    }
    if let Ok(ipmi) = client.ipmi() {
        let _ = ipmi.status().await;
        let _ = ipmi.settings().await;
    }

    if let Some(kvm) = within(5, client.kvm()).await {
        // A frame of a size alone is a quiet second: the recording is through.
        while within(2, kvm.frame()).await.is_some_and(|frame| frame.len() > 4) {}
        kvm.close().await;
    }
    if let Some(sol) = within(5, client.sol()).await {
        within(2, sol.read()).await;
        within(2, sol.write(b"ls\r")).await;
        sol.close().await;
    }
    let image = fixture.join("disc.iso");
    std::fs::write(&image, [0x5A; 2048 * 8]).unwrap();
    if let Some(media) = within(5, client.mount(std::fs::File::open(image).unwrap())).await {
        within(10, media.serve(|_| {}, tokio::time::sleep(Duration::from_secs(1)))).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn survives_what_a_device_says() {
    let rounds: u64 = std::env::var("GARBLED").map_or(8, |rounds| rounds.parse().unwrap());
    let mut reads = tokio::task::JoinSet::new();
    for fixture in mock::fixtures().filter(|fixture| !fixture.ends_with("go-wsman-messages")) {
        for (flavor, round) in
            mock::flavors(&fixture).into_iter().flat_map(|flavor| (0..rounds).map(move |round| (flavor, round)))
        {
            let fixture = fixture.clone();
            reads.spawn(async move {
                let mut random = Random(round.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
                let (dir, mut files) = (tempfile::tempdir().unwrap(), Vec::new());
                copy(&fixture, dir.path(), &mut files);
                files.sort();

                // Sessions and recordings are few among hundreds of pages: every other file is one of them.
                let (sessions, pages): (Vec<_>, Vec<_>) = files.iter().partition(|file| {
                    file.extension().is_some_and(|kind| !kind.eq_ignore_ascii_case("xml") && kind != "json")
                });
                let mut garbled = Vec::new();
                for _ in 0..=random.below(3) {
                    let files = if random.below(2) == 0 || pages.is_empty() { &sessions } else { &pages };
                    if let Some(file) = files.get(random.below(files.len())) {
                        garble_file(&mut random, file);
                        garbled.push(file.strip_prefix(dir.path()).unwrap());
                    }
                }

                let name = format!("round {round} of {} as {flavor:?}, garbled {garbled:?}", fixture.display());
                let reading = tokio::spawn(async move { read(dir.path(), flavor).await });
                match tokio::time::timeout(Duration::from_secs(120), reading).await {
                    Ok(read) => read.unwrap_or_else(|panic| panic!("{name}: {panic}")),
                    Err(_) => panic!("{name}: still waiting after two minutes"),
                }
            });
        }
    }
    while let Some(read) = reads.join_next().await {
        read.unwrap();
    }
}
