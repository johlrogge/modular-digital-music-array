/// A single command available in the command palette.
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

/// All commands recognised by the palette.
pub static COMMANDS: &[Command] = &[
    Command {
        name: "play",
        description: "Resume or start playback",
    },
    Command {
        name: "pause",
        description: "Pause playback",
    },
    Command {
        name: "stop",
        description: "Stop playback",
    },
    Command {
        name: "next",
        description: "Skip to next track",
    },
    Command {
        name: "clear",
        description: "Clear the playback queue",
    },
    Command {
        name: "shuffle",
        description: "Shuffle the playback queue",
    },
    Command {
        name: "quit",
        description: "Quit mdma-tui",
    },
    Command {
        name: "search",
        description: "Switch active pane to search",
    },
    Command {
        name: "browser",
        description: "Switch active pane to browser",
    },
    Command {
        name: "queue",
        description: "Switch active pane to queue",
    },
    Command {
        name: "playlists",
        description: "Switch active pane to playlists list",
    },
    Command {
        name: "o",
        description: "Open or create a playlist  :o <name>",
    },
    Command {
        name: "history",
        description: "Show recent play history  :history [days]",
    },
];

/// Return all commands whose name starts with `input` (case-insensitive).
pub fn matching(input: &str) -> Vec<&'static Command> {
    let lower = input.to_ascii_lowercase();
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(lower.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_empty_returns_all() {
        let results = matching("");
        assert_eq!(results.len(), COMMANDS.len());
    }

    #[test]
    fn matching_p_returns_play_pause() {
        let results = matching("p");
        let names: Vec<&str> = results.iter().map(|c| c.name).collect();
        assert!(names.contains(&"play"), "expected 'play' in results");
        assert!(names.contains(&"pause"), "expected 'pause' in results");
        // Should not include commands not starting with 'p'
        for name in &names {
            assert!(
                name.starts_with('p'),
                "unexpected command '{}' in results for prefix 'p'",
                name
            );
        }
    }

    #[test]
    fn matching_unknown_returns_empty() {
        let results = matching("xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn pane_switching_commands_are_registered() {
        let all_names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert!(
            all_names.contains(&"search"),
            "expected 'search' command in COMMANDS"
        );
        assert!(
            all_names.contains(&"browser"),
            "expected 'browser' command in COMMANDS"
        );
        assert!(
            all_names.contains(&"queue"),
            "expected 'queue' command in COMMANDS"
        );
        assert!(
            all_names.contains(&"playlists"),
            "expected 'playlists' command in COMMANDS"
        );
    }

    #[test]
    fn matching_br_returns_browser() {
        let results = matching("br");
        let names: Vec<&str> = results.iter().map(|c| c.name).collect();
        assert!(names.contains(&"browser"), "expected 'browser' in results");
    }

    #[test]
    fn o_command_is_registered() {
        let all_names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert!(
            all_names.contains(&"o"),
            "expected 'o' command in COMMANDS for open/create playlist"
        );
    }

    #[test]
    fn matching_o_returns_o_command() {
        let results = matching("o");
        let names: Vec<&str> = results.iter().map(|c| c.name).collect();
        assert!(
            names.contains(&"o"),
            "expected 'o' in results for prefix 'o'"
        );
    }
}
