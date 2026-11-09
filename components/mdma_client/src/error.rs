//! Error mapping helpers between gateway and service-specific error types

use library_ipc_client::{ClientError, ProtocolError};

/// Map gateway client errors to library client errors.
///
/// Gateway errors are the same underlying type as media client errors
/// (`NngClientError`), but library client errors have an extra `Protocol`
/// variant. An "unsupported request" `Service` message (an old gateway
/// rejecting a request it cannot parse) becomes `Protocol::InvalidRequest`,
/// the same as a library that rejects it with
/// `LibraryResponse::Error(ProtocolError::InvalidRequest)`; other `Service` messages become
/// `Protocol::Internal`.
pub fn map_gw_to_lib_error(e: gateway_client::ClientError) -> ClientError {
    match e {
        nng_transport::NngClientError::Service(msg)
            if gateway_client::is_unsupported_request(&msg) =>
        {
            ClientError::Protocol(ProtocolError::InvalidRequest { message: msg })
        }
        nng_transport::NngClientError::Service(msg) => {
            ClientError::Protocol(ProtocolError::Internal { message: msg })
        }
        other => ClientError::Transport(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nng_transport::NngClientError;

    #[test]
    fn old_gateway_rejection_is_unsupported_request() {
        let e = NngClientError::Service(
            "Invalid request: unknown variant `ListMarked`, expected one of".to_string(),
        );
        assert!(map_gw_to_lib_error(e).is_unsupported_request());
    }

    #[test]
    fn other_service_message_is_not_unsupported_request() {
        let e = NngClientError::Service("library service unreachable".to_string());
        let mapped = map_gw_to_lib_error(e);
        assert!(!mapped.is_unsupported_request());
        assert!(matches!(
            mapped,
            ClientError::Protocol(ProtocolError::Internal { .. })
        ));
    }
}
