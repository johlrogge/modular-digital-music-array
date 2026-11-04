use crate::Cli;
use clap::CommandFactory;
use color_eyre::Result;

#[derive(clap::Args, Debug)]
pub(crate) struct Args {
    #[arg(value_enum)]
    shell: clap_complete::Shell,
}

pub(crate) fn generate(args: Args) -> Result<()> {
    let Args { shell } = &args;
    generate_completions(*shell, &mut std::io::stdout());
    Ok(())
}

// =============================================================================
// Shell Completions
// =============================================================================

fn generate_completions(shell: clap_complete::Shell, out: &mut dyn std::io::Write) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "mdma", out);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Shell completions ─────────────────────────────────────────────────────

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
