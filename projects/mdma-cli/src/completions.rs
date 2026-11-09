use crate::args::impl_arg;
use color_eyre::Result;

/// The shell to generate completions for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellArg(pub hicli::Shell);

impl std::str::FromStr for ShellArg {
    type Err = hicli::UnknownChoice;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

impl_arg!(
    ShellArg,
    positional "SHELL",
    "Shell to generate completions for (bash, zsh or fish)"
);

/// Generate shell completions
#[hicli::command(hide)]
pub(crate) fn generate_completions(shell: ShellArg) -> Result<()> {
    print!(
        "{}",
        hicli::completion_script(shell.0, env!("CARGO_BIN_NAME"))
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_arg_parser_reads_a_shell_name() {
        use crate::test_support::run_arg;
        assert_eq!(
            run_arg::<ShellArg>(&["bash"]),
            Ok(ShellArg(hicli::Shell::Bash))
        );
        assert_eq!(
            run_arg::<ShellArg>(&["zsh"]),
            Ok(ShellArg(hicli::Shell::Zsh))
        );
        assert_eq!(
            run_arg::<ShellArg>(&["fish"]),
            Ok(ShellArg(hicli::Shell::Fish))
        );
        assert!(run_arg::<ShellArg>(&["tcsh"]).is_err());
        assert!(run_arg::<ShellArg>(&[]).is_err());
    }

    #[test]
    fn completion_scripts_are_nonempty_and_name_the_binary() {
        let bin = env!("CARGO_BIN_NAME");
        for shell in [hicli::Shell::Bash, hicli::Shell::Zsh, hicli::Shell::Fish] {
            let output = hicli::completion_script(shell, bin);
            assert!(!output.is_empty(), "{shell:?} script must not be empty");
            assert!(
                output.contains(bin),
                "{shell:?} script must reference the binary name '{bin}'"
            );
        }
    }
}
