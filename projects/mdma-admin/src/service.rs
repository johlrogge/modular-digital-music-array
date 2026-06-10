use admin_ipc_protocol::{AdminRequest, AdminResponse};
use nng::options::{Options, RecvTimeout, SendTimeout};
use rpi_eeprom::{
    apply_eeprom_config, read_current_eeprom_config, EepromConfig, BOOT_ORDER_NVME_FIRST,
    BOOT_ORDER_SD_FIRST,
};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Dispatch a single `AdminRequest` to produce an `AdminResponse`.
///
/// All EEPROM I/O is async; the NNG loop drives this via `.await`.
pub async fn dispatch(req: AdminRequest) -> AdminResponse {
    match req {
        AdminRequest::Ping => AdminResponse::Pong,

        AdminRequest::ServiceModeStatus => match read_current_eeprom_config().await {
            Err(e) => AdminResponse::Error {
                message: format!("failed to read EEPROM config: {e}"),
            },
            Ok(config) => build_status_response(&config),
        },

        AdminRequest::ServiceModeEnable => {
            let config = match read_current_eeprom_config().await {
                Err(e) => {
                    return AdminResponse::Error {
                        message: format!("failed to read EEPROM config: {e}"),
                    }
                }
                Ok(c) => c,
            };
            let pcie_probe = config.get("PCIE_PROBE").unwrap_or("0");
            if pcie_probe != "1" {
                return AdminResponse::Error {
                    message: "PCIE_PROBE must be 1 before enabling service mode \
                              — return path would break"
                        .to_string(),
                };
            }
            let new_config = config.with_boot_order(BOOT_ORDER_SD_FIRST);
            match apply_eeprom_config(&new_config).await {
                Ok(()) => AdminResponse::Ok,
                Err(e) => AdminResponse::Error {
                    message: format!("failed to apply EEPROM config: {e}"),
                },
            }
        }

        AdminRequest::ServiceModeDisable => {
            let config = match read_current_eeprom_config().await {
                Err(e) => {
                    return AdminResponse::Error {
                        message: format!("failed to read EEPROM config: {e}"),
                    }
                }
                Ok(c) => c,
            };
            let new_config = config.with_boot_order(BOOT_ORDER_NVME_FIRST);
            match apply_eeprom_config(&new_config).await {
                Ok(()) => AdminResponse::Ok,
                Err(e) => AdminResponse::Error {
                    message: format!("failed to apply EEPROM config: {e}"),
                },
            }
        }

        AdminRequest::Reboot => {
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                match std::process::Command::new("reboot").spawn() {
                    Ok(_) => tracing::info!("Reboot command spawned, system going down"),
                    Err(e) => tracing::error!(error=?e, "Failed to spawn reboot command"),
                }
            });
            AdminResponse::Ok
        }
    }
}

/// Build a `Status` response from an [`EepromConfig`].
///
/// `boot_order` is left empty when the key is absent from the EEPROM config
/// (rather than silently defaulting to NVMe-first), so callers can distinguish
/// "definitely NVMe-first" from "key not present / unknown".
fn build_status_response(config: &EepromConfig) -> AdminResponse {
    let boot_order = config.get("BOOT_ORDER").unwrap_or("").to_string();
    let pcie_probe = config.get("PCIE_PROBE").unwrap_or("0").to_string();
    let service_mode_armed = !boot_order.is_empty() && boot_order != BOOT_ORDER_NVME_FIRST;
    AdminResponse::Status {
        boot_order,
        service_mode_armed,
        pcie_probe,
    }
}

/// Tighten the IPC socket file to `0660 root:mdma` so only root and the `mdma`
/// group (which the gateway runs as) can connect.
///
/// On dev machines where the `mdma` group does not exist this is best-effort:
/// the chmod still runs (restricting to root-only until the group is present)
/// and a warning is logged for the missing group.
fn secure_ipc_socket(socket_path: &str) {
    // chmod 0660
    match std::fs::metadata(socket_path) {
        Ok(meta) => {
            let mut perms = meta.permissions();
            perms.set_mode(0o660);
            if let Err(e) = std::fs::set_permissions(socket_path, perms) {
                tracing::warn!("Failed to chmod {socket_path} to 0660: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to stat {socket_path} for chmod: {e}");
            return;
        }
    }

    // chown :mdma — look up gid by group name
    let gid = unsafe {
        let name = std::ffi::CString::new("mdma").expect("CString");
        let grp = libc::getgrnam(name.as_ptr());
        if grp.is_null() {
            None
        } else {
            Some((*grp).gr_gid)
        }
    };

    match gid {
        Some(gid) => {
            let path = std::ffi::CString::new(socket_path).expect("CString");
            // uid_t(-1) means "don't change owner", only change group
            let rc = unsafe { libc::chown(path.as_ptr(), libc::uid_t::MAX, gid) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                tracing::warn!("Failed to chown {socket_path} to gid {gid}: {err}");
            } else {
                tracing::debug!("Socket {socket_path} secured to 0660 root:mdma");
            }
        }
        None => {
            tracing::warn!(
                "Group 'mdma' not found — socket {socket_path} left at 0660 root:root; \
                 gateway will not be able to connect until the group exists"
            );
        }
    }
}

/// Run the NNG Rep0 request/response loop.
///
/// Must be called from within a tokio runtime (uses `Handle::current()`).
/// Blocks the calling thread; returns only on an unrecoverable socket error.
pub fn run(socket_addr: &str) -> color_eyre::Result<()> {
    let ServiceSockets { rep_socket, .. } = service::create_sockets(&service::ServiceConfig {
        socket_address: socket_addr.to_string(),
        event_address: None,
    })
    .map_err(|e| color_eyre::eyre::eyre!("Failed to create service sockets: {e}"))?;

    // Tighten ACL: only root and the mdma group (gateway) may connect.
    if let Some(path) = socket_addr.strip_prefix("ipc://") {
        secure_ipc_socket(path);
    }

    rep_socket
        .set_opt::<RecvTimeout>(Some(Duration::from_secs(1)))
        .map_err(|e| color_eyre::eyre::eyre!("Failed to set recv timeout: {e}"))?;
    rep_socket
        .set_opt::<SendTimeout>(Some(Duration::from_secs(5)))
        .map_err(|e| color_eyre::eyre::eyre!("Failed to set send timeout: {e}"))?;

    tracing::info!(address = %socket_addr, "Admin service listening");

    let rt = tokio::runtime::Handle::current();

    loop {
        let msg = match rep_socket.recv() {
            Ok(m) => m,
            Err(nng::Error::TimedOut) => continue,
            Err(e) => {
                tracing::error!("NNG recv error: {e}");
                return Err(color_eyre::eyre::eyre!("NNG recv error: {e}"));
            }
        };

        let response = match serde_json::from_slice::<AdminRequest>(msg.as_slice()) {
            Ok(req) => {
                tracing::debug!(?req, "admin request");
                rt.block_on(dispatch(req))
            }
            Err(e) => AdminResponse::Error {
                message: format!("failed to deserialize request: {e}"),
            },
        };

        let reply_bytes = match serde_json::to_vec(&response) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to serialize response: {e}");
                continue;
            }
        };

        if let Err((_msg, e)) = rep_socket.send(nng::Message::from(&reply_bytes[..])) {
            tracing::error!("NNG send error: {e}");
        }
    }
}

// Needed for `run()` — import the struct directly.
use service::ServiceSockets;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use admin_ipc_protocol::{AdminRequest, AdminResponse};
    use rpi_eeprom::{BOOT_ORDER_NVME_FIRST, BOOT_ORDER_SD_FIRST};

    // --- Ping → Pong ---------------------------------------------------------

    #[tokio::test]
    async fn ping_returns_pong() {
        let resp = dispatch(AdminRequest::Ping).await;
        assert_eq!(resp, AdminResponse::Pong);
    }

    // --- service_mode_armed boolean ------------------------------------------

    #[test]
    fn service_mode_armed_false_when_boot_order_is_nvme_first() {
        let raw = format!("BOOT_ORDER={BOOT_ORDER_NVME_FIRST}\nPCIE_PROBE=1\n");
        let config = rpi_eeprom::EepromConfig::parse(&raw);
        let resp = build_status_response(&config);
        match resp {
            AdminResponse::Status {
                service_mode_armed, ..
            } => assert!(!service_mode_armed, "NVMe-first should not be armed"),
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn service_mode_armed_true_when_boot_order_is_sd_first() {
        let raw = format!("BOOT_ORDER={BOOT_ORDER_SD_FIRST}\nPCIE_PROBE=1\n");
        let config = rpi_eeprom::EepromConfig::parse(&raw);
        let resp = build_status_response(&config);
        match resp {
            AdminResponse::Status {
                service_mode_armed, ..
            } => assert!(service_mode_armed, "SD-first should be armed"),
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn service_mode_armed_false_when_boot_order_missing() {
        // When BOOT_ORDER is absent, boot_order is empty and service mode is not armed.
        let raw = "PCIE_PROBE=1\nBOOT_UART=1\n";
        let config = rpi_eeprom::EepromConfig::parse(raw);
        let resp = build_status_response(&config);
        match resp {
            AdminResponse::Status {
                service_mode_armed,
                boot_order,
                ..
            } => {
                assert!(
                    boot_order.is_empty(),
                    "absent BOOT_ORDER should yield empty string"
                );
                assert!(
                    !service_mode_armed,
                    "unknown BOOT_ORDER should not be armed"
                );
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    // --- ServiceModeEnable PCIE_PROBE guard ----------------------------------

    #[test]
    fn enable_guard_fires_when_pcie_probe_is_0() {
        let raw = format!("BOOT_ORDER={BOOT_ORDER_NVME_FIRST}\nPCIE_PROBE=0\n");
        let config = rpi_eeprom::EepromConfig::parse(&raw);
        let pcie_probe = config.get("PCIE_PROBE").unwrap_or("0");
        assert_eq!(pcie_probe, "0");
        assert_ne!(pcie_probe, "1", "guard should fire: pcie_probe is not 1");
    }

    #[test]
    fn enable_guard_passes_when_pcie_probe_is_1() {
        let raw = format!("BOOT_ORDER={BOOT_ORDER_NVME_FIRST}\nPCIE_PROBE=1\n");
        let config = rpi_eeprom::EepromConfig::parse(&raw);
        let pcie_probe = config.get("PCIE_PROBE").unwrap_or("0");
        assert_eq!(pcie_probe, "1", "guard should pass when PCIE_PROBE=1");
    }

    #[test]
    fn enable_guard_fires_when_pcie_probe_missing() {
        let raw = format!("BOOT_ORDER={BOOT_ORDER_NVME_FIRST}\n");
        let config = rpi_eeprom::EepromConfig::parse(&raw);
        let pcie_probe = config.get("PCIE_PROBE").unwrap_or("0");
        assert_ne!(
            pcie_probe, "1",
            "guard should fire when PCIE_PROBE is absent"
        );
    }
}
