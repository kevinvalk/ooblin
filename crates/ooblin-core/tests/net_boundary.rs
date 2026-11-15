//! Everything that touches the network lives in `ooblin-core/src/net/`: our egress surface, reviewable in one place.
//! The sources say so by what they name, the app's manifest by what it depends on.

use std::fs;
use std::path::Path;

#[test]
fn only_net_touches_the_network() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut offenders = Vec::new();
    for dir in ["ooblin-core/src", "ooblin/src"] {
        visit(&crates.join(dir), &mut offenders);
    }
    assert!(offenders.is_empty(), "network code outside ooblin-core/src/net: {offenders:#?}");
}

fn visit(dir: &Path, offenders: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit(&path, offenders);
        } else if !path.parent().unwrap().ends_with("ooblin-core/src/net") {
            let source = fs::read_to_string(&path).unwrap();
            // What opens a socket or looks a name up, however it's imported: addresses are no network.
            let network = [
                "reqwest",
                "rustls",
                "hyper",
                "tungstenite",
                "socket2",
                "TcpStream",
                "TcpSocket",
                "TcpListener",
                "UdpSocket",
                "lookup_host",
                "ToSocketAddrs",
            ];
            offenders.extend(
                network.iter().filter(|name| source.contains(*name)).map(|name| format!("{}: {name}", path.display())),
            );
        }
    }
}

/// The app's dependencies, none a way onto the network of its own but Tauri's, to our own window, and the verify
/// build's bridge. A name can hide in a source, not here: a new one is weighed before it joins them.
#[test]
fn the_app_depends_on_what_we_looked_at() {
    let looked_at = "clap futures-util keyring log ooblin-core rfd serde serde_json tauri tauri-plugin-log \
        tauri-plugin-mcp-bridge tauri-plugin-opener tauri-plugin-single-instance tauri-plugin-window-state tokio ts-rs";
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../ooblin/Cargo.toml")).unwrap();
    let dependencies = manifest.lines().skip_while(|line| *line != "[dependencies]").skip(1);
    let dependencies = dependencies.take_while(|line| !line.starts_with('['));
    let names: Vec<_> = dependencies.filter_map(|line| Some(line.split_once(" = ")?.0)).collect();
    assert_eq!(names, looked_at.split_whitespace().collect::<Vec<_>>());
}
