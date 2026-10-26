use askama::Template;
use axum::{extract::State, response::Html, Form};
use rpi_eeprom::{BootTarget, EepromConfig};
use serde::Deserialize;
use tracing::{error, info};

use super::AppError;
use crate::server::AppState;

/// Template for the EEPROM boot order page
#[derive(Template)]
#[template(path = "boot_order.html")]
struct BootOrderTemplate {
    /// Current `BOOT_ORDER` value, or "(not set)"
    boot_order: String,
    /// Current `PCIE_PROBE` value, or "(not set)"
    pcie_probe: String,
    /// Human-readable meaning of the current `BOOT_ORDER`
    description: &'static str,
    /// Set when `rpi-eeprom-config` could not be read
    read_error: Option<String>,
    /// Success message after staging an update
    message: Option<String>,
    /// Failure message after a failed staging attempt
    failure: Option<String>,
}

/// Form body of `POST /boot-order`
#[derive(Debug, Deserialize)]
pub struct BootOrderForm {
    target: String,
}

/// Parse the form value for the requested boot target.
fn parse_target(value: &str) -> Option<BootTarget> {
    match value {
        "sd" => Some(BootTarget::SdFirst),
        "nvme" => Some(BootTarget::NvmeFirst),
        _ => None,
    }
}

/// Human-readable meaning of a boot target.
fn describe_target(target: Option<BootTarget>) -> &'static str {
    match target {
        Some(BootTarget::SdFirst) => "SD first (SD, NVMe, USB) - rescue",
        Some(BootTarget::NvmeFirst) => "NVMe first (USB, NVMe, SD) - normal",
        None => "Custom or unknown order",
    }
}

/// The values shown for the current EEPROM config.
#[derive(Debug, PartialEq, Eq)]
struct CurrentConfigView {
    boot_order: String,
    pcie_probe: String,
    description: &'static str,
}

fn current_config_view(config: &EepromConfig) -> CurrentConfigView {
    let or_unset = |v: Option<&str>| v.unwrap_or("(not set)").to_string();
    CurrentConfigView {
        boot_order: or_unset(config.get("BOOT_ORDER")),
        pcie_probe: or_unset(config.get("PCIE_PROBE")),
        description: describe_target(config.boot_target()),
    }
}

/// Message shown after a boot target has been staged.
///
/// The update is written to the bootfs of the device the beacon booted from
/// (the SD card) and is only flashed when the Pi boots from that device.
fn staged_message(target: BootTarget) -> String {
    format!(
        "Staged BOOT_ORDER={} with PCIE_PROBE=1. The update applies the next time the Pi \
         boots from this SD card: reboot with the SD card inserted.",
        target.boot_order()
    )
}

/// Message shown in CHECK mode instead of staging anything.
fn check_mode_message(target: BootTarget) -> String {
    format!(
        "CHECK mode: would stage BOOT_ORDER={} with PCIE_PROBE=1. Nothing was changed; \
         start the beacon with --apply to stage it.",
        target.boot_order()
    )
}

async fn render(
    message: Option<String>,
    failure: Option<String>,
) -> Result<Html<String>, AppError> {
    let (view, read_error) = match rpi_eeprom::read_current_eeprom_config().await {
        Ok(config) => (current_config_view(&config), None),
        Err(e) => (
            current_config_view(&EepromConfig::parse("")),
            Some(e.to_string()),
        ),
    };

    let html = BootOrderTemplate {
        boot_order: view.boot_order,
        pcie_probe: view.pcie_probe,
        description: view.description,
        read_error,
        message,
        failure,
    }
    .render()
    .map_err(|e| AppError::Template(e.to_string()))?;

    Ok(Html(html))
}

/// Handler showing the current EEPROM boot order and the switch actions
pub async fn show() -> Result<Html<String>, AppError> {
    render(None, None).await
}

/// Handler staging an EEPROM boot order change
pub async fn switch(
    State(state): State<AppState>,
    Form(form): Form<BootOrderForm>,
) -> Result<Html<String>, AppError> {
    let target = parse_target(&form.target)
        .ok_or_else(|| AppError::Validation(format!("Unknown boot target '{}'", form.target)))?;

    if state.config.is_check_mode() {
        info!(
            "CHECK mode: not staging EEPROM boot order {}",
            target.boot_order()
        );
        return render(Some(check_mode_message(target)), None).await;
    }
    info!("Staging EEPROM boot order {}", target.boot_order());

    match rpi_eeprom::stage_boot_target(target).await {
        Ok(path) => {
            info!("EEPROM update staged at {}", path.display());
            render(Some(staged_message(target)), None).await
        }
        Err(e) => {
            error!("Failed to stage EEPROM boot order: {}", e);
            render(None, Some(e.to_string())).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_accepts_known_values() {
        assert_eq!(parse_target("sd"), Some(BootTarget::SdFirst));
        assert_eq!(parse_target("nvme"), Some(BootTarget::NvmeFirst));
    }

    #[test]
    fn parse_target_rejects_unknown_values() {
        assert_eq!(parse_target(""), None);
        assert_eq!(parse_target("0xf461"), None);
        assert_eq!(parse_target("SD"), None);
    }

    #[test]
    fn current_config_view_shows_values() {
        let cfg = EepromConfig::parse("BOOT_UART=1\nBOOT_ORDER=0xf461\nPCIE_PROBE=1\n");
        assert_eq!(
            current_config_view(&cfg),
            CurrentConfigView {
                boot_order: "0xf461".to_string(),
                pcie_probe: "1".to_string(),
                description: "SD first (SD, NVMe, USB) - rescue",
            }
        );
    }

    #[test]
    fn current_config_view_handles_missing_and_unknown() {
        let cfg = EepromConfig::parse("BOOT_ORDER=0xf41\n");
        let view = current_config_view(&cfg);
        assert_eq!(view.boot_order, "0xf41");
        assert_eq!(view.pcie_probe, "(not set)");
        assert_eq!(view.description, "Custom or unknown order");
    }

    #[test]
    fn staged_message_mentions_reboot_with_sd_inserted() {
        let msg = staged_message(BootTarget::SdFirst);
        assert!(msg.contains("0xf461"));
        assert!(msg.contains("boots from this SD card"));
        assert!(msg.contains("SD card inserted"));
    }

    #[test]
    fn check_mode_message_says_nothing_was_changed() {
        let msg = check_mode_message(BootTarget::NvmeFirst);
        assert!(msg.contains("CHECK mode"));
        assert!(msg.contains("0xf164"));
        assert!(msg.contains("Nothing was changed"));
    }
}
