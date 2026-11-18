//! Error mapping helpers between gateway and service-specific error types

use library_ipc_client::{ClientError, ProtocolError};
use nng_transport::ConnectionError;

/// The text shown when a node rejects a request it does not understand; `client`
/// names the program that sent it (e.g. "CLI").
pub fn upgrade_node_text(client: &str) -> String {
    format!(
        "The node did not understand this request; it may be older than this {client} \
         — upgrade the node."
    )
}

/// Why connecting to `what` failed, as one layer: `Failed to connect to
/// <what> at <address>: <OS error>` for a failed dial, else `Failed to connect
/// to <what>: <error>`.
pub fn connect_failure(what: &str, err: &ConnectionError) -> String {
    match err {
        ConnectionError::Dial { address, cause } => {
            format!("Failed to connect to {what} at {address}: {cause}")
        }
        other => connect_failure_text(what, other),
    }
}

/// `Failed to connect to <what>: <err>`, for a failure without a dialed address.
pub fn connect_failure_text(what: &str, err: &dyn std::fmt::Display) -> String {
    format!("Failed to connect to {what}: {err}")
}

/// How a source is named in messages: `source 'bandcamp'`.
pub fn source_label(name: &str) -> String {
    format!("source '{name}'")
}

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

    fn refused_at(address: &str) -> ConnectionError {
        ConnectionError::Dial {
            address: address.to_string(),
            cause: nng::Error::ConnectionRefused,
        }
    }

    #[test]
    fn connect_failure_names_what_and_where_once() {
        assert_eq!(
            connect_failure("library", &refused_at("tcp://10.0.0.2:5555")),
            "Failed to connect to library at tcp://10.0.0.2:5555: Connection refused"
        );
    }

    #[test]
    fn connect_failure_without_an_address_keeps_the_cause() {
        assert_eq!(
            connect_failure(
                "library",
                &ConnectionError::DnsResolution("no host".to_string())
            ),
            "Failed to connect to library: DNS resolution failed: no host"
        );
    }

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
