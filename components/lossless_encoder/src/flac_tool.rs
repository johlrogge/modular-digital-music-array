//! The `flac` command-line program (libFLAC), found on PATH or at a given path.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::refusal::{EncodeError, EnvironmentError, Refusal, RefusalReason};

/// `--keep-foreign-metadata` for AIFF needs libFLAC 1.4.
const MIN_VERSION: (u32, u32) = (1, 4);
const DETAIL_LIMIT: usize = 400;

#[derive(Debug, Clone)]
pub(crate) struct FlacTool {
    binary: PathBuf,
}

/// Binaries whose version has been accepted, so `flac --version` runs once per
/// binary and process.
fn accepted() -> &'static Mutex<HashSet<PathBuf>> {
    static ACCEPTED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ACCEPTED.get_or_init(Mutex::default)
}

/// `major.minor` of `flac --version` output (`flac 1.4.3`).
fn parse_version(output: &str) -> Option<(u32, u32)> {
    let mut words = output.lines().next()?.split_whitespace();
    if words.next()? != "flac" {
        return None;
    }
    let mut parts = words.next()?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.len() <= DETAIL_LIMIT {
        return text.to_string();
    }
    let mut start = text.len() - DETAIL_LIMIT;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
}

impl FlacTool {
    pub fn on_path() -> Self {
        Self::at("flac")
    }

    pub fn at(binary: impl Into<PathBuf>) -> Self {
        FlacTool {
            binary: binary.into(),
        }
    }

    /// The program exists and is 1.4 or newer. Checked once per binary and
    /// process; only success is remembered.
    pub fn ensure_usable(&self) -> Result<(), EnvironmentError> {
        if accepted()
            .lock()
            .is_ok_and(|accepted| accepted.contains(&self.binary))
        {
            return Ok(());
        }
        let output = Command::new(&self.binary)
            .arg("--version")
            .output()
            .map_err(|e| {
                let what = format!("{}: {e}", self.binary.display());
                if e.kind() == std::io::ErrorKind::NotFound {
                    EnvironmentError::FlacMissing(what)
                } else {
                    EnvironmentError::Spawn(what)
                }
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let version = parse_version(&stdout).ok_or_else(|| {
            EnvironmentError::FlacTooOld(format!("cannot read a version from {:?}", stdout.trim()))
        })?;
        if version < MIN_VERSION {
            return Err(EnvironmentError::FlacTooOld(format!(
                "found {}.{}, need 1.4",
                version.0, version.1
            )));
        }
        if let Ok(mut accepted) = accepted().lock() {
            accepted.insert(self.binary.clone());
        }
        Ok(())
    }

    /// `flac -8 --keep-foreign-metadata --verify -o out src`. `-f` replaces a
    /// leftover of a crashed run; `-s` keeps stderr to the errors.
    pub fn encode(&self, src: &Path, out: &Path) -> Result<(), EncodeError> {
        self.run(
            [
                OsStr::new("-8"),
                OsStr::new("-s"),
                OsStr::new("-f"),
                OsStr::new("--keep-foreign-metadata"),
                OsStr::new("--verify"),
                OsStr::new("-o"),
                out.as_os_str(),
                src.as_os_str(),
            ],
            RefusalReason::EncoderFailed,
        )
    }

    /// `flac -d --keep-foreign-metadata -o out flac`.
    pub fn decode(&self, flac: &Path, out: &Path) -> Result<(), EncodeError> {
        self.run(
            [
                OsStr::new("-d"),
                OsStr::new("-s"),
                OsStr::new("-f"),
                OsStr::new("--keep-foreign-metadata"),
                OsStr::new("-o"),
                out.as_os_str(),
                flac.as_os_str(),
            ],
            RefusalReason::RebuildMismatch,
        )
    }

    /// Run flac to the end and report. `failure` is the refusal for a flac that
    /// ran and rejected the file. There is no timeout: this runs on the
    /// background conversion worker (ADR-037 decision 7), never on a request
    /// thread, and a long track legitimately takes a while.
    fn run<'a>(
        &self,
        args: impl IntoIterator<Item = &'a OsStr>,
        failure: RefusalReason,
    ) -> Result<(), EncodeError> {
        // The version check has passed, so the program exists: a failure to
        // start it now is the machine's (permissions, resources), not "missing".
        let output = Command::new(&self.binary)
            .args(args)
            .output()
            .map_err(|e| EnvironmentError::Spawn(format!("{}: {e}", self.binary.display())))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = format!("flac {}: {}", output.status, tail(&output.stderr));
        if output.status.code().is_none() {
            return Err(EnvironmentError::FlacKilled(detail).into());
        }
        Err(Refusal::new(failure, detail).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn versions_parse_to_major_and_minor() {
        assert_eq!(parse_version("flac 1.4.3\n"), Some((1, 4)));
        assert_eq!(parse_version("flac 1.3.4"), Some((1, 3)));
        assert_eq!(parse_version("flac 1.5.0"), Some((1, 5)));
        assert_eq!(parse_version("flac 2.0"), Some((2, 0)));
        assert_eq!(parse_version("flac"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("flac one.two"), None);
        assert_eq!(parse_version("echo (GNU coreutils) 9.5"), None);
    }

    #[test]
    fn the_minimum_is_1_4() {
        assert!((1, 3) < MIN_VERSION);
        assert!((1, 4) >= MIN_VERSION);
        assert!((2, 0) >= MIN_VERSION);
    }

    #[test]
    fn long_stderr_is_cut_from_the_front_on_a_char_boundary() {
        let text = "ä".repeat(500);
        let cut = tail(text.as_bytes());
        assert!(cut.starts_with("..."));
        assert!(cut.len() <= DETAIL_LIMIT + 3);
    }
}
