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
pub(crate) struct ShellArg(pub clap_complete::Shell);

impl hicli::Arg for ShellArg {
    fn parser() -> impl bpaf::Parser<Self> {
        use bpaf::Parser as _;
        bpaf::positional::<clap_complete::Shell>("SHELL").map(Self)
    }
}

pub(crate) fn generate(shell: ShellArg) -> Result<()> {
    let ShellArg(shell) = shell;
    generate_completions(shell, &mut std::io::stdout());
    Ok(())
}

fn generate_completions(shell: clap_complete::Shell, out: &mut dyn std::io::Write) {
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
            Ok(ShellArg(clap_complete::Shell::Bash))
        );
        assert_eq!(
            run_arg::<ShellArg>(&["zsh"]),
            Ok(ShellArg(clap_complete::Shell::Zsh))
        );
        assert!(run_arg::<ShellArg>(&["tcsh"]).is_err());
        assert!(run_arg::<ShellArg>(&[]).is_err());
    }

    #[test]
    fn generate_completions_bash_produces_nonempty_output() {
        // The generate_completions function must produce a non-empty bash script.
        let mut buf = Vec::new();
        generate_completions(clap_complete::Shell::Bash, &mut buf);
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
