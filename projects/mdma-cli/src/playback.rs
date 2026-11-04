use crate::ctx::Ctx;
use crate::output::{handle_error, handle_playback_error};
use clap::Subcommand;
use color_eyre::Result;
use mdma_client::{Deck, LibraryBackend, PlaybackBackend};
use track_formatter::short_hash;

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Start playback from the queue (use `mdma queue append <hash>` to enqueue tracks)
    Start,

    /// Stop playback on deck A
    Stop,

    /// Pause playback on deck A (keeps the current track loaded; resume to continue)
    Pause,

    /// Resume playback on deck A after a pause
    Resume,

    /// Skip to the next track in the queue
    Skip,

    /// Show what is currently playing
    Now,

    /// Show the current session ID (a session spans from first play to queue empty)
    Session,

    /// List available audio output devices
    Outputs,

    /// Select an audio output device by name
    SetOutput {
        /// Device name as shown by `mdma playback outputs`
        name: String,
    },

    /// Show the currently selected audio output
    GetOutput,
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    let pb = ctx.playback();
    match &command {
        Command::Start => handle_playback_start(&pb),
        Command::Stop => handle_playback_stop(&pb),
        Command::Pause => handle_playback_pause(&pb),
        Command::Resume => handle_playback_resume(&pb),
        Command::Skip => handle_playback_skip(&pb),
        Command::Now => {
            use std::io::IsTerminal;
            if std::io::stdout().is_terminal() {
                let lib = ctx.library();
                handle_playback_now(&pb, Some(&lib))
            } else {
                handle_playback_now(&pb, None)
            }
        }
        Command::Session => handle_playback_session(&pb),
        Command::Outputs => handle_playback_outputs(&pb),
        Command::SetOutput { name } => handle_playback_set_output(&pb, name),
        Command::GetOutput => handle_playback_get_output(&pb),
    }
}

// =============================================================================
// Playback Command Handlers
// =============================================================================

fn handle_playback_start(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.play_queue() {
        handle_playback_error(e);
    }
    println!("Playing from queue");
    Ok(())
}

fn handle_playback_now(
    media_client: &PlaybackBackend,
    library_client: Option<&LibraryBackend>,
) -> Result<()> {
    let hash = match media_client.now_playing() {
        Ok(h) => h,
        Err(e) => handle_playback_error(e),
    };
    match hash {
        None => {
            eprintln!("Nothing playing");
            std::process::exit(1);
        }
        Some(h) => match library_client {
            None => println!("{}", h.as_str()),
            Some(lib) => {
                let track = match lib.get_track(&h) {
                    Ok(t) => t,
                    Err(e) => handle_error(e),
                };
                let artist = track.artist.as_deref().unwrap_or("-");
                let title = track.title.as_deref().unwrap_or("-");
                let duration = track
                    .duration
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!("{}  {} - {}  [{}]", short_hash(&h), artist, title, duration);
            }
        },
    }
    Ok(())
}

fn handle_playback_session(media_client: &PlaybackBackend) -> Result<()> {
    match media_client.session() {
        Ok(Some(id)) => {
            println!("{}", id);
        }
        Ok(None) => {
            println!("No active session");
        }
        Err(e) => handle_playback_error(e),
    }
    Ok(())
}

fn handle_playback_outputs(media_client: &PlaybackBackend) -> Result<()> {
    let sinks = match media_client.list_audio_outputs() {
        Ok(s) => s,
        Err(e) => handle_playback_error(e),
    };
    println!("{:<40} {:<40} MAX RATE", "NAME", "DESCRIPTION");
    println!("{}", "-".repeat(90));
    for sink in &sinks {
        println!(
            "{:<40} {:<40} {}",
            sink.name,
            sink.description.as_deref().unwrap_or("-"),
            sink.max_sample_rate
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
    Ok(())
}

fn handle_playback_set_output(media_client: &PlaybackBackend, name: &str) -> Result<()> {
    let cfg = match media_client.set_audio_output(name.to_string()) {
        Ok(c) => c,
        Err(e) => handle_playback_error(e),
    };
    let device = cfg.device_name.as_deref().unwrap_or("auto");
    let rate = cfg
        .sample_rate
        .map(|r| r.to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("Audio output set to: {} ({}Hz)", device, rate);
    Ok(())
}

fn handle_playback_get_output(media_client: &PlaybackBackend) -> Result<()> {
    let cfg = match media_client.get_audio_output() {
        Ok(c) => c,
        Err(e) => handle_playback_error(e),
    };
    let device = cfg.device_name.as_deref().unwrap_or("auto");
    let rate = cfg
        .sample_rate
        .map(|r| r.to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("{} ({}Hz)", device, rate);
    Ok(())
}

fn handle_playback_stop(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.stop(Deck::A) {
        handle_playback_error(e);
    }

    println!("Stopped");
    Ok(())
}

fn handle_playback_skip(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.skip() {
        handle_playback_error(e);
    }

    println!("Skipped");
    Ok(())
}

fn handle_playback_pause(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.pause(Deck::A) {
        handle_playback_error(e);
    }

    println!("Paused");
    Ok(())
}

fn handle_playback_resume(media_client: &PlaybackBackend) -> Result<()> {
    if let Err(e) = media_client.resume(Deck::A) {
        handle_playback_error(e);
    }

    println!("Resumed");
    Ok(())
}
