use event_protocol::{decode_topic_message, PlaybackEvent, TOPIC_PLAYBACK};
use nng::options::Options;
use std::sync::mpsc;
use std::thread;

/// Events that flow from the background subscriber thread to the main loop.
pub enum AppEvent {
    Playback(PlaybackEvent),
    SubscriberError(String),
}

/// Spawn a background thread that subscribes to playback events and sends
/// them over the returned receiver.
///
/// Uses the same nng Sub0 pattern as `mdma subscribe` in the CLI.
pub fn spawn_event_subscriber(
    event_addr: &str,
) -> Result<mpsc::Receiver<AppEvent>, color_eyre::Report> {
    let socket = nng::Socket::new(nng::Protocol::Sub0)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to create Sub0 socket: {}", e))?;

    socket
        .set_opt::<nng::options::protocol::pubsub::Subscribe>(TOPIC_PLAYBACK.as_bytes().to_vec())
        .map_err(|e| color_eyre::eyre::eyre!("Failed to set subscription: {}", e))?;

    let resolved = nng_transport::resolve_tcp_hostname(event_addr)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to resolve address: {}", e))?;

    socket
        .dial(&resolved)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to {}: {}", event_addr, e))?;

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || loop {
        match socket.recv() {
            Ok(msg) => match decode_topic_message::<PlaybackEvent>(msg.as_slice()) {
                Ok((_topic, event)) => {
                    if tx.send(AppEvent::Playback(event)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::SubscriberError(format!("Parse error: {}", e)));
                    // Don't break on parse errors; continue receiving.
                }
            },
            Err(e) => {
                let _ = tx.send(AppEvent::SubscriberError(format!("Receive error: {}", e)));
                break;
            }
        }
    });

    Ok(rx)
}
