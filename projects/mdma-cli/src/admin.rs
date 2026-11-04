use crate::args::impl_arg;
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
        Command::BootHealth => boot_health(ctx),
        Command::Reboot { force } => reboot(Force(force), ctx),
    }
}

/// Send an admin request and hand the response to `accept`. A response it
/// declines, an error response, or a failed request is reported and exits 1.
fn admin_call<T>(
    admin: &GatewayClient,
    request: &AdminRequest,
    accept: impl FnOnce(AdminResponse) -> std::result::Result<T, AdminResponse>,
) -> T {
    match admin.admin_request(request) {
        Ok(AdminResponse::Error { message }) => {
            eprintln!("error: {}", message);
            std::process::exit(1);
        }
        Ok(response) => accept(response).unwrap_or_else(|other| {
            eprintln!("unexpected response: {:?}", other);
            std::process::exit(1);
        }),
        Err(e) => {
            eprintln!("request failed: {}", e);
            std::process::exit(1);
        }
    }
}

/// Like [`admin_call`] for requests that are answered with a plain `Ok`.
fn admin_call_ok(admin: &GatewayClient, request: &AdminRequest) {
    admin_call(admin, request, |response| match response {
        AdminResponse::Ok => Ok(()),
        other => Err(other),
    });
}

/// Service mode subcommands (EEPROM boot order)
#[hicli::group]
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
        match command {
            Command::Status => status(ctx),
            Command::Enable => enable(ctx),
            Command::Disable => disable(ctx),
        }
    }

    /// Show current EEPROM boot order, armed state, and PCIE_PROBE
    #[hicli::command]
    pub(crate) fn status(ctx: &Ctx) -> Result<()> {
        let admin = ctx.admin();
        let (boot_order, service_mode_armed, pcie_probe) = admin_call(
            &admin,
            &AdminRequest::ServiceModeStatus,
            |response| match response {
                AdminResponse::Status {
                    boot_order,
                    service_mode_armed,
                    pcie_probe,
                } => Ok((boot_order, service_mode_armed, pcie_probe)),
                other => Err(other),
            },
        );
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
                "Service mode:      not armed. \
                 Use `mdma admin service-mode enable` to arm it."
            );
        }
        println!("PCIE_PROBE:        {}", pcie_probe);
        Ok(())
    }

    /// Arm service mode: next reboot will boot from SD (beacon)
    #[hicli::command]
    pub(crate) fn enable(ctx: &Ctx) -> Result<()> {
        let admin = ctx.admin();
        admin_call_ok(&admin, &AdminRequest::ServiceModeEnable);
        println!("Service mode armed.");
        println!(
            "Reboot the device with `mdma admin reboot` (or `sudo reboot`) \
             to enter service mode."
        );
        Ok(())
    }

    /// Disarm service mode: restore NVMe-first boot order
    #[hicli::command]
    pub(crate) fn disable(ctx: &Ctx) -> Result<()> {
        let admin = ctx.admin();
        admin_call_ok(&admin, &AdminRequest::ServiceModeDisable);
        println!("Service mode disarmed. NVMe-first boot order restored.");
        Ok(())
    }
}

/// Report whether the host can safely reboot into its installed kernel
#[hicli::command]
pub(crate) fn boot_health(ctx: &Ctx) -> Result<()> {
    let admin = ctx.admin();
    let verdict = admin_call(
        &admin,
        &AdminRequest::BootHealth,
        |response| match response {
            AdminResponse::BootHealth { verdict } => Ok(verdict),
            other => Err(other),
        },
    );
    println!("Boot health: {verdict}");
    if !verdict.allows_reboot() {
        std::process::exit(1);
    }
    Ok(())
}

/// Reboot even if the boot-health check fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Force(pub bool);

impl_arg!(
    Force,
    switch "force",
    "Reboot even if the boot-health check fails"
);

/// Reboot the host (refused if the boot partition looks unhealthy)
#[hicli::command]
pub(crate) fn reboot(force: Force, ctx: &Ctx) -> Result<()> {
    let Force(force) = force;
    let admin = ctx.admin();
    admin_call_ok(&admin, &AdminRequest::Reboot { force });
    println!("Reboot initiated. Connection will drop within ~2 seconds.");
    Ok(())
}

#[cfg(test)]
mod tests {

    use crate::test_support::{parses, run_arg};

    use super::service_mode;
    use super::Command;
    use super::Force;
    use crate::Commands;

    #[test]
    fn force_arg_parser_is_a_switch() {
        assert_eq!(run_arg::<Force>(&[]), Ok(Force(false)));
        assert_eq!(run_arg::<Force>(&["--force"]), Ok(Force(true)));
        assert!(run_arg::<Force>(&["--forse"]).is_err());
    }

    // ── admin service-mode clap parsing ──────────────────────────────────────

    #[test]
    fn admin_service_mode_enable_parses_correctly() {
        match parses(&["admin", "service-mode", "enable"]) {
            Commands::Admin {
                command:
                    Command::ServiceMode {
                        command: service_mode::Command::Enable,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_service_mode_status_parses_correctly() {
        match parses(&["admin", "service-mode", "status"]) {
            Commands::Admin {
                command:
                    Command::ServiceMode {
                        command: service_mode::Command::Status,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_service_mode_disable_parses_correctly() {
        match parses(&["admin", "service-mode", "disable"]) {
            Commands::Admin {
                command:
                    Command::ServiceMode {
                        command: service_mode::Command::Disable,
                    },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_boot_health_parses_correctly() {
        match parses(&["admin", "boot-health"]) {
            Commands::Admin {
                command: Command::BootHealth,
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }

    #[test]
    fn admin_reboot_parses_correctly() {
        match parses(&["admin", "reboot"]) {
            Commands::Admin {
                command: Command::Reboot { force: false },
            } => {}
            other => panic!("unexpected command: {:?}", other),
        }
    }
}
