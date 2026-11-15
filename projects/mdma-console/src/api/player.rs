//! Player wire types for the new web UI.
//!
//! `ApiPlayer` is the shape of `GET /api/player` and the `{queue}` every `/api/queue/*`
//! mutation returns. `static/js/core.js` reads exactly these keys.

use serde::Serialize;

/// Transport state. `idle` means nothing is loaded; paused and loading both count as busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerState {
    Idle,
    Playing,
    Paused,
    Loading,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiPlayer {
    pub state: PlayerState,
    /// Hash of the loaded track, or null when idle.
    pub now: Option<String>,
    /// Hashes in Up next, next track first.
    pub queue: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_json_has_exactly_state_now_queue() {
        let p = ApiPlayer {
            state: PlayerState::Paused,
            now: Some("sha256:a".into()),
            queue: vec!["sha256:b".into()],
        };
        let json = serde_json::to_value(&p).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["now", "queue", "state"]);
        assert_eq!(json["state"], "paused");
        assert_eq!(json["queue"][0], "sha256:b");
    }

    #[test]
    fn idle_player_has_null_now_and_every_state_is_lowercase() {
        let p = ApiPlayer {
            state: PlayerState::Idle,
            now: None,
            queue: vec![],
        };
        let json = serde_json::to_value(&p).unwrap();
        assert!(json["now"].is_null());
        for (s, want) in [
            (PlayerState::Idle, "idle"),
            (PlayerState::Playing, "playing"),
            (PlayerState::Paused, "paused"),
            (PlayerState::Loading, "loading"),
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), want);
        }
    }
}
