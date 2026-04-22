use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HttpServerError {
    #[error("TCP bind failed: {source}")]
    Bind {
        #[source]
        source: std::io::Error,
    },
    #[error("server error: {source}")]
    Serve {
        #[source]
        source: std::io::Error,
    },
}

pub struct HttpServerConfig {
    pub port: u16,
}

/// Creates a dual-stack TCP listener bound to `[::]:<port>` with `IPV6_V6ONLY=0`,
/// so a single socket accepts both IPv4 and IPv6 connections.
///
/// This is the idiomatic Linux "listen everywhere" pattern and is required when
/// mDNS resolves the host to a link-local IPv6 address (`fe80::…`) while some
/// clients connect via IPv4.
pub fn dual_stack_listener(port: u16) -> Result<tokio::net::TcpListener, std::io::Error> {
    let addr = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0));
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(false)?; // IPV6_V6ONLY = 0 — accept IPv4-mapped addresses too
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    let std_listener: std::net::TcpListener = socket.into();
    std_listener.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(std_listener)
}

/// Binds `[::]:<port>` with `IPV6_V6ONLY=0` and serves the given Axum router.
/// The dual-stack socket accepts both IPv4 and IPv6 connections on a single fd.
/// Does NOT call color_eyre::install() or tracing_subscriber::init().
pub async fn serve(router: axum::Router, config: &HttpServerConfig) -> Result<(), HttpServerError> {
    let listener =
        dual_stack_listener(config.port).map_err(|source| HttpServerError::Bind { source })?;
    axum::serve(listener, router)
        .await
        .map_err(|source| HttpServerError::Serve { source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_port_zero_succeeds() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await;
        assert!(listener.is_ok());
    }

    /// The dual-stack listener must bind [::]:0 with IPV6_V6ONLY=0 so that
    /// both IPv4 and IPv6 clients can connect on the same socket.
    /// Two sequential connect/accept pairs are exercised on the same listener:
    /// first via 127.0.0.1 (IPv4) then via [::1] (IPv6). A future regression
    /// that sets `IPV6_V6ONLY=1` would cause the IPv4 leg to fail, while a
    /// regression that accidentally binds `0.0.0.0` would cause the IPv6 leg
    /// to fail.
    #[tokio::test]
    async fn dual_stack_listener_accepts_ipv4_and_ipv6() {
        let listener = dual_stack_listener(0).expect("dual_stack_listener should succeed");
        let local_addr = listener.local_addr().expect("local_addr");
        // The bound address must be an IPv6 address ([::]:<port>)
        assert!(
            local_addr.is_ipv6(),
            "expected IPv6 local address, got {local_addr}"
        );
        let port = local_addr.port();

        // --- IPv4 leg: connect and accept concurrently ---
        let (accept_result, connect_result) = tokio::join!(
            listener.accept(),
            tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")),
        );
        let _ = connect_result.expect(
            "IPv4 connect to dual-stack socket should succeed; \
             if this fails the socket is IPv6-only",
        );
        let _ = accept_result.expect("IPv4 accept should succeed");

        // --- IPv6 leg: connect and accept concurrently ---
        let (accept_result, connect_result) = tokio::join!(
            listener.accept(),
            tokio::net::TcpStream::connect(format!("[::1]:{port}")),
        );
        let _ = connect_result.expect(
            "IPv6 connect to dual-stack socket should succeed; \
             if this fails the socket doesn't accept IPv6",
        );
        let _ = accept_result.expect("IPv6 accept should succeed");
    }
}
