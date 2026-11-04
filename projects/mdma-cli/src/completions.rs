use crate::args::impl_arg;
use crate::Cli;
use clap::CommandFactory;
use color_eyre::Result;

#[derive(clap::Args, Debug)]
pub(crate) struct CompletionsCli {
    #[arg(value_enum)]
    pub(crate) shell: clap_complete::Shell,
}

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

/// Clap's completion script (clap drives `main` until the tree is flipped).
pub(crate) fn generate_clap(shell: clap_complete::Shell) -> Result<()> {
    generate_clap_completions(shell, &mut std::io::stdout());
    Ok(())
}

fn generate_clap_completions(shell: clap_complete::Shell, out: &mut dyn std::io::Write) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "mdma", out);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Shell completions ─────────────────────────────────────────────────────

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
    fn generate_completions_bash_produces_nonempty_output() {
        // The generate_clap_completions function must produce a non-empty bash script.
        let mut buf = Vec::new();
        generate_clap_completions(clap_complete::Shell::Bash, &mut buf);
        let output = String::from_utf8(buf).expect("completion output is valid UTF-8");
        assert!(
            !output.is_empty(),
            "bash completion output must not be empty"
        );
        assert!(
            output.contains("mdma"),
            "bash completion output must reference the binary name 'mdma'"
        );
    }
}
