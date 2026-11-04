use crate::ctx::Ctx;
use crate::output::{handle_error, handle_playback_error};
use clap::Subcommand;
use color_eyre::Result;
use mdma_client::Deck;
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
    match command {
        Command::Start => start(ctx),
        Command::Stop => stop(ctx),
        Command::Pause => pause(ctx),
        Command::Resume => resume(ctx),
        Command::Skip => skip(ctx),
        Command::Now => now(ctx),
        Command::Session => session(ctx),
        Command::Outputs => outputs(ctx),
        Command::SetOutput { name } => set_output(name, ctx),
        Command::GetOutput => get_output(ctx),
    }
}

pub(crate) fn start(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    if let Err(e) = media_client.play_queue() {
        handle_playback_error(e);
    }
    println!("Playing from queue");
    Ok(())
}

pub(crate) fn stop(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    if let Err(e) = media_client.stop(Deck::A) {
        handle_playback_error(e);
    }

    println!("Stopped");
    Ok(())
}

pub(crate) fn pause(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    if let Err(e) = media_client.pause(Deck::A) {
        handle_playback_error(e);
    }

    println!("Paused");
    Ok(())
}

pub(crate) fn resume(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    if let Err(e) = media_client.resume(Deck::A) {
        handle_playback_error(e);
    }

    println!("Resumed");
    Ok(())
}

pub(crate) fn skip(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    if let Err(e) = media_client.skip() {
        handle_playback_error(e);
    }

    println!("Skipped");
    Ok(())
}

pub(crate) fn now(ctx: &Ctx) -> Result<()> {
    use std::io::IsTerminal;
    let media_client = ctx.playback();
    // The library is only consulted on a terminal, where the track is shown in full.
    let library_client = std::io::stdout().is_terminal().then(|| ctx.library());
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

pub(crate) fn session(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
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

pub(crate) fn outputs(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
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

pub(crate) fn set_output(name: String, ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
    let cfg = match media_client.set_audio_output(name) {
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

pub(crate) fn get_output(ctx: &Ctx) -> Result<()> {
    let media_client = ctx.playback();
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
