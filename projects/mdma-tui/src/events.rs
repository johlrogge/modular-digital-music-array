use event_protocol::{decode_topic_message, PlaybackEvent, TOPIC_PLAYBACK};
use nng::options::Options;
use std::sync::mpsc;
use std::thread;

/// Events that flow from the background subscriber thread to the main loop.
pub enum AppEvent {
    Playback(PlaybackEvent),
    /// A message that could not be read. The subscription goes on; it is only news
    /// for the status line.
    Malformed(String),
    /// The subscription failed and has ended: the thread exits after sending this,
    /// and the channel closes.
    SubscriberError(String),
}

/// Spawn a background thread that subscribes to playback events and sends
/// them over the returned receiver.
///
/// Uses the same nng Sub0 pattern as `mdma subscribe` in the CLI, except that the dial
/// does not wait for a connection: the socket connects, and reconnects after a
/// publisher goes and comes back, by itself. Whether the node answers is therefore not
/// the subscriber's business, and nothing needs to subscribe again. The thread ends
/// when the socket fails or when nobody is listening to the receiver any more.
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
        .dial_async(&resolved)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to {}: {}", event_addr, e))?;

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || loop {
        let event = match socket.recv() {
            Ok(msg) => match decode_topic_message::<PlaybackEvent>(msg.as_slice()) {
                Ok((_topic, event)) => AppEvent::Playback(event),
                Err(e) => AppEvent::Malformed(format!("Parse error: {}", e)),
            },
            Err(e) => {
                let _ = tx.send(AppEvent::SubscriberError(format!("Receive error: {}", e)));
                break;
            }
        };
        if tx.send(event).is_err() {
            break;
        }
    });

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use event_protocol::to_topic_message;
    use std::time::{Duration, Instant};

    fn address(name: &str) -> String {
        format!(
            "ipc://{}/mdma-tui-events-{name}-{}.sock",
            std::env::temp_dir().display(),
            std::process::id()
        )
    }

    fn publisher(address: &str) -> nng::Socket {
        let publisher = nng::Socket::new(nng::Protocol::Pub0).expect("publisher socket");
        publisher.listen(address).expect("listen");
        publisher
    }

    fn started() -> Vec<u8> {
        to_topic_message(&PlaybackEvent::TrackStarted {
            hash: mdma_client::ContentHash::new("sha256:aaaa000000000001"),
        })
    }

    /// Publish `message` until `wanted` finds an event it likes, or time runs out: a
    /// subscriber that connects late misses what was sent before.
    fn publish_until<T>(
        publisher: &nng::Socket,
        rx: &mpsc::Receiver<AppEvent>,
        message: &[u8],
        mut wanted: impl FnMut(AppEvent) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            publisher
                .send(nng::Message::from(message))
                .map_err(|(_, e)| e)
                .expect("publish");
            if let Ok(event) = rx.recv_timeout(Duration::from_millis(100)) {
                if let Some(found) = wanted(event) {
                    return found;
                }
            }
        }
        panic!("no event came in time");
    }

    /// A message that cannot be read is news for the status line; the same
    /// subscription (one thread, one receiver) keeps delivering what follows.
    #[test]
    fn a_malformed_event_leaves_the_subscription_running() {
        let address = address("malformed");
        let publisher = publisher(&address);
        let rx = spawn_event_subscriber(&address).expect("subscribes");

        let malformed = publish_until(
            &publisher,
            &rx,
            b"playback/{not json",
            |event| match event {
                AppEvent::Malformed(text) => Some(text),
                _ => None,
            },
        );
        assert!(malformed.starts_with("Parse error"), "{malformed}");

        // The same receiver still gets events: the thread did not end.
        publish_until(&publisher, &rx, &started(), |event| match event {
            AppEvent::Playback(PlaybackEvent::TrackStarted { .. }) => Some(()),
            _ => None,
        });
    }

    /// The dial does not wait for a publisher: events come once one appears later.
    #[test]
    fn events_come_once_a_publisher_appears_later() {
        let address = address("late");
        let rx = spawn_event_subscriber(&address).expect("subscribes with nobody there");
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let publisher = publisher(&address);

        publish_until(&publisher, &rx, &started(), |event| match event {
            AppEvent::Playback(PlaybackEvent::TrackStarted { .. }) => Some(()),
            _ => None,
        });
    }
}
