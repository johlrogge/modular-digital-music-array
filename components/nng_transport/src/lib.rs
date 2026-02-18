//! NNG Transport utilities
//!
//! Shared NNG connection logic with hostname resolution for mDNS (.local) support.

use thiserror::Error;

/// Errors that can occur during connection.
#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("DNS resolution failed: {0}")]
    DnsResolution(String),
}

/// Connect to an NNG address with hostname resolution.
///
/// Supports both IPC (`ipc:///path/to/socket`) and TCP (`tcp://host:port`).
/// For TCP addresses with hostnames, resolves to IPv4 using:
/// - `avahi-resolve` for mDNS (.local) hostnames
/// - Standard DNS for other hostnames
///
/// # Examples
///
/// ```no_run
/// use nng_transport::connect;
///
/// // IPC connection
/// let socket = connect("ipc:///run/mdma/library.sock")?;
///
/// // TCP with mDNS hostname
/// let socket = connect("tcp://mdma-909.local:5555")?;
///
/// // TCP with IP address
/// let socket = connect("tcp://192.168.0.171:5555")?;
/// # Ok::<(), nng_transport::ConnectionError>(())
/// ```
pub fn connect(address: &str) -> Result<nng::Socket, ConnectionError> {
    let resolved_address = resolve_tcp_hostname(address)?;

    let socket = nng::Socket::new(nng::Protocol::Req0)?;
    socket.dial(&resolved_address).map_err(|e| {
        ConnectionError::ConnectionFailed(format!("Failed to connect to {}: {}", address, e))
    })?;
    Ok(socket)
}

/// Resolve hostname in TCP addresses to IPv4.
///
/// NNG doesn't handle DNS resolution, so we need to resolve hostnames
/// before passing the address to NNG.
///
/// Transforms `tcp://hostname:port` to `tcp://ip:port`.
/// IPC addresses are passed through unchanged.
pub fn resolve_tcp_hostname(address: &str) -> Result<String, ConnectionError> {
    // Only process TCP addresses
    let Some(rest) = address.strip_prefix("tcp://") else {
        return Ok(address.to_string());
    };

    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => return Ok(address.to_string()),
    };

    // If host looks like an IP address, pass through
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Ok(address.to_string());
    }

    // For .local hostnames, use avahi-resolve for mDNS
    let ip = if host.ends_with(".local") {
        resolve_with_avahi(host).or_else(|| resolve_with_std(host))
    } else {
        resolve_with_std(host)
    }
    .ok_or_else(|| ConnectionError::DnsResolution(format!("No IPv4 address found for {}", host)))?;

    Ok(format!("tcp://{}:{}", ip, port))
}

/// Resolve using `avahi-resolve` (mDNS for .local hostnames)
fn resolve_with_avahi(host: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new("avahi-resolve")
        .args(["-4", "-n", host])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // avahi-resolve output: "mdma-909.local\t192.168.0.171"
    stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .map(|ip| ip.to_string())
}

/// Resolve using standard library (for regular DNS)
fn resolve_with_std(host: &str) -> Option<String> {
    use std::net::ToSocketAddrs;

    let socket_addr = format!("{}:0", host);
    socket_addr
        .to_socket_addrs()
        .ok()?
        .find(|addr| addr.is_ipv4())
        .map(|addr| addr.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_address_unchanged() {
        let addr = "ipc:///run/mdma/test.sock";
        assert_eq!(resolve_tcp_hostname(addr).unwrap(), addr);
    }

    #[test]
    fn tcp_ip_address_unchanged() {
        let addr = "tcp://192.168.0.1:5555";
        assert_eq!(resolve_tcp_hostname(addr).unwrap(), addr);
    }

    #[test]
    fn tcp_localhost_resolved() {
        let addr = "tcp://localhost:5555";
        let resolved = resolve_tcp_hostname(addr).unwrap();
        assert!(resolved.starts_with("tcp://127."));
    }
}
