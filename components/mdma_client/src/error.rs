//! Error mapping helpers between gateway and service-specific error types

use library_ipc_client::{ClientError, ProtocolError};

/// Map gateway client errors to library client errors.
///
/// Gateway errors are the same underlying type as media client errors
/// (`NngClientError`), but library client errors have an extra `Protocol`
/// variant. `Service` messages from the gateway become `Protocol::Internal`.
pub fn map_gw_to_lib_error(e: gateway_client::ClientError) -> ClientError {
    match e {
        nng_transport::NngClientError::Service(msg) => {
            ClientError::Protocol(ProtocolError::Internal { message: msg })
        }
        other => ClientError::Transport(other),
    }
}
