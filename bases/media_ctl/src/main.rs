use clap::Parser;
use color_eyre::Result;
mod commands;

use commands::Commands;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let mut client = media_client::MediaClient::connect("ipc:///tmp/mdma-commands")?;

    match cli.command {
        Commands::Load {
            library,
            artist,
            album,
            song,
            channel,
        } => {
            let channel = commands::parse_channel(channel)?;
            let path = commands::construct_path(library, artist, album, song)?;
            client.load_track(path, channel)?;
            println!(
                "Track loaded on channel {}",
                commands::channel_to_string(channel)
            );
        }

        Commands::Play { channel } => {
            let channel = commands::parse_channel(channel)?;
            client.play(channel)?;
            println!("Playing channel {}", commands::channel_to_string(channel));
        }

        Commands::Stop { channel } => {
            let channel = commands::parse_channel(channel)?;
            client.stop(channel)?;
            println!("Stopped channel {}", commands::channel_to_string(channel));
        }

        Commands::Volume { channel, db } => {
            let channel = commands::parse_channel(channel)?;
            client.set_volume(channel, db)?;
            println!(
                "Set volume of channel {} to {}dB",
                commands::channel_to_string(channel),
                db
            );
        }
    }

    Ok(())
}
