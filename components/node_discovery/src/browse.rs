//! Discovering MDMA nodes on the local network with DNS-SD.
//!
//! The pure parts (TXT parsing, service mapping, merging) are separate from
//! the live browse so they can be tested without a network.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};

/// What kind of `_mdma._tcp` announcer this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeKind {
    /// A provisioned (or unconfigured) MDMA node.
    #[default]
    Node,
    /// An unprovisioned beacon; never auto-selected as a node.
    Beacon,
}

/// A node found on the network via DNS-SD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredNode {
    /// Provisioned hostname; `None` when unconfigured.
    pub id: Option<String>,
    pub name: Option<String>,
    pub host: String,
    pub ipv4s: Vec<Ipv4Addr>,
    pub port: u16,
    pub version: Option<String>,
    pub kind: NodeKind,
}

impl DiscoveredNode {
    /// Whether the node may be chosen as a target. Beacons never are; the
    /// resolver and `select` both use this one rule.
    pub fn is_selectable(&self) -> bool {
        self.kind == NodeKind::Node
    }
}

/// Fields extracted from a TXT record set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TxtInfo {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub kind: NodeKind,
}

/// Parse TXT key/value pairs (`id`, `name`, `ver`, `kind`). Empty values count
/// as absent; unknown or missing `kind` means [`NodeKind::Node`].
pub(crate) fn parse_txt<'a>(props: impl IntoIterator<Item = (&'a str, &'a str)>) -> TxtInfo {
    let mut info = TxtInfo::default();
    for (key, value) in props {
        let non_empty = (!value.is_empty()).then(|| value.to_string());
        match key {
            "id" => info.id = non_empty,
            "name" => info.name = non_empty,
            "ver" => info.version = non_empty,
            "kind" if value == "beacon" => info.kind = NodeKind::Beacon,
            _ => {}
        }
    }
    info
}

/// DNS-SD service type for provisioned/unconfigured nodes.
pub const NODE_SERVICE_TYPE: &str = "_mdma._tcp.local.";
/// DNS-SD service type for (future) beacons.
pub const BEACON_SERVICE_TYPE: &str = "_mdma-beacon._tcp.local.";

/// Pure mapping from a resolved DNS-SD service to a [`DiscoveredNode`].
///
/// Keeps only non-loopback IPv4 addresses (sorted, deduplicated), strips the
/// trailing dot from `hostname`, and forces [`NodeKind::Beacon`] when
/// `service_type` is the beacon service type regardless of TXT.
pub(crate) fn map_service(
    hostname: &str,
    addresses: &[IpAddr],
    port: u16,
    txt: &[(&str, &str)],
    service_type: &str,
) -> DiscoveredNode {
    let info = parse_txt(txt.iter().copied());
    let mut ipv4s: Vec<Ipv4Addr> = addresses
        .iter()
        .filter_map(|a| match a {
            IpAddr::V4(v4) if !v4.is_loopback() => Some(*v4),
            _ => None,
        })
        .collect();
    ipv4s.sort();
    ipv4s.dedup();
    let kind = if service_type.trim_end_matches('.') == BEACON_SERVICE_TYPE.trim_end_matches('.') {
        NodeKind::Beacon
    } else {
        info.kind
    };
    DiscoveredNode {
        id: info.id,
        name: info.name,
        host: hostname.trim_end_matches('.').to_string(),
        ipv4s,
        port,
        version: info.version,
        kind,
    }
}

/// Merge `node` into `found`.
///
/// A node with an id is keyed by its kind and that id, so one machine
/// announced under several service names (a renamed avahi record) is listed
/// once. Without an id the service fullname is the key. A repeated key has its addresses merged
/// into the existing entry; other fields are refreshed from the newer event.
pub(crate) fn merge_node(
    found: &mut BTreeMap<String, DiscoveredNode>,
    fullname: &str,
    node: DiscoveredNode,
) {
    // The kind is part of the key: a beacon that announces the installed
    // hostname as its id must not merge into the node and become selectable.
    let key = match &node.id {
        Some(id) => format!("id:{:?}:{id}", node.kind),
        None => format!("service:{fullname}"),
    };
    match found.get_mut(&key) {
        Some(existing) => {
            let mut ipv4s = std::mem::take(&mut existing.ipv4s);
            ipv4s.extend(node.ipv4s.iter().copied());
            ipv4s.sort();
            ipv4s.dedup();
            *existing = DiscoveredNode { ipv4s, ..node };
        }
        None => {
            found.insert(key, node);
        }
    }
}

/// Browse `_mdma._tcp` and `_mdma-beacon._tcp` for up to `timeout` and return
/// everything resolved. Synchronous; returns an empty list (and logs a
/// warning) if the mDNS daemon cannot be started.
pub fn browse(timeout: Duration) -> Vec<DiscoveredNode> {
    browse_until(timeout, |_| false)
}

/// [`browse`], but return early as soon as a selectable node with id
/// `until_id` has resolved. `None` listens for the full `timeout`.
pub fn browse_for(timeout: Duration, until_id: Option<&str>) -> Vec<DiscoveredNode> {
    browse_until(timeout, |nodes| {
        until_id.is_some_and(|id| {
            nodes
                .iter()
                .any(|n| n.is_selectable() && n.id.as_deref() == Some(id))
        })
    })
}

/// Browse for up to `timeout`, checking `stop` with everything found so far
/// whenever something new resolves; `true` ends the browse early.
pub fn browse_until(
    timeout: Duration,
    mut stop: impl FnMut(&[DiscoveredNode]) -> bool,
) -> Vec<DiscoveredNode> {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("mDNS daemon unavailable: {e}");
            return Vec::new();
        }
    };
    // Only IPv4 addresses are used.
    if let Err(e) = daemon.disable_interface(IfKind::IPv6) {
        tracing::warn!("could not disable IPv6 for mDNS: {e}");
    }
    let mut receivers = Vec::new();
    for ty in [NODE_SERVICE_TYPE, BEACON_SERVICE_TYPE] {
        match daemon.browse(ty) {
            Ok(rx) => receivers.push((ty, rx)),
            Err(e) => tracing::warn!("mDNS browse for {ty} failed: {e}"),
        }
    }

    let deadline = Instant::now() + timeout;
    let slice = Duration::from_millis(20);
    let mut found = BTreeMap::new();
    while Instant::now() < deadline {
        let mut got_any = false;
        for (ty, rx) in &receivers {
            while let Ok(event) = rx.try_recv() {
                got_any = true;
                if let ServiceEvent::ServiceResolved(svc) = event {
                    let addrs: Vec<IpAddr> = svc.get_addresses().iter().copied().collect();
                    let txt: Vec<(&str, &str)> = svc
                        .get_properties()
                        .iter()
                        .map(|p| (p.key(), p.val_str()))
                        .collect();
                    let node = map_service(svc.get_hostname(), &addrs, svc.get_port(), &txt, ty);
                    merge_node(&mut found, svc.get_fullname(), node);
                }
            }
        }
        if got_any {
            let snapshot: Vec<DiscoveredNode> = found.values().cloned().collect();
            if stop(&snapshot) {
                break;
            }
        } else {
            std::thread::sleep(slice.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    for (ty, _) in &receivers {
        let _ = daemon.stop_browse(ty);
    }
    if let Ok(done) = daemon.shutdown() {
        let _ = done.recv_timeout(Duration::from_millis(500));
    }
    found.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn mapping_keeps_only_non_loopback_ipv4() {
        let node = map_service(
            "mdma-stage.local.",
            &[
                addr("127.0.0.1"),
                addr("fe80::1"),
                addr("192.168.1.5"),
                addr("192.168.1.5"),
            ],
            5555,
            &[("id", "mdma-stage"), ("ver", "0.29.0")],
            NODE_SERVICE_TYPE,
        );
        assert_eq!(node.ipv4s, vec![Ipv4Addr::new(192, 168, 1, 5)]);
        assert_eq!(node.host, "mdma-stage.local");
        assert_eq!(node.id.as_deref(), Some("mdma-stage"));
        assert_eq!(node.kind, NodeKind::Node);
    }

    #[test]
    fn beacon_service_type_forces_beacon_kind() {
        let node = map_service(
            "b.local.",
            &[],
            80,
            &[("kind", "node")],
            BEACON_SERVICE_TYPE,
        );
        assert_eq!(node.kind, NodeKind::Beacon);
    }

    #[test]
    fn txt_beacon_kind_on_node_service_is_beacon() {
        let node = map_service(
            "b.local.",
            &[],
            80,
            &[("kind", "beacon")],
            NODE_SERVICE_TYPE,
        );
        assert_eq!(node.kind, NodeKind::Beacon);
    }

    #[test]
    fn only_nodes_are_selectable() {
        let node = map_service("n.local.", &[], 1, &[], NODE_SERVICE_TYPE);
        let beacon = map_service("b.local.", &[], 1, &[], BEACON_SERVICE_TYPE);
        assert!(node.is_selectable());
        assert!(!beacon.is_selectable());
    }

    #[test]
    fn duplicate_events_merge_addresses() {
        let mut found = BTreeMap::new();
        let a = map_service("h.local.", &[addr("10.0.0.1")], 1, &[], NODE_SERVICE_TYPE);
        let b = map_service(
            "h.local.",
            &[addr("10.0.0.2"), addr("10.0.0.1")],
            1,
            &[],
            NODE_SERVICE_TYPE,
        );
        merge_node(&mut found, "x._mdma._tcp.local.", a);
        merge_node(&mut found, "x._mdma._tcp.local.", b);
        merge_node(
            &mut found,
            "y._mdma._tcp.local.",
            map_service("o.local.", &[addr("10.0.0.9")], 1, &[], NODE_SERVICE_TYPE),
        );
        assert_eq!(found.len(), 2);
        assert_eq!(
            found["service:x._mdma._tcp.local."].ipv4s,
            vec![Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2)]
        );
    }

    #[test]
    fn same_id_under_different_service_names_is_one_node() {
        // A renamed avahi record: the same Pi announced as two services.
        let mut found = BTreeMap::new();
        let old = map_service(
            "pi.local.",
            &[addr("10.0.0.1")],
            5555,
            &[("id", "mdma-stage"), ("name", "Old")],
            NODE_SERVICE_TYPE,
        );
        let renamed = map_service(
            "pi-2.local.",
            &[addr("10.0.0.1"), addr("10.0.0.7")],
            5555,
            &[("id", "mdma-stage"), ("name", "New")],
            NODE_SERVICE_TYPE,
        );
        merge_node(&mut found, "Old._mdma._tcp.local.", old);
        merge_node(&mut found, "New._mdma._tcp.local.", renamed);
        assert_eq!(found.len(), 1);
        let node = found.values().next().unwrap();
        assert_eq!(node.name.as_deref(), Some("New"));
        assert_eq!(
            node.ipv4s,
            vec![Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 7)]
        );
    }

    #[test]
    fn beacon_with_the_nodes_id_never_merges_into_the_node() {
        let mut found = BTreeMap::new();
        let node = map_service(
            "pi.local.",
            &[addr("10.0.0.1")],
            5555,
            &[("id", "mdma-stage")],
            NODE_SERVICE_TYPE,
        );
        let beacon = map_service(
            "pi.local.",
            &[addr("10.0.0.2")],
            80,
            &[("id", "mdma-stage")],
            BEACON_SERVICE_TYPE,
        );
        merge_node(&mut found, "n._mdma._tcp.local.", node);
        merge_node(&mut found, "b._mdma-beacon._tcp.local.", beacon);
        assert_eq!(found.len(), 2);
        let selectable = found.values().filter(|n| n.is_selectable()).count();
        assert_eq!(selectable, 1);
        let node = found.values().find(|n| n.is_selectable()).unwrap();
        assert_eq!(node.ipv4s, vec![Ipv4Addr::new(10, 0, 0, 1)]);
    }

    #[test]
    fn different_ids_stay_separate() {
        let mut found = BTreeMap::new();
        for id in ["a", "b"] {
            let n = map_service("h.local.", &[], 1, &[("id", id)], NODE_SERVICE_TYPE);
            merge_node(&mut found, "same._mdma._tcp.local.", n);
        }
        assert_eq!(found.len(), 2);
    }

    /// Live test: needs a network with mDNS. Run manually with:
    /// cargo test -p node-discovery -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_browse_prints_discovered_nodes() {
        let nodes = browse(Duration::from_secs(2));
        for n in &nodes {
            println!("{n:?}");
        }
        println!("found {} node(s)", nodes.len());
    }

    #[test]
    fn parses_node_txt() {
        let info = parse_txt([
            ("id", "mdma-stage"),
            ("name", "Stage"),
            ("ver", "0.29.0"),
            ("kind", "node"),
            ("proto", "1"),
        ]);
        assert_eq!(info.id.as_deref(), Some("mdma-stage"));
        assert_eq!(info.name.as_deref(), Some("Stage"));
        assert_eq!(info.version.as_deref(), Some("0.29.0"));
        assert_eq!(info.kind, NodeKind::Node);
    }

    #[test]
    fn parses_beacon_and_empty_values() {
        let info = parse_txt([("id", ""), ("kind", "beacon")]);
        assert_eq!(info.id, None);
        assert_eq!(info.kind, NodeKind::Beacon);
    }

    #[test]
    fn missing_kind_is_node() {
        assert_eq!(parse_txt([]).kind, NodeKind::Node);
    }
}
