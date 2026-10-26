//! IPC Protocol types for the admin service.
//!
//! Pure types with no network dependencies. Shared between:
//! - mdma-admin (server)
//! - mdma-console (consumer via gateway)
//! - mdma-cli (consumer via gateway)
//! - gateway envelope routing

use boot_health::BootHealth;
use serde::{Deserialize, Serialize};

/// Prefix of the `AdminResponse::Error` message when a reboot is refused by
/// the boot-health guard (as opposed to an operational failure).
pub const REBOOT_REFUSED_PREFIX: &str = "reboot refused";

/// Whether an admin error message is a boot-health reboot refusal.
pub fn is_reboot_refusal(message: &str) -> bool {
    message.starts_with(REBOOT_REFUSED_PREFIX)
}

// ============================================================================
// Request Types
// ============================================================================

/// Requests that can be sent to the admin service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum AdminRequest {
    /// Ping to check if service is alive.
    Ping,
    /// Query current service-mode status and boot configuration.
    ServiceModeStatus,
    /// Arm service mode: configure boot order to SD-first on next reboot.
    ServiceModeEnable,
    /// Disarm service mode: restore NVMe-first boot order.
    ServiceModeDisable,
    /// Report whether the unit can safely reboot into its installed kernel.
    BootHealth,
    /// Trigger a system reboot. Refused unless boot health allows it,
    /// unless `force` is set.
    Reboot {
        #[serde(default)]
        force: bool,
    },
}

// ============================================================================
// Response Types
// ============================================================================

/// Responses from the admin service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum AdminResponse {
    /// Pong response to Ping.
    Pong,
    /// Current boot and service-mode status.
    Status {
        /// Raw boot order value from rpi-eeprom-config, e.g. "0xf164" or "0xf461".
        boot_order: String,
        /// True if boot_order differs from the NVMe-first canonical value.
        service_mode_armed: bool,
        /// PCIE_PROBE value from rpi-eeprom-config, e.g. "1" or "0".
        pcie_probe: String,
    },
    /// Boot health verdict (reply to `BootHealth`).
    BootHealth { verdict: BootHealth },
    /// Success with no additional data (Enable/Disable/Reboot).
    Ok,
    /// Error response for any failure path.
    Error { message: String },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ---- AdminRequest -------------------------------------------------------

    #[test]
    fn reboot_refusal_is_recognised_by_prefix() {
        assert!(is_reboot_refusal("reboot refused: /boot is not vfat"));
        assert!(!is_reboot_refusal("failed to determine boot health: x"));
    }

    #[test]
    fn admin_request_roundtrip() {
        let variants: Vec<AdminRequest> = vec![
            AdminRequest::Ping,
            AdminRequest::ServiceModeStatus,
            AdminRequest::ServiceModeEnable,
            AdminRequest::ServiceModeDisable,
            AdminRequest::BootHealth,
            AdminRequest::Reboot { force: false },
            AdminRequest::Reboot { force: true },
        ];
        for req in variants {
            let json = serde_json::to_string(&req).unwrap();
            let parsed: AdminRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, req, "roundtrip failed for: {json}");
        }
    }

    #[test]
    fn admin_request_ping_has_type_tag() {
        let req = AdminRequest::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"Ping\""), "json was: {json}");
    }

    #[test]
    fn admin_request_service_mode_status_has_type_tag() {
        let req = AdminRequest::ServiceModeStatus;
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"type\":\"ServiceModeStatus\""),
            "json was: {json}"
        );
    }

    #[test]
    fn admin_request_service_mode_enable_has_type_tag() {
        let req = AdminRequest::ServiceModeEnable;
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"type\":\"ServiceModeEnable\""),
            "json was: {json}"
        );
    }

    #[test]
    fn admin_request_service_mode_disable_has_type_tag() {
        let req = AdminRequest::ServiceModeDisable;
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"type\":\"ServiceModeDisable\""),
            "json was: {json}"
        );
    }

    #[test]
    fn admin_request_reboot_has_type_tag() {
        let req = AdminRequest::Reboot { force: false };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"Reboot\""), "json was: {json}");
    }

    #[test]
    fn admin_request_reboot_without_force_field_defaults_to_false() {
        // Wire format of older clients that predate the `force` field.
        let parsed: AdminRequest = serde_json::from_str(r#"{"type":"Reboot"}"#).unwrap();
        assert_eq!(parsed, AdminRequest::Reboot { force: false });
    }

    #[test]
    fn admin_response_boot_health_roundtrip() {
        let resp = AdminResponse::BootHealth {
            verdict: BootHealth::KernelImageStale {
                pkg: "rpi-kernel-6.12.95_1".to_string(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"BootHealth\""), "json was: {json}");
        let parsed: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, resp);
    }

    // ---- AdminResponse ------------------------------------------------------

    #[test]
    fn admin_response_roundtrip() {
        let variants: Vec<AdminResponse> = vec![
            AdminResponse::Pong,
            AdminResponse::Status {
                boot_order: "0xf164".to_string(),
                service_mode_armed: false,
                pcie_probe: "1".to_string(),
            },
            AdminResponse::Status {
                boot_order: "0xf461".to_string(),
                service_mode_armed: true,
                pcie_probe: "0".to_string(),
            },
            AdminResponse::Ok,
            AdminResponse::Error {
                message: "rpi-eeprom-config not found".to_string(),
            },
        ];
        for resp in variants {
            let json = serde_json::to_string(&resp).unwrap();
            let parsed: AdminResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, resp, "roundtrip failed for: {json}");
        }
    }

    #[test]
    fn admin_response_pong_has_type_tag() {
        let resp = AdminResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"Pong\""), "json was: {json}");
    }

    #[test]
    fn admin_response_status_has_type_tag_and_fields() {
        let resp = AdminResponse::Status {
            boot_order: "0xf164".to_string(),
            service_mode_armed: false,
            pcie_probe: "1".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"Status\""), "json was: {json}");
        assert!(
            json.contains("\"boot_order\":\"0xf164\""),
            "json was: {json}"
        );
        assert!(
            json.contains("\"service_mode_armed\":false"),
            "json was: {json}"
        );
        assert!(json.contains("\"pcie_probe\":\"1\""), "json was: {json}");
        let parsed: AdminResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            AdminResponse::Status {
                boot_order,
                service_mode_armed,
                pcie_probe,
            } => {
                assert_eq!(boot_order, "0xf164");
                assert!(!service_mode_armed);
                assert_eq!(pcie_probe, "1");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn admin_response_ok_has_type_tag() {
        let resp = AdminResponse::Ok;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"Ok\""), "json was: {json}");
    }

    #[test]
    fn admin_response_error_has_type_tag_and_message() {
        let resp = AdminResponse::Error {
            message: "sudo denied".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"Error\""), "json was: {json}");
        assert!(
            json.contains("\"message\":\"sudo denied\""),
            "json was: {json}"
        );
        let parsed: AdminResponse = serde_json::from_str(&json).unwrap();
        match parsed {
            AdminResponse::Error { message } => assert_eq!(message, "sudo denied"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
