//! Step definitions for CLI-level BDD tests.
//!
//! These steps invoke the real `mdma` binary as a subprocess, pointing it at
//! the test harness IPC sockets via environment variables. Since stdout is
//! captured (`Stdio::piped()`), the CLI automatically uses pipe-mode output.

use crate::world::MdmaWorld;
use cucumber::{gherkin::Step, then, when};

// =============================================================================
// When steps — run mdma subcommands
// =============================================================================

#[when(regex = r#"^I run mdma (.+)$"#)]
async fn run_mdma(world: &mut MdmaWorld, raw_args: String) {
    let args = shell_split(&raw_args);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    world.run_cli(&arg_refs, None);
}

#[when(regex = r#"^I pipe that through mdma (.+)$"#)]
async fn pipe_through_mdma(world: &mut MdmaWorld, raw_args: String) {
    let stdin = world.last_cli_stdout.clone();
    let args = shell_split(&raw_args);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    world.run_cli(&arg_refs, Some(&stdin));
}

/// Step with a docstring providing explicit stdin content.
#[when(regex = r#"^I pipe the following through mdma (.+):$"#)]
async fn pipe_docstring_through_mdma(world: &mut MdmaWorld, step: &Step, raw_args: String) {
    let stdin = step
        .docstring
        .as_ref()
        .expect("missing docstring for stdin")
        .trim();
    let args = shell_split(&raw_args);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    world.run_cli(&arg_refs, Some(stdin));
}

// =============================================================================
// Then steps — assertions on CLI output
// =============================================================================

#[then(regex = r"^the exit code should be (\d+)$")]
async fn exit_code_should_be(world: &mut MdmaWorld, expected: i32) {
    let actual = world.last_cli_exit_code.expect("no exit code captured");
    assert_eq!(
        actual, expected,
        "Expected exit code {}, got {}.\nstderr: {}",
        expected, actual, world.last_cli_stderr
    );
}

#[then(regex = r"^the output should contain (\d+) lines?$")]
async fn output_should_contain_n_lines(world: &mut MdmaWorld, expected: usize) {
    let lines: Vec<&str> = world
        .last_cli_stdout
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        expected,
        "Expected {} line(s), got {}.\nstdout:\n{}\nstderr:\n{}",
        expected,
        lines.len(),
        world.last_cli_stdout,
        world.last_cli_stderr
    );
}

#[then(regex = r#"^the output should contain "([^"]*)"$"#)]
async fn output_should_contain(world: &mut MdmaWorld, expected: String) {
    assert!(
        world.last_cli_stdout.contains(&expected),
        "Expected output to contain '{}', got:\nstdout:\n{}\nstderr:\n{}",
        expected,
        world.last_cli_stdout,
        world.last_cli_stderr
    );
}

#[then(regex = r#"^the output should not contain "([^"]*)"$"#)]
async fn output_should_not_contain(world: &mut MdmaWorld, expected: String) {
    assert!(
        !world.last_cli_stdout.contains(&expected),
        "Expected output NOT to contain '{}', but it does:\n{}",
        expected,
        world.last_cli_stdout
    );
}

#[then(regex = r#"^line (\d+) should contain "([^"]*)"$"#)]
async fn line_n_should_contain(world: &mut MdmaWorld, line_num: usize, expected: String) {
    let lines: Vec<&str> = world
        .last_cli_stdout
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        line_num >= 1 && line_num <= lines.len(),
        "Line {} out of range (have {} lines)",
        line_num,
        lines.len()
    );
    let line = lines[line_num - 1];
    assert!(
        line.contains(&expected),
        "Expected line {} to contain '{}', got: '{}'",
        line_num,
        expected,
        line
    );
}

#[then(regex = r"^line (\d+) should start with hash ([0-9a-f]{8})$")]
async fn line_n_should_start_with_hash(world: &mut MdmaWorld, line_num: usize, hash: String) {
    let lines: Vec<&str> = world
        .last_cli_stdout
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        line_num >= 1 && line_num <= lines.len(),
        "Line {} out of range (have {} lines)",
        line_num,
        lines.len()
    );
    let line = lines[line_num - 1];
    let first_token = line.split_whitespace().next().unwrap_or("");
    assert_eq!(
        first_token, hash,
        "Expected line {} to start with hash '{}', got first token '{}'",
        line_num, hash, first_token
    );
}

#[then("each line should start with a valid hash")]
async fn each_line_should_start_with_valid_hash(world: &mut MdmaWorld) {
    for (i, line) in world
        .last_cli_stdout
        .lines()
        .filter(|l| !l.is_empty())
        .enumerate()
    {
        let first_token = line.split_whitespace().next().unwrap_or("");
        assert!(
            first_token.len() == 8
                && first_token
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "Line {} has invalid hash prefix '{}': {}",
            i + 1,
            first_token,
            line
        );
    }
}

#[then("the output should:")]
async fn output_should_table(world: &mut MdmaWorld, step: &Step) {
    let table = step
        .table
        .as_ref()
        .expect("missing table for output assertions");
    let stdout = &world.last_cli_stdout;
    for row in &table.rows {
        let (check, value) = (&row[0], &row[1]);
        match check.as_str() {
            "contain" => assert!(
                stdout.contains(value.as_str()),
                "Expected output to contain '{}', got:\n{}",
                value,
                stdout
            ),
            "not contain" => assert!(
                !stdout.contains(value.as_str()),
                "Expected output NOT to contain '{}', but it does:\n{}",
                value,
                stdout
            ),
            "lines" => {
                let expected: usize = value.parse().expect("lines value must be a number");
                let actual = stdout.lines().filter(|l| !l.is_empty()).count();
                assert_eq!(
                    actual, expected,
                    "Expected {} line(s), got {}.\nstdout:\n{}",
                    expected, actual, stdout
                );
            }
            other => panic!("Unknown output assertion: '{}'", other),
        }
    }
}

/// Assert that the output contains exactly the expected rows.
/// The first table row is treated as headers; subsequent rows are data.
/// Rows are matched by the first column (hash) — order doesn't matter.
/// Each cell value must appear somewhere in the matched output line.
#[then("the output rows should be:")]
async fn output_rows_should_be(world: &mut MdmaWorld, step: &Step) {
    assert_rows(step, &world.last_cli_stdout);
}

/// Same as `the output rows should be:` but strips ANSI escape codes first.
#[then("the stripped output rows should be:")]
async fn stripped_output_rows_should_be(world: &mut MdmaWorld, step: &Step) {
    let stripped = strip_ansi(&world.last_cli_stdout);
    assert_rows(step, &stripped);
}

fn assert_rows(step: &Step, stdout: &str) {
    let table = step.table.as_ref().expect("missing table for output rows");
    let headers: Vec<&str> = table.rows[0].iter().map(|s| s.as_str()).collect();
    let expected_rows = &table.rows[1..];

    let output_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| is_data_line(l))
        .collect();

    assert_eq!(
        output_lines.len(),
        expected_rows.len(),
        "Expected {} output line(s), got {}.\nstdout:\n{}",
        expected_rows.len(),
        output_lines.len(),
        stdout
    );

    for row in expected_rows {
        let key = row[0].trim();
        let line = output_lines
            .iter()
            .find(|l| l.contains(key))
            .unwrap_or_else(|| {
                panic!(
                    "No output line contains key '{}' (column '{}')\nFull stdout:\n{}",
                    key, headers[0], stdout
                )
            });
        for (col_idx, header) in headers.iter().enumerate() {
            let expected = row[col_idx].trim();
            if expected.is_empty() {
                continue;
            }
            assert!(
                line.contains(expected),
                "Row with key '{}': expected '{}' (column '{}') in line: '{}'\nFull stdout:\n{}",
                key,
                expected,
                header,
                line,
                stdout
            );
        }
    }
}

#[then(regex = r#"^the stderr should contain "([^"]*)"$"#)]
async fn stderr_should_contain(world: &mut MdmaWorld, expected: String) {
    assert!(
        world.last_cli_stderr.contains(&expected),
        "Expected stderr to contain '{}', got:\n{}",
        expected,
        world.last_cli_stderr
    );
}

// =============================================================================
// When steps — TTY mode (stdout is a terminal, with column width control)
//
// These use a "in tty mode" PREFIX to avoid regex ambiguity with the plain
// `^I run mdma (.+)$` pattern (whose `.+` would greedily match the suffix).
// =============================================================================

/// Default terminal width when not explicitly specified.
const DEFAULT_TTY_COLUMNS: usize = 100;

#[when(regex = r#"^in tty mode I run mdma (.+)$"#)]
async fn run_mdma_tty(world: &mut MdmaWorld, raw_args: String) {
    let args = shell_split(&raw_args);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    world.run_cli_tty(&arg_refs, DEFAULT_TTY_COLUMNS, None);
}

#[when(regex = r#"^in tty mode at (\d+) columns I run mdma (.+)$"#)]
async fn run_mdma_tty_at_columns(world: &mut MdmaWorld, columns: usize, raw_args: String) {
    let args = shell_split(&raw_args);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    world.run_cli_tty(&arg_refs, columns, None);
}

#[when(regex = r#"^in tty mode I pipe that through mdma (.+)$"#)]
async fn pipe_through_mdma_tty(world: &mut MdmaWorld, raw_args: String) {
    // Strip ANSI codes from previous output before piping, so hash parsing works
    let stdin = strip_ansi(&world.last_cli_stdout);
    let args = shell_split(&raw_args);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    world.run_cli_tty(&arg_refs, DEFAULT_TTY_COLUMNS, Some(&stdin));
}

#[when(regex = r#"^in tty mode at (\d+) columns I pipe that through mdma (.+)$"#)]
async fn pipe_through_mdma_tty_at_columns(world: &mut MdmaWorld, columns: usize, raw_args: String) {
    let stdin = strip_ansi(&world.last_cli_stdout);
    let args = shell_split(&raw_args);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    world.run_cli_tty(&arg_refs, columns, Some(&stdin));
}

// =============================================================================
// Then steps — TTY output assertions
// =============================================================================

#[then("the output should have ANSI color codes")]
async fn output_should_have_ansi(world: &mut MdmaWorld) {
    assert!(
        world.last_cli_stdout.contains("\x1b["),
        "Expected ANSI escape codes in output, but found none.\nstdout:\n{}",
        world.last_cli_stdout
    );
}

#[then(regex = r#"^the stripped output should contain "([^"]*)"$"#)]
async fn stripped_output_should_contain(world: &mut MdmaWorld, expected: String) {
    let stripped = strip_ansi(&world.last_cli_stdout);
    assert!(
        stripped.contains(&expected),
        "Expected stripped output to contain '{}', got:\n{}",
        expected,
        stripped
    );
}

#[then(regex = r#"^the stripped output should not contain "([^"]*)"$"#)]
async fn stripped_output_should_not_contain(world: &mut MdmaWorld, expected: String) {
    let stripped = strip_ansi(&world.last_cli_stdout);
    assert!(
        !stripped.contains(&expected),
        "Expected stripped output NOT to contain '{}', but it does:\n{}",
        expected,
        stripped
    );
}

#[then("the stripped output should be:")]
async fn stripped_output_should_be(world: &mut MdmaWorld, step: &Step) {
    let expected = step
        .docstring
        .as_ref()
        .expect("missing docstring for expected output")
        .trim();
    let stripped = strip_ansi(&world.last_cli_stdout);

    // Compare non-empty lines, trimming trailing whitespace from each
    let actual_lines: Vec<&str> = stripped
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .collect();
    let expected_lines: Vec<&str> = expected
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .collect();

    assert_eq!(
        actual_lines,
        expected_lines,
        "Stripped output mismatch.\nExpected:\n{}\n\nActual:\n{}",
        expected_lines.join("\n"),
        actual_lines.join("\n")
    );
}

#[then(regex = r#"^the stripped output line (\d+) should be "([^"]*)"$"#)]
async fn stripped_output_line_should_be(world: &mut MdmaWorld, line_num: usize, expected: String) {
    let stripped = strip_ansi(&world.last_cli_stdout);
    let lines: Vec<&str> = stripped
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        line_num >= 1 && line_num <= lines.len(),
        "Line {} out of range (have {} non-empty lines)",
        line_num,
        lines.len()
    );
    assert_eq!(
        lines[line_num - 1],
        expected,
        "Stripped line {} mismatch.\nExpected: '{}'\nActual:   '{}'",
        line_num,
        expected,
        lines[line_num - 1]
    );
}

// =============================================================================
// Helpers
// =============================================================================

/// Check if a line is a data line (starts with an 8-char hex hash).
/// Filters out header lines like "Search results (N matches)".
fn is_data_line(line: &str) -> bool {
    let token = line.split_whitespace().next().unwrap_or("");
    token.len() == 8 && token.chars().all(|c| c.is_ascii_hexdigit())
}

/// Strip ANSI escape sequences (CSI sequences like `\x1b[...X` where X is a letter).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Look for '[' to start a CSI sequence
            if let Some(next) = chars.next() {
                if next == '[' {
                    // Skip until we find a terminating letter (ASCII @ through ~)
                    for seq_char in chars.by_ref() {
                        if seq_char.is_ascii_alphabetic() || seq_char == '~' {
                            break;
                        }
                    }
                }
                // else: non-CSI escape, just skip the two chars
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Minimal shell-like argument splitting that handles double-quoted strings.
/// Splits on whitespace but keeps quoted segments together.
fn shell_split(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}
