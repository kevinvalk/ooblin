//! A stand-in Intel AMT for development, see the README.

use amt_core::Target;
use clap::Parser;
use mock_amt::{Amt, PASSWORD, USER};

#[derive(Parser)]
#[command(about = "A stand-in Intel AMT, replaying fixtures recorded from real ones")]
enum Command {
    /// Serve a fixture on 127.0.0.1: WS-Man on 16993, SOL, KVM and IDER on 16995.
    Run { fixture: String },
    /// Record a device into a fixture, read-only: all its WS-Man data (scrubbed) and a short SOL, KVM and IDER session.
    Record {
        fixture: String,
        host: String,
        #[arg(long, default_value = "admin")]
        user: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Command::parse() {
        Command::Run { fixture } => {
            let dir = mock_amt::fixture(&fixture);
            Amt::start("127.0.0.1:16993", "127.0.0.1:16995", &dir).await;
            println!("Serving {} on 127.0.0.1 as {USER} with password {PASSWORD}", dir.display());
            std::future::pending().await
        }
        Command::Record { fixture, host, user } => {
            let password = rpassword::prompt_password(format!("Password for {user}@{host}: "))?;
            let dir = mock_amt::fixture(&fixture);
            eprintln!("Recording {host} into {}", dir.display());
            mock_amt::record(&Target::new(&host, user, password), &dir).await
        }
    }
}
