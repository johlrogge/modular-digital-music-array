//! The idle guard for "play now" and "start".
//!
//! `PlayQueue` on a playing deck pops the next track and cuts off the current one; on a paused
//! deck it resumes. The new UI must never do either by accident, so the only way to build the
//! commands that end in `PlayQueue` is to hold an [`Idle`], and the only way to get an [`Idle`]
//! is [`require_idle`] on what the playback service reported as loaded.

use media_protocol::{CanonicalHash, Command, ContentHash, SourceName};

/// Proof that nothing is loaded. Only [`require_idle`] can make one.
pub struct Idle(());

/// Why the guard refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Busy {
    /// This track is loaded (playing or paused).
    Loaded(ContentHash),
}

/// `now` is the answer to `NowPlaying`: `Some` for playing and paused alike.
pub fn require_idle(now: Option<ContentHash>) -> Result<Idle, Busy> {
    match now {
        None => Ok(Idle(())),
        Some(hash) => Err(Busy::Loaded(hash)),
    }
}

/// Start whatever is at the head of the queue.
pub fn start_command(_idle: Idle) -> Command {
    Command::PlayQueue
}

/// `QueueNext` for each hash in reverse order (the set plays in the order given). Only
/// resolved hashes are taken, so nothing unresolved can be sent.
pub fn queue_next_commands(hashes: &[CanonicalHash]) -> Vec<Command> {
    hashes
        .iter()
        .rev()
        .map(|hash| Command::QueueNext {
            hash: hash.into(),
            source: SourceName::audio(),
        })
        .collect()
}

/// `QueueAppend` for each hash, in order. Only resolved hashes are taken.
pub fn queue_last_commands(hashes: &[CanonicalHash]) -> Vec<Command> {
    hashes
        .iter()
        .map(|hash| Command::QueueAppend {
            hash: hash.into(),
            source: SourceName::audio(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> ContentHash {
        ContentHash::new(s)
    }

    fn full(c: char) -> CanonicalHash {
        CanonicalHash::try_from(h(&format!("sha256:{}", c.to_string().repeat(64)))).unwrap()
    }

    fn describe(c: &Command) -> String {
        match c {
            Command::QueueNext { hash, .. } => format!("next {}", hash.as_str()),
            Command::QueueAppend { hash, .. } => format!("append {}", hash.as_str()),
            Command::PlayQueue => "play".into(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn a_loaded_track_is_busy() {
        let err = require_idle(Some(h("sha256:a"))).err().expect("busy");
        assert_eq!(err, Busy::Loaded(h("sha256:a")));
    }

    #[test]
    fn nothing_loaded_is_idle() {
        assert!(require_idle(None).is_ok());
    }

    #[test]
    fn queue_next_queues_in_reverse() {
        let commands = queue_next_commands(&[full('a'), full('b'), full('c')]);
        let got: Vec<_> = commands.iter().map(describe).collect();
        assert_eq!(
            got,
            [
                format!("next {}", full('c')),
                format!("next {}", full('b')),
                format!("next {}", full('a')),
            ]
        );
    }

    #[test]
    fn start_is_play_queue() {
        let idle = require_idle(None).ok().unwrap();
        assert!(matches!(start_command(idle), Command::PlayQueue));
    }

    #[test]
    fn queue_last_keeps_order() {
        let got: Vec<_> = queue_last_commands(&[full('a'), full('b')])
            .iter()
            .map(describe)
            .collect();
        assert_eq!(
            got,
            [
                format!("append {}", full('a')),
                format!("append {}", full('b')),
            ]
        );
    }
}
