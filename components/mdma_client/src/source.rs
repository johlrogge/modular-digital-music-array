//! Source client abstraction — works in both gateway and direct mode.

use gateway_client::SourceName;
use nng_transport::NngClientError;
use source_protocol::{SourceRequest, SourceResponse};

/// Abstraction for sending source requests, works in both gateway and direct mode.
pub enum SourceClient {
    Gateway(gateway_client::GatewayClient),
    Direct(nng::Socket),
}

impl SourceClient {
    /// Connect directly to a source IPC socket.
    pub fn connect_direct(sources_dir: &str, name: &str) -> Result<Self, NngClientError> {
        let socket_path = format!("ipc://{}/{}.sock", sources_dir, name);
        let socket = nng_transport::connect(&socket_path)?;
        Ok(SourceClient::Direct(socket))
    }

    /// Connect to a source via gateway.
    pub fn connect_gateway(gateway: &str) -> Result<Self, NngClientError> {
        let gw = gateway_client::GatewayClient::connect(gateway)?;
        Ok(SourceClient::Gateway(gw))
    }

    /// Connect using gateway if provided, otherwise direct.
    pub fn connect(
        gateway: Option<&str>,
        sources_dir: &str,
        name: &str,
    ) -> Result<Self, NngClientError> {
        match gateway {
            Some(gw) => Self::connect_gateway(gw),
            None => Self::connect_direct(sources_dir, name),
        }
    }

    /// Send a request to the source service.
    pub fn request(
        &self,
        name: &str,
        req: &SourceRequest,
    ) -> Result<SourceResponse, NngClientError> {
        match self {
            SourceClient::Gateway(gw) => gw.source_request(name, req),
            SourceClient::Direct(socket) => {
                let data = serde_json::to_vec(req)?;
                let msg = nng::Message::from(&data[..]);
                socket.send(msg).map_err(|(_, e)| NngClientError::Nng(e))?;
                let resp_msg = socket.recv()?;
                let response = serde_json::from_slice(&resp_msg)?;
                Ok(response)
            }
        }
    }
}

/// List available sources (gateway or directory scan).
pub fn list_available_sources(
    gateway: Option<&str>,
    sources_dir: &str,
) -> Result<Vec<SourceName>, NngClientError> {
    if let Some(gw_addr) = gateway {
        let gw = gateway_client::GatewayClient::connect(gw_addr)?;
        gw.list_sources()
    } else {
        let entries = match std::fs::read_dir(sources_dir) {
            Ok(e) => e,
            Err(_) => return Ok(vec![]),
        };

        Ok(entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension()?.to_str()? == "sock" {
                    path.file_stem()?.to_str().map(|s| SourceName::new(s))
                } else {
                    None
                }
            })
            .collect())
    }
}
