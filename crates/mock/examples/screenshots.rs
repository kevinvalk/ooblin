use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::ensure;
use mock::{Amt, Bmc, PASSWORD};

/// Stops the app and the driver's daemon however we end, as both outlive us otherwise.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = drive(&["daemon", "stop"]);
    }
}

/// Runs a command of the driver of AGENTS.md (`tauri-mcp`) and says whether it went well.
fn drive(arguments: &[&str]) -> anyhow::Result<bool> {
    let mut driver = Command::new("npx");
    driver.args(["--yes", "--package=@hypothesi/tauri-mcp-cli@0.13", "tauri-mcp"]).args(arguments);
    Ok(driver.stdout(Stdio::null()).stderr(Stdio::null()).status()?.success())
}

fn must_drive(arguments: &[&str]) -> anyhow::Result<()> {
    ensure!(drive(arguments)?, "tauri-mcp {arguments:?} failed");
    Ok(())
}

/// Takes docs/screenshots/*.png of the verify build against the mock devices, on macOS: `cargo screenshots`.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()?;
    ensure!(Command::new(env!("CARGO")).arg("verify").current_dir(&root).status()?.success(), "cargo verify failed");

    // We serve the devices as `mock run` does, the X10 with IPMI on UDP 623.
    let x10 = Bmc::start("127.0.0.1:16443", &mock::fixture("x10")).await;
    x10.serve_ipmi("0.0.0.0:623").await?;
    let _ms_02 = Amt::start("127.0.0.1:16993", "127.0.0.1:16995", &mock::fixture("ms-02")).await;

    // The app gets a HOME of its own, so it starts without the devices the verify build saved before, yours among them.
    // The verify build picks the disk image `OOBLIN_PICK` names, which the X10 reads on through for a while.
    let home = tempfile::tempdir()?;
    let image = home.path().join("disc.iso");
    std::fs::write(&image, vec![0x5A; 48 << 20])?;
    let app = |host: &str| {
        let mut app = Command::new(root.join("target/debug/ooblin"));
        app.args([host, "--password", PASSWORD]).env("HOME", home.path()).env("OOBLIN_PICK", &image);
        app.env("TAURI_MCP_NO_FOREGROUND", "1").stdout(Stdio::null()).stderr(Stdio::null());
        app
    };
    let _running = Running(app("127.0.0.1:16443").spawn()?);
    while !drive(&["driver-session", "start", "--port=9223"])? {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Each device keeps its own tabs and buttons in the page, so we click those of the one on screen, by their text.
    let click = |selector: &str, text: &str| {
        must_drive(&["webview-execute-js", &format!(
            "--script=(() => {{
                const section = [...document.querySelectorAll('section')].find((section) => !section.hidden && section.querySelector('[role=tab]'));
                [...section.querySelectorAll('{selector}')].find((element) => element.textContent.trim() === '{text}').click();
            }})()"
        )])
    };
    let show = |tab: &str| click("[role=tab]", tab);
    let wait_for = |kind: &str, value: &str| {
        must_drive(&["webview-wait-for", &format!("--type={kind}"), &format!("--value={value}"), "--timeout=30000"])
    };
    let shoot = |name: &str| {
        let path: PathBuf = root.join("docs/screenshots").join(format!("{name}.png"));
        must_drive(&["webview-screenshot", "--format=png", &format!("--file-path={}", path.display())])
    };
    let read_and_shoot = |tab: &str, name: &str| {
        show(tab)?;
        wait_for("selector", "section:not([hidden]) [role=tablist] button.btn-secondary:not([disabled])")?; // Refresh, once read
        shoot(name)
    };

    read_and_shoot("Sensors", "sensors")?;

    // The footer maps what the X10 read of the image, as it reads on through it for a while.
    click("button", "Disk image…")?;
    wait_for("selector", "footer div[title^=\"Disk read\"]")?;
    show("Status")?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    shoot("disk-image")?;

    // The MS-02, handed to the app, which shows it.
    ensure!(app("localhost").status()?.success(), "the app did not take the MS-02");
    wait_for("text", "Audit log")?;
    read_and_shoot("Hardware", "hardware")?;
    read_and_shoot("Settings", "settings")?;

    // The footer says the size of the screen once its first frame is in, and a few more fill it in.
    show("KVM")?;
    wait_for("selector", "footer span[title^=\"The device\"]")?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    shoot("kvm")
}
