//! Everything that touches the network lives in `amt-core/src/net.rs`, so our egress surface is reviewable in one file.

use std::fs;
use std::path::Path;

#[test]
fn only_net_touches_the_network() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut offenders = Vec::new();
    for dir in ["amt-core/src", "amt-console/src"] {
        visit(&crates.join(dir), &mut offenders);
    }
    assert!(offenders.is_empty(), "network code outside net.rs: {offenders:#?}");
}

fn visit(dir: &Path, offenders: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit(&path, offenders);
        } else if !path.ends_with("amt-core/src/net.rs") {
            let source = fs::read_to_string(&path).unwrap();
            let network = ["reqwest", "rustls", "hyper", "tokio::net", "std::net"];
            offenders.extend(
                network
                    .iter()
                    .filter(|name| source.contains(*name))
                    .map(|name| format!("{}: {name}", path.display())),
            );
        }
    }
}
