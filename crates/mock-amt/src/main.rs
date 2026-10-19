//! A stand-in Intel AMT for development, see the README.

use amt_core::Target;
use clap::Parser;
use mock_amt::{Amt, PASSWORD, USER};

#[derive(Parser)]
#[command(about = "A stand-in Intel AMT, replaying fixtures recorded from real ones")]
enum Command {
    /// Serve a fixture on 127.0.0.1: WS-Man on 16993, SOL, KVM and IDER on 16995, presence pings on UDP 623.
    Run { fixture: String },
    /// Record a device into a fixture, read-only: all its WS-Man data (scrubbed) and a short SOL, KVM and IDER session.
    Record {
        fixture: String,
        host: String,
        #[arg(long, default_value = "admin")]
        user: String,
        /// Only record the serial console, until you press Enter: make the device write to it meanwhile.
        #[arg(long)]
        sol: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Command::parse() {
        Command::Run { fixture } => {
            let dir = mock_amt::fixture(&fixture);
            Amt::start("127.0.0.1:16993", "127.0.0.1:16995", &dir).await;
            // Below 1024, which Linux keeps for root and macOS too unless it's on every address.
            if let Err(error) = mock_amt::answer_pings("0.0.0.0:623").await {
                eprintln!("Not answering presence pings on UDP 623: {error}");
            }
            println!("Serving {} on 127.0.0.1 as {USER} with password {PASSWORD}", dir.display());
            std::future::pending().await
        }
        Command::Record { fixture, host, user, sol } => {
            let password = rpassword::prompt_password(format!("Password for {user}@{host}: "))?;
            let (dir, target) = (mock_amt::fixture(&fixture), Target::new(&host, user, password));
            eprintln!("Recording {host} into {}", dir.display());
            match sol {
                true => mock_amt::record_sol(&target, &dir).await,
                false => mock_amt::record(&target, &dir).await,
            }
        }
    }
}
