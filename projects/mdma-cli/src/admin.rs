use crate::args::impl_arg;
use crate::ctx::Ctx;
use crate::output::{interpret_admin, success_unless};
use color_eyre::Result;
use gateway_client::{AdminRequest, AdminResponse, GatewayClient};
use hicli::Success;

/// Send an admin request and interpret the reply with [`interpret_admin`].
fn admin_call<T>(
    admin: &GatewayClient,
    request: &AdminRequest,
    accept: impl FnOnce(AdminResponse) -> Option<T>,
) -> Result<T> {
    interpret_admin(admin.admin_request(request), accept)
}

/// The response a request answered with a plain `Ok` must be.
fn accept_ok(response: AdminResponse) -> Option<()> {
    matches!(response, AdminResponse::Ok).then_some(())
}

/// Like [`admin_call`] for requests that are answered with a plain `Ok`.
fn admin_call_ok(admin: &GatewayClient, request: &AdminRequest) -> Result<()> {
    admin_call(admin, request, accept_ok)
}

/// Service mode subcommands (EEPROM boot order)
#[hicli::group]
mod service_mode {
    use super::*;

    /// Show current EEPROM boot order, armed state, and PCIE_PROBE
    #[hicli::command]
    pub(crate) fn status(ctx: &Ctx) -> Result<()> {
        let admin = ctx.admin()?;
        let (boot_order, service_mode_armed, pcie_probe) = admin_call(
            &admin,
            &AdminRequest::ServiceModeStatus,
            |response| match response {
                AdminResponse::Status {
                    boot_order,
                    service_mode_armed,
                    pcie_probe,
                } => Some((boot_order, service_mode_armed, pcie_probe)),
                _ => None,
            },
        )?;
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
        let admin = ctx.admin()?;
        admin_call_ok(&admin, &AdminRequest::ServiceModeEnable)?;
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
        let admin = ctx.admin()?;
        admin_call_ok(&admin, &AdminRequest::ServiceModeDisable)?;
        println!("Service mode disarmed. NVMe-first boot order restored.");
        Ok(())
    }
}

/// Report whether the host can safely reboot into its installed kernel
#[hicli::command]
pub(crate) fn boot_health(ctx: &Ctx) -> Result<Success> {
    let admin = ctx.admin()?;
    let verdict = admin_call(
        &admin,
        &AdminRequest::BootHealth,
        |response| match response {
            AdminResponse::BootHealth { verdict } => Some(verdict),
            _ => None,
        },
    )?;
    println!("Boot health: {verdict}");
    Ok(success_unless(!verdict.allows_reboot()))
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
    let admin = ctx.admin()?;
    admin_call_ok(&admin, &AdminRequest::Reboot { force })?;
    println!("Reboot initiated. Connection will drop within ~2 seconds.");
    Ok(())
}

#[cfg(test)]
mod tests {

    use crate::test_support::{parses, run_arg};

    use super::Force;

    #[test]
    fn force_arg_parser_is_a_switch() {
        assert_eq!(run_arg::<Force>(&[]), Ok(Force(false)));
        assert_eq!(run_arg::<Force>(&["--force"]), Ok(Force(true)));
        assert!(run_arg::<Force>(&["--forse"]).is_err());
    }

    // ── admin command parsing ────────────────────────────────────────────────

    #[test]
    fn admin_service_mode_commands_parse_correctly() {
        for sub in ["status", "enable", "disable"] {
            assert_eq!(
                parses(&["admin", "service-mode", sub]),
                ["admin", "service-mode", sub]
            );
        }
    }

    #[test]
    fn admin_boot_health_parses_correctly() {
        assert_eq!(parses(&["admin", "boot-health"]), ["admin", "boot-health"]);
    }

    #[test]
    fn admin_reboot_parses_correctly() {
        assert_eq!(parses(&["admin", "reboot"]), ["admin", "reboot"]);
        assert_eq!(parses(&["admin", "reboot", "--force"]), ["admin", "reboot"]);
    }
}
