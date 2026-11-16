use std::fs;
use std::path::Path;

fn visit(directory: &Path, offenders: &mut Vec<String>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit(&path, offenders);
        } else if !path.parent().unwrap().ends_with("ooblin-core/src/net") {
            let source = fs::read_to_string(&path).unwrap();

            // We look for what opens a socket or looks a name up, however it is imported. Addresses are no network.
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

/// IMPORTANT: Everything that touches the network lives in `ooblin-core/src/net/`, so that our egress surface can be
/// reviewed in one place. The sources say so by what they name.
#[test]
fn only_net_touches_the_network() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();

    let mut offenders = Vec::new();
    for directory in ["ooblin-core/src", "ooblin/src"] {
        visit(&crates.join(directory), &mut offenders);
    }

    assert!(offenders.is_empty(), "network code outside ooblin-core/src/net: {offenders:#?}");
}

/// The app's manifest says the same by what it depends on. None of its dependencies is a way onto the network of its
/// own, but Tauri's to our own window, the verify build's bridge and the updater's to our releases on GitHub, which
/// only takes what our key signed. reqwest is there only for the updater's TLS, the app never calls it. A name can hide
/// in a source but not here, so we weigh a new one before it joins them.
#[test]
fn the_app_depends_on_what_we_looked_at() {
    let looked_at = "clap futures-util keyring log ooblin-core reqwest rfd serde serde_json tauri tauri-plugin-log \
        tauri-plugin-mcp-bridge tauri-plugin-opener tauri-plugin-single-instance tauri-plugin-updater \
        tauri-plugin-window-state tokio ts-rs";
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../ooblin/Cargo.toml")).unwrap();

    let dependencies = manifest.lines().skip_while(|line| *line != "[dependencies]").skip(1);
    let dependencies = dependencies.take_while(|line| !line.starts_with('['));
    let names: Vec<_> = dependencies.filter_map(|line| Some(line.split_once(" = ")?.0)).collect();

    assert_eq!(names, looked_at.split_whitespace().collect::<Vec<_>>());
}
