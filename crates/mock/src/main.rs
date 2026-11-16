use clap::Parser;
use mock::{Amt, Bmc, PASSWORD, USER};
use ooblin_core::{Client, Flavor, Target};

#[derive(Parser)]
#[command(about = "A stand-in Intel AMT, Supermicro or IPMI-only BMC, replaying fixtures recorded from real ones")]
enum Command {
    /// Serve a fixture on 127.0.0.1: an AMT's WS-Man on 16993, SOL, KVM and IDER on 16995, a BMC's web port on 16443,
    /// presence pings on UDP 623, where a BMC answers IPMI too.
    Run { fixture: String },
    /// Record a device into a fixture, read-only: an AMT's WS-Man data (scrubbed) and a short SOL, KVM and IDER
    /// session, a Supermicro's Redfish and IPMI (scrubbed) and a short KVM session, another BMC's IPMI, or its
    /// Redfish where it has IPMI off.
    Record {
        fixture: String,
        host: String,
        #[arg(long, default_value = "admin")]
        user: String,
        /// Only record AMT's serial console, until you press Enter: make the device write to it meanwhile.
        #[arg(long)]
        sol: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Command::parse() {
        Command::Run { fixture } => {
            let directory = mock::fixture(&fixture);
            let flavor = mock::flavor(&directory);
            let (address, bmc) = match flavor {
                Flavor::Amt => {
                    Amt::start("127.0.0.1:16993", "127.0.0.1:16995", &directory).await;
                    ("127.0.0.1", None)
                }
                Flavor::Supermicro | Flavor::Ipmi | Flavor::Redfish => {
                    ("127.0.0.1:16443", Some(Bmc::start("127.0.0.1:16443", &directory).await))
                }
            };

            // NOTE: Port 623 is below 1024, which Linux keeps for root, and macOS too unless we bind it on every
            // address. A BMC answers IPMI there as well.
            let udp = match &bmc {
                Some(bmc) => bmc.serve_ipmi("0.0.0.0:623").await,
                None => mock::answer_pings("0.0.0.0:623").await.map(drop),
            };
            if let Err(error) = udp {
                eprintln!("Not answering on UDP 623: {error}");
            }

            println!("Serving {} on {address} as {USER} with password {PASSWORD}", directory.display());
            let Some(bmc) = bmc else { return std::future::pending().await };

            // We print what the BMC is asked as it is, the keys and the pointer of the KVM among it.
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                for request in bmc.asked() {
                    println!("{request}");
                }
            }
        }
        Command::Record { fixture, host, user, sol } => {
            let password = rpassword::prompt_password(format!("Password for {user}@{host}: "))?;
            let (directory, target) = (mock::fixture(&fixture), Target::new(&host, user, password));
            eprintln!("Recording {host} into {}", directory.display());

            if sol {
                return mock::record_sol(&target, &directory).await;
            }

            match Client::login(&target, None, None).await?.1.flavor {
                Flavor::Amt => mock::record(&target, &directory).await,
                Flavor::Supermicro => {
                    mock::redfish::record(&target, &directory, Flavor::Supermicro).await?;
                    mock::ipmi::record(&target, &directory).await?;
                    mock::ikvm::record(&target, &directory).await
                }
                Flavor::Ipmi => mock::ipmi::record(&target, &directory).await,
                Flavor::Redfish => mock::redfish::record(&target, &directory, Flavor::Redfish).await,
            }
        }
    }
}
