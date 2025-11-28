// bases/beacon/src/config.rs
use crate::actions::ExecutionMode;
use clap::Parser;

/// Beacon configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Port to listen on
    pub port: u16,
    
    /// Execution mode (DryRun or Apply)
    pub execution_mode: ExecutionMode,
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
            80  // Production
        } else {
            8080  // Development/check mode
        };
        
        let port = args.port.unwrap_or(default_port);
        
        Self {
            port,
            execution_mode,
        }
    }

    /// Check if running in check mode (safe)
    pub fn is_check_mode(&self) -> bool {
        self.execution_mode == ExecutionMode::DryRun
    }
    
    /// Check if running in apply mode (dangerous)
    pub fn is_apply_mode(&self) -> bool {
        self.execution_mode == ExecutionMode::Apply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_check_mode() {
        let args = CliArgs {
            port: None,
            apply: false,
            check: false,
        };
        let config = Config::from_args(args);
        assert_eq!(config.execution_mode, ExecutionMode::DryRun);
        assert_eq!(config.port, 8080);
        assert!(config.is_check_mode());
    }

    #[test]
    fn apply_flag_enables_changes() {
        let args = CliArgs {
            port: None,
            apply: true,
            check: false,
        };
        let config = Config::from_args(args);
        assert_eq!(config.execution_mode, ExecutionMode::Apply);
        assert_eq!(config.port, 80);
        assert!(config.is_apply_mode());
    }

    #[test]
    fn explicit_check_flag() {
        let args = CliArgs {
            port: None,
            apply: false,
            check: true,
        };
        let config = Config::from_args(args);
        assert_eq!(config.execution_mode, ExecutionMode::DryRun);
        assert!(config.is_check_mode());
    }

    #[test]
    fn custom_port_overrides_default() {
        let args = CliArgs {
            port: Some(3000),
            apply: false,
            check: false,
        };
        let config = Config::from_args(args);
        assert_eq!(config.port, 3000);
    }
}
