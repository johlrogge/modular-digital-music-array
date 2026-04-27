// bases/beacon/src/config.rs
use crate::actions::ExecutionMode;
use clap::Parser;
use std::path::PathBuf;

/// Beacon configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Port to listen on
    pub port: u16,

    /// Execution mode (DryRun or Apply)
    pub execution_mode: ExecutionMode,

    /// Path to the live log file tailed by the SSE /stream endpoint
    pub log_file: PathBuf,
}

/// MDMA Beacon - System Provisioning Tool
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct CliArgs {
    /// Port to listen on (defaults: 8080 in check mode, 80 in apply mode)
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Actually apply changes to the system (DANGEROUS!)
    ///
    /// By default, beacon runs in --check mode which only shows what would be done.
    /// Use --apply to actually partition drives and install the system.
    #[arg(long, alias = "danger")]
    pub apply: bool,

    /// Check mode (dry run) - show what would be done without making changes
    ///
    /// This is the DEFAULT mode. Only use --apply when you're ready to modify the system.
    #[arg(long, conflicts_with = "apply")]
    pub check: bool,

    /// Path to the log file to tail for the /stream SSE endpoint.
    ///
    /// Defaults to /var/log/beacon/current (svlogd managed by runit).
    /// Override in development with e.g. --log-file /tmp/beacon.log
    #[arg(long, default_value = "/var/log/beacon/current")]
    pub log_file: PathBuf,
}

impl Config {
    /// Create configuration from CLI arguments
    pub fn from_args(args: CliArgs) -> Self {
        let execution_mode = if args.apply {
            ExecutionMode::Apply
        } else {
            // Default to check mode (safe)
            ExecutionMode::DryRun
        };

        // Choose default port based on mode
        let default_port = if execution_mode == ExecutionMode::Apply {
            80 // Production
        } else {
            8080 // Development/check mode
        };

        let port = args.port.unwrap_or(default_port);

        Self {
            port,
            execution_mode,
            log_file: args.log_file,
        }
    }

    /// Check if running in check mode (safe)
    pub fn is_check_mode(&self) -> bool {
        self.execution_mode == ExecutionMode::DryRun
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn default_args() -> CliArgs {
        CliArgs {
            port: None,
            apply: false,
            check: false,
            log_file: PathBuf::from("/var/log/beacon/current"),
        }
    }

    #[test]
    fn default_is_check_mode() {
        let config = Config::from_args(default_args());
        assert_eq!(config.execution_mode, ExecutionMode::DryRun);
        assert_eq!(config.port, 8080);
        assert!(config.is_check_mode());
    }

    #[test]
    fn apply_flag_enables_changes() {
        let args = CliArgs {
            apply: true,
            ..default_args()
        };
        let config = Config::from_args(args);
        assert_eq!(config.execution_mode, ExecutionMode::Apply);
        assert_eq!(config.port, 80);
    }

    #[test]
    fn explicit_check_flag() {
        let args = CliArgs {
            check: true,
            ..default_args()
        };
        let config = Config::from_args(args);
        assert_eq!(config.execution_mode, ExecutionMode::DryRun);
        assert!(config.is_check_mode());
    }

    #[test]
    fn custom_port_overrides_default() {
        let args = CliArgs {
            port: Some(3000),
            ..default_args()
        };
        let config = Config::from_args(args);
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn custom_log_file() {
        let args = CliArgs {
            log_file: PathBuf::from("/tmp/test.log"),
            ..default_args()
        };
        let config = Config::from_args(args);
        assert_eq!(config.log_file, PathBuf::from("/tmp/test.log"));
    }

    #[test]
    fn default_log_file_is_svlogd_path() {
        let config = Config::from_args(default_args());
        assert_eq!(config.log_file, PathBuf::from("/var/log/beacon/current"));
    }
}
