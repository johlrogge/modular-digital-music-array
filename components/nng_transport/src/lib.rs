//! NNG Transport utilities
//!
//! Shared NNG connection logic with hostname resolution for mDNS (.local) support.

use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

use nng::options::{Options, RecvTimeout, SendTimeout};

/// Default send/receive timeout for NNG Req0 client sockets.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

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
/// - avahi D-Bus (`org.freedesktop.Avahi.Server.ResolveHostName`) for mDNS (.local) hostnames
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
    socket
        .set_opt::<SendTimeout>(Some(DEFAULT_TIMEOUT))
        .map_err(|e| {
            ConnectionError::ConnectionFailed(format!("Failed to set send timeout: {}", e))
        })?;
    socket
        .set_opt::<RecvTimeout>(Some(DEFAULT_TIMEOUT))
        .map_err(|e| {
            ConnectionError::ConnectionFailed(format!("Failed to set recv timeout: {}", e))
        })?;
    socket.dial(&resolved_address).map_err(|e| {
        ConnectionError::ConnectionFailed(format!("Failed to connect to {}: {}", address, e))
    })?;
    Ok(socket)
}

/// Resolve `host` to an IPv4 address string using the project's resolution
/// order: avahi-resolve subprocess for `.local` hostnames, std resolver
/// otherwise, with disk-backed cache.
///
/// If `host` already looks like an IPv4 address (digits and dots only), it is
/// returned unchanged.
///
/// # Errors
///
/// Returns [`ConnectionError::DnsResolution`] when neither avahi nor std
/// resolver can find the host and there is no cached result.
pub fn resolve_hostname_to_ipv4(host: &str) -> Result<String, ConnectionError> {
    // If host already looks like an IP address, pass through
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Ok(host.to_string());
    }

    // For .local hostnames, use avahi D-Bus for mDNS (Linux) or fall through to std (macOS)
    let resolved = if host.ends_with(".local") {
        avahi_platform::resolve(host).or_else(|| resolve_with_std(host))
    } else {
        resolve_with_std(host)
    };

    match resolved {
        Some(ip) => {
            write_cached_ip(host, &ip);
            Ok(ip)
        }
        None => {
            if let Some(cached_ip) = read_cached_ip(host) {
                return Ok(cached_ip);
            }
            Err(ConnectionError::DnsResolution(format!(
                "No IPv4 address found for {}",
                host
            )))
        }
    }
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

    let ip = resolve_hostname_to_ipv4(host)?;
    Ok(format!("tcp://{}:{}", ip, port))
}

fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("mdma").join("dns-cache"))
}

fn read_cached_ip(host: &str) -> Option<String> {
    read_cached_ip_from(host, cache_path()?)
}

fn read_cached_ip_from(host: &str, path: PathBuf) -> Option<String> {
    let content = std::fs::read_to_string(&path).ok()?;
    content.lines().find_map(|line| {
        let (h, ip) = line.split_once('\t')?;
        if h == host {
            Some(ip.to_string())
        } else {
            None
        }
    })
}

fn write_cached_ip(host: &str, ip: &str) {
    let Some(path) = cache_path() else { return };
    write_cached_ip_to(host, ip, path);
}

fn write_cached_ip_to(host: &str, ip: &str, path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut entries: Vec<(String, String)> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (h, i) = line.split_once('\t')?;
            Some((h.to_string(), i.to_string()))
        })
        .filter(|(h, _)| h != host)
        .collect();
    entries.push((host.to_string(), ip.to_string()));
    let content: String = entries
        .iter()
        .map(|(h, i)| format!("{}\t{}", h, i))
        .collect::<Vec<_>>()
        .join("\n");
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &content).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Platform-specific avahi resolution via D-Bus (Linux) or no-op stub (other).
#[cfg(target_os = "linux")]
mod avahi_platform {
    use zbus::blocking::Connection;
    use zbus::proxy;

    /// Avahi D-Bus interface constants.
    /// AVAHI_IF_UNSPEC = -1, AVAHI_PROTO_UNSPEC = -1, AVAHI_PROTO_INET = 0
    const IF_UNSPEC: i32 = -1;
    const PROTO_UNSPEC: i32 = -1;
    const PROTO_INET: i32 = 0;
    const FLAGS_NONE: u32 = 0;

    #[proxy(
        interface = "org.freedesktop.Avahi.Server",
        default_service = "org.freedesktop.Avahi",
        default_path = "/"
    )]
    trait AvahiServer {
        /// ResolveHostName(interface i, protocol i, name s, aprotocol i, flags u)
        /// Returns: (interface i, protocol i, name s, aprotocol i, address s, flags u)
        ///
        /// Signature confirmed from avahi D-Bus specification:
        /// https://avahi.org/doxygen/html/
        /// and `dbus-send --system --print-reply --dest=org.freedesktop.Avahi /
        ///   org.freedesktop.DBus.Introspectable.Introspect`
        fn resolve_host_name(
            &self,
            interface: i32,
            protocol: i32,
            name: &str,
            aprotocol: i32,
            flags: u32,
        ) -> zbus::Result<(i32, i32, String, i32, String, u32)>;
    }

    /// Resolve a `.local` hostname via the avahi-daemon D-Bus service.
    ///
    /// Returns the IPv4 address string on success, or `None` if:
    /// - avahi-daemon is not running
    /// - the hostname cannot be resolved
    /// - any D-Bus error occurs
    pub fn resolve(host: &str) -> Option<String> {
        let conn = Connection::system().ok()?;
        let proxy = AvahiServerProxyBlocking::new(&conn).ok()?;
        let resp = proxy
            .resolve_host_name(IF_UNSPEC, PROTO_UNSPEC, host, PROTO_INET, FLAGS_NONE)
            .ok()?;
        // The 5th field (index 4) of the returned tuple is the resolved address.
        Some(resp.4)
    }
}

#[cfg(not(target_os = "linux"))]
mod avahi_platform {
    /// On non-Linux platforms (macOS, etc.), avahi is not available.
    /// `.local` resolution falls through to the standard library, which
    /// works natively via mDNSResponder on macOS.
    pub fn resolve(_host: &str) -> Option<String> {
        None
    }
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

/// Send a request and receive a response over an existing NNG socket.
///
/// This is the shared serialize→send→recv→deserialize pattern used by all NNG clients.
/// The caller is responsible for connecting the socket with [`connect`].
///
/// # Errors
///
/// Returns [`NngClientError`] on serialization failure, NNG send/recv error,
/// or deserialization failure.
///
/// # Examples
///
/// ```no_run
/// use nng_transport::{connect, request_response, NngClientError};
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize)]
/// struct Ping;
///
/// #[derive(Deserialize)]
/// struct Pong;
///
/// let socket = connect("ipc:///run/mdma/library.sock")?;
/// let _pong: Pong = request_response(&socket, &Ping)?;
/// # Ok::<(), NngClientError>(())
/// ```
pub fn request_response<Req, Resp>(
    socket: &nng::Socket,
    request: &Req,
) -> Result<Resp, NngClientError>
where
    Req: serde::Serialize,
    Resp: serde::de::DeserializeOwned,
{
    let data = serde_json::to_vec(request)?;
    let msg = nng::Message::from(&data[..]);
    socket.send(msg).map_err(|(_, e)| NngClientError::Nng(e))?;
    let response_msg = socket.recv()?;
    let response: Resp = serde_json::from_slice(&response_msg)?;
    Ok(response)
}

/// Shared NNG client error type for use across all NNG-based clients.
///
/// Covers the common error cases: failed connection, NNG transport errors,
/// JSON serialization errors, and service-level errors (e.g. command rejected).
#[derive(Debug, thiserror::Error)]
pub enum NngClientError {
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),

    #[error("nng error: {0}")]
    Nng(#[from] nng::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("service error: {0}")]
    Service(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use std::time::Instant;

    fn temp_cache_path(dir: &std::path::Path) -> PathBuf {
        dir.join("mdma").join("dns-cache")
    }

    #[test]
    fn nng_client_error_from_connection_error() {
        let conn_err = ConnectionError::ConnectionFailed("test".to_string());
        let client_err: NngClientError = conn_err.into();
        assert!(client_err.to_string().contains("connection error"));
    }

    #[test]
    fn nng_client_error_service_variant() {
        let err = NngClientError::Service("rejected".to_string());
        assert!(err.to_string().contains("rejected"));
    }

    #[test]
    fn resolve_ipv4_address_passes_through() {
        let ip = "192.168.0.1";
        assert_eq!(resolve_hostname_to_ipv4(ip).unwrap(), ip);
    }

    #[test]
    fn resolve_localhost_returns_loopback() {
        let result = resolve_hostname_to_ipv4("localhost").unwrap();
        assert!(
            result.starts_with("127."),
            "expected loopback, got {}",
            result
        );
    }

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

    #[test]
    fn cache_write_and_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = temp_cache_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        write_cached_ip_to("myhost.local", "192.168.1.42", path.clone());
        let result = read_cached_ip_from("myhost.local", path);
        assert_eq!(result, Some("192.168.1.42".to_string()));
    }

    #[test]
    fn cache_miss_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = temp_cache_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        write_cached_ip_to("other.local", "10.0.0.1", path.clone());
        let result = read_cached_ip_from("missing.local", path);
        assert_eq!(result, None);
    }

    #[test]
    fn cache_overwrite_same_host() {
        let tmp = tempfile::tempdir().unwrap();
        let path = temp_cache_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        write_cached_ip_to("myhost.local", "192.168.1.1", path.clone());
        write_cached_ip_to("myhost.local", "192.168.1.99", path.clone());
        let result = read_cached_ip_from("myhost.local", path);
        assert_eq!(result, Some("192.168.1.99".to_string()));
    }

    #[test]
    fn connect_configures_send_and_recv_timeouts() {
        // Bind a Rep0 server so connect()'s blocking dial() can succeed.
        let addr = format!("ipc:///tmp/mdma-test-timeouts-{}.sock", std::process::id());
        let server = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        server.listen(&addr).unwrap();

        let client = connect(&addr).expect("should connect to local server");

        // Verify connect() configured both timeout options.
        let send_timeout = client.get_opt::<SendTimeout>().unwrap();
        let recv_timeout = client.get_opt::<RecvTimeout>().unwrap();
        assert_eq!(send_timeout, Some(DEFAULT_TIMEOUT));
        assert_eq!(recv_timeout, Some(DEFAULT_TIMEOUT));
    }

    /// Integration test: requires avahi-daemon running and mdma-909.local reachable.
    /// Run manually with: cargo test -p nng-transport -- --ignored
    #[test]
    #[ignore]
    fn resolve_via_avahi_returns_some_for_known_local_host() {
        let result = avahi_platform::resolve("mdma-909.local");
        assert!(
            result.is_some(),
            "expected avahi to resolve mdma-909.local, got None"
        );
        let ip = result.unwrap();
        // Should look like an IPv4 address
        assert!(
            ip.split('.').count() == 4 && ip.chars().all(|c| c.is_ascii_digit() || c == '.'),
            "expected IPv4 address, got: {}",
            ip
        );
    }

    /// Integration test: requires avahi-daemon running.
    /// Run manually with: cargo test -p nng-transport -- --ignored
    #[test]
    #[ignore]
    fn resolve_via_avahi_returns_none_for_nonexistent() {
        let result = avahi_platform::resolve("nonexistent-mdma-host.local");
        assert!(
            result.is_none(),
            "expected None for nonexistent host, got: {:?}",
            result
        );
    }

    #[test]
    fn recv_on_unresponsive_server_times_out() {
        // Server accepts connection but never replies.
        let addr = format!(
            "ipc:///tmp/mdma-test-timeout-fires-{}.sock",
            std::process::id()
        );
        let server = nng::Socket::new(nng::Protocol::Rep0).unwrap();
        server.listen(&addr).unwrap();

        let client = connect(&addr).expect("should connect to local server");
        // Short timeout so the test doesn't take 5 seconds.
        client
            .set_opt::<RecvTimeout>(Some(Duration::from_millis(100)))
            .unwrap();

        // Send a request — succeeds (goes into IPC buffer).
        let msg = nng::Message::new();
        client.send(msg).expect("send should succeed");
        // Server never reads and never replies.

        // Recv blocks until RecvTimeout fires.
        let start = Instant::now();
        let result = client.recv();
        let elapsed = start.elapsed();

        assert!(result.is_err(), "expected recv to time out");
        assert!(
            elapsed < Duration::from_millis(500),
            "timed out in {:?}",
            elapsed
        );
    }
}
