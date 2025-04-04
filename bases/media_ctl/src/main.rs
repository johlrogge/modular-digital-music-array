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

    let client = media_client::MediaClient::connect("ipc:///tmp/mdma-commands")?;

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
            println!("Play channel {channel}");
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
        Commands::Unload { channel } => {
            let channel = commands::parse_channel(channel)?;
            client.unload_track(channel)?;
            println!(
                "Unloaded track from channel {}",
                commands::channel_to_string(channel)
            );
        }
        Commands::Seek { channel, position } => {
            let channel = commands::parse_channel(channel)?;
            let start = std::time::Instant::now();
            client.seek(channel, position)?;
            let duration = start.elapsed();
            println!(
                "Seeked channel {} to position {} in {:?}",
                commands::channel_to_string(channel),
                position,
                duration
            );
        }

        Commands::GetPosition { channel } => {
            let channel = commands::parse_channel(channel)?;
            let position = client.get_position(channel)?;

            // Present the position in a more human-readable way
            let sample_rate = 48000; // Assuming 48kHz
            let channels = 2; // Assuming stereo

            let seconds = position as f64 / (sample_rate as f64 * channels as f64);
            let minutes = (seconds / 60.0).floor();
            let seconds = seconds % 60.0;

            println!(
                "Position of channel {}: {} samples ({:02}:{:05.2})",
                commands::channel_to_string(channel),
                position,
                minutes as u32,
                seconds
            );
        }

        Commands::GetLength { channel } => {
            let channel = commands::parse_channel(channel)?;
            let length = client.get_length(channel)?;

            // Present the length in a more human-readable way
            let sample_rate = 48000; // Assuming 48kHz
            let channels = 2; // Assuming stereo

            let seconds = length as f64 / (sample_rate as f64 * channels as f64);
            let minutes = (seconds / 60.0).floor();
            let seconds = seconds % 60.0;

            println!(
                "Length of channel {}: {} samples ({:02}:{:05.2})",
                commands::channel_to_string(channel),
                length,
                minutes as u32,
                seconds
            );
        }
    }

    Ok(())
}
