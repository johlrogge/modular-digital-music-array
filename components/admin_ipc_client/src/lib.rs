//! IPC Client for mdma-admin
//!
//! NNG client wrapper for connecting to the admin service.
//! Used by mdma-cli and mdma-console.

pub use admin_ipc_protocol::{AdminRequest, AdminResponse};

use thiserror::Error;

/// Errors that can occur when communicating with the admin service.
#[derive(Debug, Error)]
pub enum AdminClientError {
    #[error("{0}")]
    Transport(#[from] nng_transport::NngClientError),

    #[error("Protocol error: {0}")]
    Protocol(String),
}

impl From<serde_json::Error> for AdminClientError {
    fn from(e: serde_json::Error) -> Self {
        AdminClientError::Transport(nng_transport::NngClientError::Serialization(e))
    }
}

impl From<nng::Error> for AdminClientError {
    fn from(e: nng::Error) -> Self {
        AdminClientError::Transport(nng_transport::NngClientError::Nng(e))
    }
}

impl From<nng_transport::ConnectionError> for AdminClientError {
    fn from(e: nng_transport::ConnectionError) -> Self {
        AdminClientError::Transport(nng_transport::NngClientError::Connection(e))
    }
}

/// Current boot and service-mode status from the admin service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceModeStatus {
    /// Raw boot order value from rpi-eeprom-config.
    pub boot_order: String,
    /// True if boot_order differs from the NVMe-first canonical value.
    pub service_mode_armed: bool,
    /// PCIE_PROBE value from rpi-eeprom-config.
    pub pcie_probe: String,
}

/// Client for connecting to the admin service.
pub struct AdminClient {
    socket: nng::Socket,
}

impl AdminClient {
    /// Connect to the admin service at the given address.
    ///
    /// Supports both IPC (`ipc:///path/to/socket`) and TCP (`tcp://host:port`).
    /// For TCP addresses, hostnames are resolved to IPv4 addresses.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use admin_ipc_client::AdminClient;
    ///
    /// let client = AdminClient::connect("tcp://mdma-909.local:5555")?;
    /// # Ok::<(), admin_ipc_client::AdminClientError>(())
    /// ```
    pub fn connect(address: &str) -> Result<Self, AdminClientError> {
        let socket = nng_transport::connect(address)?;
        Ok(Self { socket })
    }

    /// Send a request and receive a response.
    fn request(&self, request: &AdminRequest) -> Result<AdminResponse, AdminClientError> {
        Ok(nng_transport::request_response(&self.socket, request)?)
    }

    // =========================================================================
    // Convenience Methods
    // =========================================================================

    /// Ping the admin service to check if it's alive.
    pub fn ping(&self) -> Result<(), AdminClientError> {
        match self.request(&AdminRequest::Ping)? {
            AdminResponse::Pong => Ok(()),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!(
                "Unexpected response to Ping: {other:?}"
            ))),
        }
    }

    /// Query current service-mode status and boot configuration.
    pub fn service_mode_status(&self) -> Result<ServiceModeStatus, AdminClientError> {
        match self.request(&AdminRequest::ServiceModeStatus)? {
            AdminResponse::Status {
                boot_order,
                service_mode_armed,
                pcie_probe,
            } => Ok(ServiceModeStatus {
                boot_order,
                service_mode_armed,
                pcie_probe,
            }),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!(
                "Unexpected response to ServiceModeStatus: {other:?}"
            ))),
        }
    }

    /// Arm service mode: configure boot order to SD-first on next reboot.
    pub fn service_mode_enable(&self) -> Result<(), AdminClientError> {
        match self.request(&AdminRequest::ServiceModeEnable)? {
            AdminResponse::Ok => Ok(()),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!(
                "Unexpected response to ServiceModeEnable: {other:?}"
            ))),
        }
    }

    /// Disarm service mode: restore NVMe-first boot order.
    pub fn service_mode_disable(&self) -> Result<(), AdminClientError> {
        match self.request(&AdminRequest::ServiceModeDisable)? {
            AdminResponse::Ok => Ok(()),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!(
                "Unexpected response to ServiceModeDisable: {other:?}"
            ))),
        }
    }

    /// Trigger a system reboot.
    pub fn reboot(&self) -> Result<(), AdminClientError> {
        match self.request(&AdminRequest::Reboot)? {
            AdminResponse::Ok => Ok(()),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!(
                "Unexpected response to Reboot: {other:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_client_error_transport_display() {
        let err = AdminClientError::Protocol("sudo denied".to_string());
        assert!(err.to_string().contains("sudo denied"));
    }

    #[test]
    fn service_mode_status_fields() {
        let status = ServiceModeStatus {
            boot_order: "0xf164".to_string(),
            service_mode_armed: false,
            pcie_probe: "1".to_string(),
        };
        assert_eq!(status.boot_order, "0xf164");
        assert!(!status.service_mode_armed);
        assert_eq!(status.pcie_probe, "1");
    }

    #[test]
    fn admin_response_pong_maps_to_ok() {
        // Simulate what ping() does without a live socket
        let resp = AdminResponse::Pong;
        let result: Result<(), AdminClientError> = match resp {
            AdminResponse::Pong => Ok(()),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!(
                "Unexpected response to Ping: {other:?}"
            ))),
        };
        assert!(result.is_ok());
    }

    #[test]
    fn admin_response_error_maps_to_protocol_error() {
        let resp = AdminResponse::Error {
            message: "permission denied".to_string(),
        };
        let result: Result<(), AdminClientError> = match resp {
            AdminResponse::Pong => Ok(()),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!("Unexpected: {other:?}"))),
        };
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("permission denied"));
    }

    #[test]
    fn admin_response_status_maps_to_service_mode_status() {
        let resp = AdminResponse::Status {
            boot_order: "0xf461".to_string(),
            service_mode_armed: true,
            pcie_probe: "0".to_string(),
        };
        let result: Result<ServiceModeStatus, AdminClientError> = match resp {
            AdminResponse::Status {
                boot_order,
                service_mode_armed,
                pcie_probe,
            } => Ok(ServiceModeStatus {
                boot_order,
                service_mode_armed,
                pcie_probe,
            }),
            AdminResponse::Error { message } => Err(AdminClientError::Protocol(message)),
            other => Err(AdminClientError::Protocol(format!("Unexpected: {other:?}"))),
        };
        let status = result.unwrap();
        assert_eq!(status.boot_order, "0xf461");
        assert!(status.service_mode_armed);
        assert_eq!(status.pcie_probe, "0");
    }
}
