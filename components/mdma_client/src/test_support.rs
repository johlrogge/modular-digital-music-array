//! Test helpers for crates that test code talking to a node.

use node_discovery::{DiscoveredNode, NodeKind, RemoteTarget, Source, Target};

/// A discovered node with a name and a version.
pub fn discovered_stage() -> DiscoveredNode {
    DiscoveredNode {
        id: Some("mdma-stage".into()),
        name: Some("Stage".into()),
        host: "mdma-stage.local".into(),
        ipv4s: vec![std::net::Ipv4Addr::new(192, 168, 0, 238)],
        port: 5555,
        version: Some("0.29.0".into()),
        kind: NodeKind::Node,
    }
}

/// A discovered node with neither name nor version.
pub fn discovered_prod() -> DiscoveredNode {
    DiscoveredNode {
        id: Some("mdma-prod".into()),
        name: None,
        host: "mdma-prod.local".into(),
        ipv4s: vec![std::net::Ipv4Addr::new(192, 168, 0, 10)],
        port: 5555,
        version: None,
        kind: NodeKind::Node,
    }
}

/// Keeps a dead node's port reserved: bound but never listening, so dials are
/// always refused and no other test can be handed the same port.
pub struct PortGuard(#[allow(dead_code)] std::os::fd::OwnedFd);

/// A node on this machine that refuses connections. The target, its port, and
/// the guard that must be held for as long as the node should stay dead.
pub fn dead_node() -> (Target, u16, PortGuard) {
    use std::os::fd::FromRawFd;
    // std has no bound-but-not-listening TCP socket, so go through libc.
    // SAFETY: plain socket/bind/getsockname calls on a descriptor we own and
    // immediately wrap in an OwnedFd; `addr` outlives every call using it.
    let (fd, port) = unsafe {
        let raw = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        assert!(raw >= 0, "create a socket");
        let fd = std::os::fd::OwnedFd::from_raw_fd(raw);
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
        let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let addr_ptr = std::ptr::addr_of_mut!(addr).cast::<libc::sockaddr>();
        let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
        assert_eq!(libc::bind(raw, addr_ptr, len), 0, "bind a free port");
        let mut out_len = len;
        assert_eq!(
            libc::getsockname(raw, addr_ptr, &mut out_len),
            0,
            "read the bound port"
        );
        (fd, u16::from_be(addr.sin_port))
    };
    let target = Target::Remote(RemoteTarget {
        host: "localhost".to_string(),
        ip: Some(std::net::Ipv4Addr::LOCALHOST),
        port,
        source: Source::Cache,
    });
    (target, port, PortGuard(fd))
}
