use crate::args::impl_arg;
use crate::ctx::Ctx;
use color_eyre::Result;
use colored::Colorize;
use event_protocol::{from_topic_message, PlaybackEvent, TOPIC_PLAYBACK};
use mdma_client::LibraryBackend;
use nng::options::Options;
use track_formatter::short_hash;

#[derive(clap::Args, Debug)]
pub(crate) struct SubscribeCli {
    /// Topic filter (e.g. "playback/track_started"). Default: all playback events.
    #[arg(long)]
    pub(crate) topic: Option<String>,
}

/// Topic filter for events (all playback events when `None`).
// Stays a String: a topic is a free-form subscription prefix with no domain rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptTopicArg(pub Option<String>);

impl_arg!(
    OptTopicArg,
    optional_text long "topic",
    "TOPIC",
    "Topic filter (e.g. \"playback/track_started\"). Default: all playback events."
);

/// Subscribe to real-time events from the playback system
#[hicli::command]
pub(crate) fn subscribe(topic: OptTopicArg, ctx: &Ctx) -> Result<()> {
    let OptTopicArg(topic) = topic;
    // Event gateway of the resolved node (tcp://<ip>:5556)
    let addr = match ctx.event_gateway() {
        Some(a) => a,
        None => {
            eprintln!("Subscribe needs a remote node.");
            eprintln!("Set --node or MDMA_NODE to the hostname of your MDMA device");
            std::process::exit(1);
        }
    };
    let library = ctx.library();
    listen(&addr, topic.as_deref(), Some(&library))
}

fn listen(
    event_gateway: &str,
    topic: Option<&str>,
    library: Option<&LibraryBackend>,
) -> Result<()> {
    use std::io::IsTerminal;

    let socket = nng::Socket::new(nng::Protocol::Sub0)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to create Sub0 socket: {}", e))?;

    // Subscribe to the requested topic prefix, or all playback events
    let sub_topic = topic.unwrap_or(TOPIC_PLAYBACK);
    socket
        .set_opt::<nng::options::protocol::pubsub::Subscribe>(sub_topic.as_bytes().to_vec())
        .map_err(|e| color_eyre::eyre::eyre!("Failed to set subscription: {}", e))?;

    // Resolve hostname for .local mDNS addresses
    let resolved = nng_transport::resolve_tcp_hostname(event_gateway)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to resolve address: {}", e))?;

    socket
        .dial(&resolved)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to {}: {}", event_gateway, e))?;

    let is_tty = std::io::stdout().is_terminal();

    if is_tty {
        eprintln!("Subscribed to {} (topic: {})", event_gateway, sub_topic);
        eprintln!("Waiting for events... (Ctrl-C to stop)");
    }

    loop {
        let msg = socket
            .recv()
            .map_err(|e| color_eyre::eyre::eyre!("Receive error: {}", e))?;

        match from_topic_message(msg.as_slice()) {
            Ok((_topic, event)) => {
                if is_tty {
                    print_event_human(&event, library);
                } else {
                    // Pipe mode: raw JSON, one line per event
                    println!(
                        "{}",
                        serde_json::to_string(&event).unwrap_or_else(|_| format!("{:?}", event))
                    );
                }
            }
            Err(e) => {
                eprintln!("Failed to parse event: {}", e);
            }
        }
    }
}

fn print_event_human(event: &PlaybackEvent, library: Option<&LibraryBackend>) {
    match event {
        PlaybackEvent::TrackStarted { hash } => {
            let track_info = library.and_then(|lib| lib.get_track(hash).ok());
            match track_info {
                Some(track) => {
                    let artist = track.artist.as_deref().unwrap_or("Unknown");
                    let title = track.title.as_deref().unwrap_or("Unknown");
                    let duration = track
                        .duration
                        .map(|d| format!(" [{}]", d))
                        .unwrap_or_default();
                    println!(
                        "{} {} {} - {}{}",
                        "▶".green().bold(),
                        short_hash(&track.content_hash).bright_black(),
                        artist.green(),
                        title.bold(),
                        duration.bright_black()
                    );
                }
                None => {
                    println!(
                        "{} {}",
                        "▶ started".green().bold(),
                        hash.as_str().bright_black()
                    );
                }
            }
        }
        PlaybackEvent::TrackEnded { hash } => {
            println!(
                "{} {}",
                "■ ended".yellow().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::TrackStopped { hash } => {
            println!(
                "{} {}",
                "⏹ stopped".red().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::TrackPaused { hash } => {
            println!(
                "{} {}",
                "⏸ paused".yellow().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::TrackResumed { hash } => {
            println!(
                "{} {}",
                "▶ resumed".green().bold(),
                hash.as_str().bright_black()
            );
        }
        PlaybackEvent::QueueChanged { length } => {
            println!(
                "{} {} track(s)",
                "♫ queue".blue().bold(),
                length.to_string().bold()
            );
        }
        PlaybackEvent::PositionUpdate { .. } => {
            // Position updates are high-frequency; suppress from the human-readable event stream.
        }
        PlaybackEvent::SessionStarted { id } => {
            println!(
                "{} {}",
                "◉ session started".cyan().bold(),
                id.to_string().bright_black()
            );
        }
        PlaybackEvent::SessionEnded { id } => {
            println!(
                "{} {}",
                "◎ session ended".cyan().bold(),
                id.to_string().bright_black()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OptTopicArg;
    use crate::test_support::run_arg;

    #[test]
    fn topic_arg_parser_is_an_optional_option() {
        assert_eq!(run_arg::<OptTopicArg>(&[]), Ok(OptTopicArg(None)));
        assert_eq!(
            run_arg::<OptTopicArg>(&["--topic", "playback/track_started"]),
            Ok(OptTopicArg(Some("playback/track_started".to_string())))
        );
        assert!(run_arg::<OptTopicArg>(&["--topic"]).is_err());
    }
}
