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
}
