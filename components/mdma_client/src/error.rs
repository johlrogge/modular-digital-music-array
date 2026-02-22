//! Error mapping helpers between gateway and service-specific error types

use library_ipc_client::{ClientError, ProtocolError};

/// Map gateway client errors to library client errors.
pub fn map_gw_to_lib_error(e: gateway_client::ClientError) -> ClientError {
    match e {
        gateway_client::ClientError::Connection(e) => ClientError::Connection(e),
        gateway_client::ClientError::Nng(e) => ClientError::Nng(e),
        gateway_client::ClientError::Serialization(e) => ClientError::Serialization(e),
        gateway_client::ClientError::Gateway(msg) => {
            ClientError::Protocol(ProtocolError::Internal { message: msg })
        }
    }
}

/// Map gateway client errors to media client errors.
pub fn map_gw_to_media_error(e: gateway_client::ClientError) -> media_client::ClientError {
    match e {
        gateway_client::ClientError::Connection(e) => media_client::ClientError::Connection(e),
        gateway_client::ClientError::Nng(e) => media_client::ClientError::Nng(e),
        gateway_client::ClientError::Serialization(e) => {
            media_client::ClientError::Serialization(e)
        }
        gateway_client::ClientError::Gateway(msg) => media_client::ClientError::Command(msg),
    }
}
