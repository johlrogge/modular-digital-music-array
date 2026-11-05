use crate::args::impl_arg;
use crate::ctx::Ctx;
use crate::output::{library_error, playback_error};
use color_eyre::Result;
use mdma_client::Deck;
use track_formatter::short_hash;

/// Start playback from the queue (use `mdma queue append <hash>` to enqueue tracks)
#[hicli::command]
pub(crate) fn start(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    playback.play_queue().map_err(playback_error)?;
    println!("Playing from queue");
    Ok(())
}

/// Stop playback on deck A
#[hicli::command]
pub(crate) fn stop(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    playback.stop(Deck::A).map_err(playback_error)?;

    println!("Stopped");
    Ok(())
}

/// Pause playback on deck A (keeps the current track loaded; resume to continue)
#[hicli::command]
pub(crate) fn pause(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    playback.pause(Deck::A).map_err(playback_error)?;

    println!("Paused");
    Ok(())
}

/// Resume playback on deck A after a pause
#[hicli::command]
pub(crate) fn resume(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    playback.resume(Deck::A).map_err(playback_error)?;

    println!("Resumed");
    Ok(())
}

/// Skip to the next track in the queue
#[hicli::command]
pub(crate) fn skip(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    playback.skip().map_err(playback_error)?;

    println!("Skipped");
    Ok(())
}

/// Show what is currently playing
#[hicli::command]
pub(crate) fn now(ctx: &Ctx) -> Result<()> {
    use std::io::IsTerminal;
    let playback = ctx.playback()?;
    // The library is only consulted on a terminal, where the track is shown in full.
    let library = std::io::stdout()
        .is_terminal()
        .then(|| ctx.library())
        .transpose()?;
    let hash = playback.now_playing().map_err(playback_error)?;
    match hash {
        None => {
            eprintln!("Nothing playing");
            std::process::exit(1);
        }
        Some(h) => match library {
            None => println!("{}", h.as_str()),
            Some(library) => {
                let track = library.get_track(&h).map_err(library_error)?;
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

/// Show the current session ID (a session spans from first play to queue empty)
#[hicli::command]
pub(crate) fn session(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    match playback.session() {
        Ok(Some(id)) => {
            println!("{}", id);
        }
        Ok(None) => {
            println!("No active session");
        }
        Err(e) => return Err(playback_error(e)),
    }
    Ok(())
}

/// List available audio output devices
#[hicli::command]
pub(crate) fn outputs(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    let sinks = playback.list_audio_outputs().map_err(playback_error)?;
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

/// An audio output device name as shown by `mdma playback outputs`.
// Stays a String: device names have no domain rule; the service reports unknown ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputNameArg(pub String);

impl std::str::FromStr for OutputNameArg {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

impl_arg!(
    OutputNameArg,
    positional "NAME",
    "Device name as shown by `mdma playback outputs`"
);

/// Select an audio output device by name
#[hicli::command]
pub(crate) fn set_output(name: OutputNameArg, ctx: &Ctx) -> Result<()> {
    let OutputNameArg(name) = name;
    let playback = ctx.playback()?;
    let cfg = playback.set_audio_output(name).map_err(playback_error)?;
    let device = cfg.device_name.as_deref().unwrap_or("auto");
    let rate = cfg
        .sample_rate
        .map(|r| r.to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("Audio output set to: {} ({}Hz)", device, rate);
    Ok(())
}

/// Show the currently selected audio output
#[hicli::command]
pub(crate) fn get_output(ctx: &Ctx) -> Result<()> {
    let playback = ctx.playback()?;
    let cfg = playback.get_audio_output().map_err(playback_error)?;
    let device = cfg.device_name.as_deref().unwrap_or("auto");
    let rate = cfg
        .sample_rate
        .map(|r| r.to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("{} ({}Hz)", device, rate);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::OutputNameArg;
    use crate::test_support::run_arg;

    #[test]
    fn output_name_arg_parser_reads_a_positional() {
        assert_eq!(
            run_arg::<OutputNameArg>(&["alsa_output.usb"]),
            Ok(OutputNameArg("alsa_output.usb".to_string()))
        );
        assert!(run_arg::<OutputNameArg>(&[]).is_err());
    }
}
