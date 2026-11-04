use crate::args::impl_arg;
use color_eyre::Result;

/// The shell to generate completions for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellArg(pub hicli::Shell);

impl std::str::FromStr for ShellArg {
    type Err = hicli::UnknownShell;

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
        assert!(run_arg::<ShellArg>(&["tcsh"]).is_err());
        assert!(run_arg::<ShellArg>(&[]).is_err());
    }

    #[test]
    fn bash_completion_script_is_nonempty_and_names_the_binary() {
        let output = hicli::completion_script(hicli::Shell::Bash, "mdma");
        assert!(
            !output.is_empty(),
            "bash completion script must not be empty"
        );
        assert!(
            output.contains("mdma"),
            "bash completion script must reference the binary name 'mdma'"
        );
    }
}
