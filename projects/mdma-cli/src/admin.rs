use crate::ctx::Ctx;
use clap::Subcommand;
use color_eyre::Result;
use gateway_client::{AdminRequest, AdminResponse, GatewayClient};

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Service mode subcommands (EEPROM boot order)
    ServiceMode {
        #[command(subcommand)]
        command: service_mode::Command,
    },
    /// Report whether the host can safely reboot into its installed kernel
    BootHealth,
    /// Reboot the host (refused if the boot partition looks unhealthy)
    Reboot {
        /// Reboot even if the boot-health check fails
        #[arg(long)]
        force: bool,
    },
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::ServiceMode { command } => service_mode::dispatch(command, ctx),
        Command::BootHealth => {
            let admin = ctx.admin();
            handle_admin_boot_health(&admin)
        }
        Command::Reboot { force } => {
            let admin = ctx.admin();
            handle_admin_reboot(&admin, force)
        }
    }
}

/// Print the boot-health verdict; exit 1 when a reboot would not be safe.
fn handle_admin_boot_health(client: &GatewayClient) -> Result<()> {
    match client.admin_request(&AdminRequest::BootHealth) {
        Ok(AdminResponse::BootHealth { verdict }) => {
            println!("Boot health: {verdict}");
            if !verdict.allows_reboot() {
                std::process::exit(1);
            }
            Ok(())
        }
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(other) => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn handle_admin_reboot(client: &GatewayClient, force: bool) -> Result<()> {
    match client.admin_request(&AdminRequest::Reboot { force }) {
        Ok(AdminResponse::Ok) => {
            println!("Reboot initiated. Connection will drop within ~2 seconds.");
            Ok(())
        }
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(other) => {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
}

mod service_mode {
    use super::*;

    #[derive(Subcommand, Debug)]
    pub(crate) enum Command {
        /// Show current EEPROM boot order, armed state, and PCIE_PROBE
        Status,
        /// Arm service mode: next reboot will boot from SD (beacon)
        Enable,
        /// Disarm service mode: restore NVMe-first boot order
        Disable,
    }

    pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
        let admin = ctx.admin();
        match &command {
            Command::Status => handle_admin_service_mode_status(&admin),
            Command::Enable => handle_admin_service_mode_enable(&admin),
            Command::Disable => handle_admin_service_mode_disable(&admin),
        }
    }

    // =============================================================================
    // Admin Handlers
    // =============================================================================

    fn handle_admin_service_mode_status(client: &GatewayClient) -> Result<()> {
        match client.admin_request(&AdminRequest::ServiceModeStatus) {
            Ok(AdminResponse::Status {
                boot_order,
                service_mode_armed,
                pcie_probe,
            }) => {
                let boot_label = if service_mode_armed {
                    "SD-first"
                } else if boot_order.is_empty() {
                    "unknown"
                } else {
                    "NVMe-first"
                };
                if boot_order.is_empty() {
                    println!("Boot order:        (not set) ({})", boot_label);
                } else {
                    println!("Boot order:        {} ({})", boot_order, boot_label);
                }
                if service_mode_armed {
                    println!("Service mode:      ARMED — next reboot will land on the beacon SD");
                } else {
                    println!(
                    "Service mode:      not armed. Use `mdma admin service-mode enable` to arm it."
                );
                }
                println!("PCIE_PROBE:        {}", pcie_probe);
                Ok(())
            }
            Ok(AdminResponse::Error { message }) => {
                eprintln!("error: {}", message);
                std::process::exit(1);
            }
            Ok(other) => {
                eprintln!("unexpected response: {:?}", other);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("request failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    fn handle_admin_service_mode_enable(client: &GatewayClient) -> Result<()> {
        match client.admin_request(&AdminRequest::ServiceModeEnable) {
            Ok(AdminResponse::Ok) => {
                println!("Service mode armed.");
                println!(
                "Reboot the device with `mdma admin reboot` (or `sudo reboot`) to enter service mode."
            );
                Ok(())
            }
            Ok(AdminResponse::Error { message }) => {
                eprintln!("error: {}", message);
                std::process::exit(1);
            }
            Ok(other) => {
                eprintln!("unexpected response: {:?}", other);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("request failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    fn handle_admin_service_mode_disable(client: &GatewayClient) -> Result<()> {
        match client.admin_request(&AdminRequest::ServiceModeDisable) {
            Ok(AdminResponse::Ok) => {
                println!("Service mode disarmed. NVMe-first boot order restored.");
                Ok(())
            }
            Ok(AdminResponse::Error { message }) => {
                eprintln!("error: {}", message);
                std::process::exit(1);
            }
            Ok(other) => {
                eprintln!("unexpected response: {:?}", other);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("request failed: {}", e);
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use clap::Parser;

    use crate::Cli;

    use super::service_mode::Command as ServiceModeCommands;
    use super::Command as AdminCommands;
    use crate::Commands;

    // ── admin service-mode clap parsing ──────────────────────────────────────

    #[test]
    fn admin_service_mode_enable_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "service-mode", "enable"]);
        assert!(
            result.is_ok(),
            "clap rejected admin service-mode enable: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command:
                    AdminCommands::ServiceMode {
                        command: ServiceModeCommands::Enable,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_service_mode_status_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "service-mode", "status"]);
        assert!(
            result.is_ok(),
            "clap rejected admin service-mode status: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command:
                    AdminCommands::ServiceMode {
                        command: ServiceModeCommands::Status,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_service_mode_disable_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "service-mode", "disable"]);
        assert!(
            result.is_ok(),
            "clap rejected admin service-mode disable: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command:
                    AdminCommands::ServiceMode {
                        command: ServiceModeCommands::Disable,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_boot_health_parses_correctly() {
        let cli = Cli::try_parse_from(["mdma", "admin", "boot-health"]).unwrap();
        match cli.command {
            Commands::Admin {
                command: AdminCommands::BootHealth,
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_reboot_parses_correctly() {
        let result = Cli::try_parse_from(["mdma", "admin", "reboot"]);
        assert!(
            result.is_ok(),
            "clap rejected admin reboot: {}",
            result.unwrap_err()
        );
        let cli = result.unwrap();
        match cli.command {
            Commands::Admin {
                command: AdminCommands::Reboot { force: false },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }
}
