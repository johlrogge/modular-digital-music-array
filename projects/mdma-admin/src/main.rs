mod service;

use clap::Parser;
use color_eyre::Result;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA Admin — privileged system-level operations service"
)]
struct Args {
    #[arg(long, default_value = "ipc:///run/mdma/admin.sock")]
    socket: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Rewrite an fstab (stdin) so the boot partition mounts at /boot instead
    /// of /boot/firmware (issue #65), printing the result to stdout.
    /// Used by /usr/libexec/mdma/migrate-bootfs. Already-migrated input is
    /// printed unchanged.
    MigrateFstab {
        /// PARTUUID of the boot partition (p1)
        #[arg(long)]
        partuuid: String,
    },
}

fn migrate_fstab_stdio(partuuid: &str) -> Result<()> {
    use std::io::{Read, Write};
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let output = boot_health::migrate_fstab(&input, partuuid)?;
    std::io::stdout().write_all(output.as_bytes())?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let args = Args::parse();

    if let Some(Command::MigrateFstab { partuuid }) = &args.command {
        return migrate_fstab_stdio(partuuid);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_admin=info".into()),
        )
        .init();

    tracing::info!(socket = %args.socket, "Starting MDMA Admin service");

    tokio::task::spawn_blocking(move || service::run(&args.socket))
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Service task panicked: {e}"))??;

    Ok(())
}
