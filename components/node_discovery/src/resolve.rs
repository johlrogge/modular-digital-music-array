//! Pure resolution order for "which node do I talk to".
//!
//! Effects (network probes, mDNS browse, TTY prompt, cache read) are injected
//! as closures so the order can be tested without any I/O. The caller saves
//! [`Resolution::cache_update`] when present.

use std::net::Ipv4Addr;
use std::time::Duration;

use crate::browse::DiscoveredNode;
use crate::cache::NodeCache;
use crate::spec::NodeSpec;
use crate::verify::{verify_candidate, VerifyError};

/// Gateway port used when none is given.
pub const DEFAULT_PORT: u16 = 5555;
/// How long a browse should listen.
pub const BROWSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Result of asking the gateway at an address who it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityCheck {
    /// The node reported this id.
    Id(String),
    /// Old gateway, or an unconfigured node (no id): it answered, but cannot
    /// say who it is. Never enough on its own to accept a cached address.
    Unverifiable,
    /// The gateway answered with an error. Treated like a mismatch: never
    /// accepted.
    Failed,
}

/// The address could not be reached at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("node unreachable")]
pub struct Unreachable;

/// Ask the gateway at `ip:port` who it is.
pub type IdentifyFn<'a> = dyn FnMut(Ipv4Addr, u16) -> Result<IdentityCheck, Unreachable> + 'a;
/// Resolve a host name afresh, never from a cache.
pub type ResolveNameFn<'a> = dyn FnMut(&str) -> Option<Ipv4Addr> + 'a;
/// Browse the network for `Duration`; with `Some(id)`, stop early once a
/// selectable node with that id has resolved.
pub type BrowseFn<'a> = dyn FnMut(Duration, Option<&str>) -> Vec<DiscoveredNode> + 'a;
/// Interactive pick; `None` on cancel.
pub type PickFn<'a> = dyn FnMut(&[DiscoveredNode]) -> Option<usize> + 'a;
/// The remembered node.
pub type LoadCacheFn<'a> = dyn FnMut() -> Option<NodeCache> + 'a;

/// Injected effects.
pub struct Effects<'a> {
    pub identify_at: Box<IdentifyFn<'a>>,
    pub resolve_name_fresh: Box<ResolveNameFn<'a>>,
    pub browse: Box<BrowseFn<'a>>,
    pub pick: Box<PickFn<'a>>,
    /// Only called once nothing more explicit applies.
    pub load_cache: Box<LoadCacheFn<'a>>,
    pub is_tty: bool,
}

/// What the user/environment asked for.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// `--node`
    pub explicit: Option<NodeSpec>,
    /// `MDMA_NODE`
    pub env: Option<NodeSpec>,
    pub local_ipc: bool,
}

/// Where a remote target came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Explicit,
    Env,
    /// Cached address answered with the cached id.
    Cache,
    /// Nothing confirmed the cached node's identity, but its name still
    /// resolves to the cached address. The CLI should print a note.
    CacheUnverified,
    CacheRelocated {
        from: Ipv4Addr,
        to: Ipv4Addr,
    },
    Discovered,
    Picked,
}

/// A node reached over the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    pub host: String,
    pub ip: Option<Ipv4Addr>,
    pub port: u16,
    pub source: Source,
}

/// What to talk to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The services on this machine, over IPC.
    LocalIpc,
    Remote(RemoteTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub target: Target,
    /// New cache contents to save, when they changed.
    pub cache_update: Option<NodeCache>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("cached node '{id}' was not found on the network")]
    CachedNodeNotFound {
        id: String,
        others: Vec<DiscoveredNode>,
    },
    #[error("no MDMA nodes found on the network")]
    NoNodesFound,
    #[error("{} nodes found and no terminal to choose from", .0.len())]
    Ambiguous(Vec<DiscoveredNode>),
    #[error("node selection cancelled")]
    Cancelled,
    /// A discovered node could not be confirmed to be what it announced.
    #[error("discovered node could not be verified: {0}")]
    Unverified(VerifyError),
}

/// Resolve the node to talk to. See the module docs and the order below:
/// explicit, env, local IPC, cache (verified by id), browse.
pub fn resolve(request: Request, effects: &mut Effects<'_>) -> Result<Resolution, ResolveError> {
    let Request {
        explicit,
        env,
        local_ipc,
    } = request;

    if let Some(spec) = explicit {
        return Ok(named(spec, Source::Explicit));
    }
    if let Some(spec) = env {
        return Ok(named(spec, Source::Env));
    }
    if local_ipc {
        return Ok(Resolution {
            target: Target::LocalIpc,
            cache_update: None,
        });
    }
    match (effects.load_cache)() {
        Some(cached) => resolve_cached(cached, effects),
        None => resolve_by_browse(effects),
    }
}

/// An explicit/env target: `host` or `host:port`. The cache is left alone.
fn named(spec: NodeSpec, source: Source) -> Resolution {
    Resolution {
        target: Target::Remote(RemoteTarget {
            host: spec.host().to_string(),
            ip: None,
            port: spec.port().unwrap_or(DEFAULT_PORT),
            source,
        }),
        cache_update: None,
    }
}

fn resolve_cached(
    cached: NodeCache,
    effects: &mut Effects<'_>,
) -> Result<Resolution, ResolveError> {
    let keep = |source| Resolution {
        target: Target::Remote(RemoteTarget {
            host: cached.host.clone(),
            ip: Some(cached.ip),
            port: cached.port,
            source,
        }),
        cache_update: None,
    };

    // 1. The cached address. A different id is never accepted, and neither is
    // an answer that cannot say who it is: a cache entry always has an id, so
    // an anonymous answer is more likely another node on a reused address.
    let mut unverifiable_at_cached_ip = false;
    match (effects.identify_at)(cached.ip, cached.port) {
        Ok(IdentityCheck::Id(id)) if id == cached.id => return Ok(keep(Source::Cache)),
        Ok(IdentityCheck::Unverifiable) => unverifiable_at_cached_ip = true,
        Ok(IdentityCheck::Id(_)) | Ok(IdentityCheck::Failed) | Err(Unreachable) => {}
    }

    // 2. Re-resolve the name; only a different address that confirms the id
    // counts.
    let fresh_ip = (effects.resolve_name_fresh)(&cached.host);
    if let Some(ip) = fresh_ip {
        if ip != cached.ip && confirms(effects, ip, cached.port, &cached.id) {
            return Ok(relocated(
                &cached,
                &cached.host,
                &cached.name,
                ip,
                cached.port,
            ));
        }
    }

    // 3. Browse for the same id (it may have been renamed). mDNS only
    // proposes candidates: each address is asked who it is, and only an answer
    // with the cached id counts. The cached address was already asked in step 1.
    let nodes = selectable((effects.browse)(BROWSE_TIMEOUT, Some(&cached.id)));
    for node in nodes
        .iter()
        .filter(|n| n.id.as_deref() == Some(cached.id.as_str()))
    {
        for &ip in node.ipv4s.iter().filter(|ip| **ip != cached.ip) {
            if confirms(effects, ip, node.port, &cached.id) {
                let name = node.name.clone().unwrap_or_else(|| cached.name.clone());
                return Ok(relocated(&cached, &node.host, &name, ip, node.port));
            }
        }
    }

    // 4. Last resort: the address could not say who it is, but the remembered
    // name still points at it.
    if unverifiable_at_cached_ip && fresh_ip == Some(cached.ip) {
        return Ok(keep(Source::CacheUnverified));
    }

    // 5. Not found. Never switch to another node on our own.
    let others: Vec<DiscoveredNode> = nodes
        .into_iter()
        .filter(|n| n.id.as_deref() != Some(cached.id.as_str()))
        .collect();
    if effects.is_tty && !others.is_empty() {
        return pick_one(&others, effects);
    }
    Err(ResolveError::CachedNodeNotFound {
        id: cached.id,
        others,
    })
}

/// Whether the gateway at `ip:port` answers with exactly `id`.
fn confirms(effects: &mut Effects<'_>, ip: Ipv4Addr, port: u16, id: &str) -> bool {
    matches!((effects.identify_at)(ip, port), Ok(IdentityCheck::Id(reported)) if reported == id)
}

fn relocated(from: &NodeCache, host: &str, name: &str, to: Ipv4Addr, port: u16) -> Resolution {
    Resolution {
        target: Target::Remote(RemoteTarget {
            host: host.to_string(),
            ip: Some(to),
            port,
            source: Source::CacheRelocated { from: from.ip, to },
        }),
        cache_update: Some(NodeCache {
            id: from.id.clone(),
            name: name.to_string(),
            host: host.to_string(),
            ip: to,
            port,
        }),
    }
}

fn resolve_by_browse(effects: &mut Effects<'_>) -> Result<Resolution, ResolveError> {
    let mut nodes = selectable((effects.browse)(BROWSE_TIMEOUT, None));
    match nodes.len() {
        0 => Err(ResolveError::NoNodesFound),
        1 => from_node(&nodes.remove(0), Source::Discovered, effects),
        _ if effects.is_tty => pick_one(&nodes, effects),
        _ => Err(ResolveError::Ambiguous(nodes)),
    }
}

fn pick_one(
    nodes: &[DiscoveredNode],
    effects: &mut Effects<'_>,
) -> Result<Resolution, ResolveError> {
    let node = (effects.pick)(nodes)
        .and_then(|i| nodes.get(i))
        .ok_or(ResolveError::Cancelled)?;
    from_node(node, Source::Picked, effects)
}

/// Target for a chosen node, at the address that confirmed it (see
/// [`verify_candidate`]), cached under the id it reported. A node that
/// announced no id and cannot say who it is may be used for this run only: it
/// is never cached. Anything else that fails verification is an error.
fn from_node(
    node: &DiscoveredNode,
    source: Source,
    effects: &mut Effects<'_>,
) -> Result<Resolution, ResolveError> {
    match verify_candidate(node, &mut *effects.identify_at) {
        Ok((entry, ip)) => Ok(Resolution {
            target: Target::Remote(RemoteTarget {
                host: entry.host.clone(),
                ip: Some(ip),
                port: entry.port,
                source,
            }),
            cache_update: Some(entry),
        }),
        Err(VerifyError::UnverifiableUnannounced { node, ip }) => Ok(Resolution {
            target: Target::Remote(RemoteTarget {
                host: node.host,
                ip: Some(ip),
                port: node.port,
                source,
            }),
            cache_update: None,
        }),
        Err(e) => Err(ResolveError::Unverified(e)),
    }
}

/// The cache entry for `node`; `None` when it has no id or no IPv4 address
/// (nothing to remember it by).
pub fn cache_entry(node: &DiscoveredNode) -> Option<NodeCache> {
    let id = node.id.as_ref()?;
    let ip = node.ipv4s.first().copied()?;
    Some(NodeCache {
        id: id.clone(),
        name: node.name.clone().unwrap_or_else(|| id.clone()),
        host: node.host.clone(),
        ip,
        port: node.port,
    })
}

/// Beacons are never auto-selected.
pub(crate) fn selectable(nodes: Vec<DiscoveredNode>) -> Vec<DiscoveredNode> {
    nodes
        .into_iter()
        .filter(DiscoveredNode::is_selectable)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::NodeKind;
    use std::cell::{Cell, RefCell};

    const OLD_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);
    const NEW_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 99);

    fn cached() -> NodeCache {
        NodeCache {
            id: "mdma-stage".into(),
            name: "Stage".into(),
            host: "mdma-stage.local".into(),
            ip: OLD_IP,
            port: 5555,
        }
    }

    fn node(id: Option<&str>, ip: Ipv4Addr, kind: NodeKind) -> DiscoveredNode {
        DiscoveredNode {
            id: id.map(Into::into),
            name: id.map(|i| i.to_uppercase()),
            host: format!("{}.local", id.unwrap_or("unconfigured")),
            ipv4s: vec![ip],
            port: 5555,
            version: Some("0.29.0".into()),
            kind,
        }
    }

    fn stage_at(ip: Ipv4Addr) -> DiscoveredNode {
        node(Some("mdma-stage"), ip, NodeKind::Node)
    }

    fn prod() -> DiscoveredNode {
        node(
            Some("mdma-prod"),
            Ipv4Addr::new(192, 168, 1, 30),
            NodeKind::Node,
        )
    }

    /// Scripted world.
    struct World {
        cache: Option<NodeCache>,
        cache_loads: Cell<usize>,
        /// ip -> identity answer (absent = unreachable)
        identities: Vec<(Ipv4Addr, IdentityCheck)>,
        fresh_ip: Option<Ipv4Addr>,
        browse: Vec<DiscoveredNode>,
        browse_stop_ids: RefCell<Vec<Option<String>>>,
        pick: Option<usize>,
        is_tty: bool,
        picked_from: RefCell<Option<Vec<DiscoveredNode>>>,
    }

    impl World {
        fn new() -> Self {
            World {
                cache: None,
                cache_loads: Cell::new(0),
                identities: vec![],
                fresh_ip: None,
                browse: vec![],
                browse_stop_ids: RefCell::new(vec![]),
                pick: None,
                is_tty: false,
                picked_from: RefCell::new(None),
            }
        }
        /// A world with the stage node remembered.
        fn cached() -> Self {
            World {
                cache: Some(cached()),
                ..World::new()
            }
        }
        fn identity(mut self, ip: Ipv4Addr, check: IdentityCheck) -> Self {
            self.identities.push((ip, check));
            self
        }
        fn fresh(mut self, ip: Option<Ipv4Addr>) -> Self {
            self.fresh_ip = ip;
            self
        }
        fn browse(mut self, nodes: Vec<DiscoveredNode>) -> Self {
            self.browse = nodes;
            self
        }
        fn tty(mut self, pick: Option<usize>) -> Self {
            self.is_tty = true;
            self.pick = pick;
            self
        }
        fn run(&self, request: Request) -> Result<Resolution, ResolveError> {
            let mut effects = Effects {
                identify_at: Box::new(|ip, _| {
                    self.identities
                        .iter()
                        .find(|(i, _)| *i == ip)
                        .map(|(_, c)| c.clone())
                        .ok_or(Unreachable)
                }),
                resolve_name_fresh: Box::new(|_| self.fresh_ip),
                browse: Box::new(|_, until| {
                    self.browse_stop_ids
                        .borrow_mut()
                        .push(until.map(String::from));
                    self.browse.clone()
                }),
                pick: Box::new(|nodes| {
                    *self.picked_from.borrow_mut() = Some(nodes.to_vec());
                    self.pick
                }),
                load_cache: Box::new(|| {
                    self.cache_loads.set(self.cache_loads.get() + 1);
                    self.cache.clone()
                }),
                is_tty: self.is_tty,
            };
            resolve(request, &mut effects)
        }
    }

    fn id(s: &str) -> IdentityCheck {
        IdentityCheck::Id(s.into())
    }

    fn remote(res: &Resolution) -> &RemoteTarget {
        match &res.target {
            Target::Remote(r) => r,
            Target::LocalIpc => panic!("expected a remote target"),
        }
    }

    fn relocated_to(ip: Ipv4Addr) -> Source {
        Source::CacheRelocated {
            from: OLD_IP,
            to: ip,
        }
    }

    #[test]
    fn cache_entry_needs_id_and_address() {
        let entry = cache_entry(&stage_at(OLD_IP)).unwrap();
        assert_eq!(
            (entry.id.as_str(), entry.ip, entry.port),
            ("mdma-stage", OLD_IP, 5555)
        );
        assert_eq!(cache_entry(&node(None, OLD_IP, NodeKind::Node)), None);
        let mut no_ip = stage_at(OLD_IP);
        no_ip.ipv4s.clear();
        assert_eq!(cache_entry(&no_ip), None);
    }

    #[test]
    fn explicit_beats_env_and_never_loads_the_cache() {
        let r = Request {
            explicit: Some("a.local".parse().unwrap()),
            env: Some("b.local".parse().unwrap()),
            local_ipc: true,
        };
        let w = World::cached();
        let res = w.run(r).unwrap();
        assert_eq!(remote(&res).host, "a.local");
        assert_eq!(remote(&res).source, Source::Explicit);
        assert_eq!(res.cache_update, None);
        assert_eq!(w.cache_loads.get(), 0);
    }

    #[test]
    fn explicit_host_with_port() {
        let r = Request {
            explicit: Some("a.local:6000".parse().unwrap()),
            ..Request::default()
        };
        let res = World::new().run(r).unwrap();
        assert_eq!(
            (remote(&res).host.as_str(), remote(&res).port),
            ("a.local", 6000)
        );
    }

    #[test]
    fn env_beats_local_ipc_and_never_loads_the_cache() {
        let r = Request {
            env: Some("b.local".parse().unwrap()),
            local_ipc: true,
            ..Request::default()
        };
        let w = World::cached();
        let res = w.run(r).unwrap();
        assert_eq!(remote(&res).source, Source::Env);
        assert_eq!(res.cache_update, None);
        assert_eq!(w.cache_loads.get(), 0);
    }

    #[test]
    fn local_ipc_beats_cache_and_never_loads_it() {
        let r = Request {
            local_ipc: true,
            ..Request::default()
        };
        let w = World::cached();
        let res = w.run(r).unwrap();
        assert_eq!(res.target, Target::LocalIpc);
        assert_eq!(res.cache_update, None);
        assert_eq!(w.cache_loads.get(), 0);
    }

    #[test]
    fn cache_is_loaded_once_when_nothing_more_explicit_applies() {
        let w = World::cached().identity(OLD_IP, id("mdma-stage"));
        w.run(Request::default()).unwrap();
        assert_eq!(w.cache_loads.get(), 1);
    }

    #[test]
    fn cached_ip_with_matching_id_is_cache() {
        let res = World::cached()
            .identity(OLD_IP, id("mdma-stage"))
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, Source::Cache);
        assert_eq!(remote(&res).ip, Some(OLD_IP));
        assert_eq!(res.cache_update, None);
    }

    #[test]
    fn cached_ip_with_different_id_is_rejected_and_never_accepted() {
        // The stage/prod swap: prod now sits on stage's old address.
        let res = World::cached()
            .identity(OLD_IP, id("mdma-prod"))
            .identity(NEW_IP, id("mdma-stage"))
            .browse(vec![prod(), stage_at(NEW_IP)])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, relocated_to(NEW_IP));
        assert_eq!(remote(&res).ip, Some(NEW_IP));
    }

    #[test]
    fn swapped_ip_and_stage_gone_is_not_found_not_prod() {
        let err = World::cached()
            .identity(OLD_IP, id("mdma-prod"))
            .browse(vec![prod()])
            .run(Request::default())
            .unwrap_err();
        assert_eq!(
            err,
            ResolveError::CachedNodeNotFound {
                id: "mdma-stage".into(),
                others: vec![prod()]
            }
        );
    }

    #[test]
    fn unverifiable_at_cached_ip_with_name_gone_and_id_not_found_is_not_found() {
        // An old-gateway node (maybe a different one) holds the cached
        // address, and nothing else vouches for it.
        let err = World::cached()
            .identity(OLD_IP, IdentityCheck::Unverifiable)
            .run(Request::default())
            .unwrap_err();
        assert_eq!(
            err,
            ResolveError::CachedNodeNotFound {
                id: "mdma-stage".into(),
                others: vec![]
            }
        );
    }

    #[test]
    fn unverifiable_at_cached_ip_with_name_pointing_there_is_cache_unverified() {
        let res = World::cached()
            .identity(OLD_IP, IdentityCheck::Unverifiable)
            .fresh(Some(OLD_IP))
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, Source::CacheUnverified);
        assert_eq!(remote(&res).ip, Some(OLD_IP));
        assert_eq!(res.cache_update, None);
    }

    #[test]
    fn unverifiable_at_cached_ip_but_browse_finds_id_elsewhere_is_relocated() {
        let res = World::cached()
            .identity(OLD_IP, IdentityCheck::Unverifiable)
            .identity(NEW_IP, id("mdma-stage"))
            .browse(vec![stage_at(NEW_IP)])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, relocated_to(NEW_IP));
        assert_eq!(res.cache_update.unwrap().ip, NEW_IP);
    }

    #[test]
    fn mdns_announcing_the_id_at_the_cached_ip_does_not_confirm_it() {
        // The identify answer is the only authority; mDNS only proposes.
        let err = World::cached()
            .identity(OLD_IP, IdentityCheck::Unverifiable)
            .browse(vec![stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::CachedNodeNotFound { .. }));
    }

    #[test]
    fn mdns_announcing_the_id_at_the_cached_ip_with_name_pointing_there_is_unverified() {
        let res = World::cached()
            .identity(OLD_IP, IdentityCheck::Unverifiable)
            .fresh(Some(OLD_IP))
            .browse(vec![stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, Source::CacheUnverified);
    }

    #[test]
    fn browse_candidate_answering_as_another_node_is_not_accepted() {
        let err = World::cached()
            .identity(NEW_IP, id("mdma-prod"))
            .browse(vec![stage_at(NEW_IP)])
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::CachedNodeNotFound { .. }));
    }

    #[test]
    fn browse_candidate_that_does_not_answer_is_not_accepted() {
        let err = World::cached()
            .browse(vec![stage_at(NEW_IP)])
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::CachedNodeNotFound { .. }));
    }

    #[test]
    fn node_with_two_addresses_relocates_to_the_one_that_confirms() {
        let second = Ipv4Addr::new(192, 168, 1, 100);
        let mut node = stage_at(NEW_IP);
        node.ipv4s = vec![NEW_IP, second];
        let res = World::cached()
            .identity(NEW_IP, id("mdma-prod"))
            .identity(second, id("mdma-stage"))
            .browse(vec![node])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, relocated_to(second));
        assert_eq!(remote(&res).ip, Some(second));
        assert_eq!(res.cache_update.unwrap().ip, second);
    }

    #[test]
    fn failed_identity_is_never_accepted() {
        let err = World::cached()
            .identity(OLD_IP, IdentityCheck::Failed)
            .fresh(Some(OLD_IP))
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::CachedNodeNotFound { .. }));
    }

    #[test]
    fn name_resolving_to_same_dead_ip_is_failure() {
        let err = World::cached()
            .fresh(Some(OLD_IP))
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::CachedNodeNotFound { .. }));
    }

    #[test]
    fn relocation_found_by_name() {
        let res = World::cached()
            .fresh(Some(NEW_IP))
            .identity(NEW_IP, id("mdma-stage"))
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, relocated_to(NEW_IP));
        let updated = res.cache_update.unwrap();
        assert_eq!(updated.ip, NEW_IP);
        assert_eq!(updated.id, "mdma-stage");
    }

    #[test]
    fn name_resolving_to_other_node_is_not_accepted() {
        let err = World::cached()
            .fresh(Some(NEW_IP))
            .identity(NEW_IP, id("mdma-prod"))
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::CachedNodeNotFound { .. }));
    }

    #[test]
    fn name_resolving_to_new_unverifiable_address_is_not_accepted() {
        let err = World::cached()
            .fresh(Some(NEW_IP))
            .identity(NEW_IP, IdentityCheck::Unverifiable)
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::CachedNodeNotFound { .. }));
    }

    #[test]
    fn relocation_found_by_browse_after_rename() {
        let mut moved = stage_at(NEW_IP);
        moved.host = "stage-renamed.local".into();
        moved.name = Some("Renamed".into());
        let res = World::cached()
            .identity(NEW_IP, id("mdma-stage"))
            .browse(vec![prod(), moved])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, relocated_to(NEW_IP));
        assert_eq!(remote(&res).host, "stage-renamed.local");
        let updated = res.cache_update.unwrap();
        assert_eq!(updated.host, "stage-renamed.local");
        assert_eq!(updated.name, "Renamed");
        assert_eq!(updated.ip, NEW_IP);
    }

    #[test]
    fn browse_for_the_cached_node_stops_when_its_id_resolves() {
        let w = World::cached()
            .identity(NEW_IP, id("mdma-stage"))
            .browse(vec![stage_at(NEW_IP)]);
        w.run(Request::default()).unwrap();
        assert_eq!(
            *w.browse_stop_ids.borrow(),
            vec![Some("mdma-stage".to_string())]
        );
    }

    #[test]
    fn browse_without_a_cache_listens_for_the_full_time() {
        let w = World::new()
            .identity(NEW_IP, id("mdma-stage"))
            .browse(vec![stage_at(NEW_IP)]);
        w.run(Request::default()).unwrap();
        assert_eq!(*w.browse_stop_ids.borrow(), vec![None]);
    }

    #[test]
    fn cached_missing_with_one_other_never_auto_switches() {
        let err = World::cached()
            .browse(vec![prod()])
            .run(Request::default())
            .unwrap_err();
        assert_eq!(
            err,
            ResolveError::CachedNodeNotFound {
                id: "mdma-stage".into(),
                others: vec![prod()]
            }
        );
    }

    #[test]
    fn cached_missing_on_tty_offers_pick_of_others() {
        let w = World::cached()
            .identity(Ipv4Addr::new(192, 168, 1, 30), id("mdma-prod"))
            .browse(vec![prod()])
            .tty(Some(0));
        let res = w.run(Request::default()).unwrap();
        assert_eq!(remote(&res).source, Source::Picked);
        assert_eq!(remote(&res).ip, Some(Ipv4Addr::new(192, 168, 1, 30)));
        assert_eq!(res.cache_update.unwrap().id, "mdma-prod");
        assert_eq!(w.picked_from.borrow().clone(), Some(vec![prod()]));
    }

    #[test]
    fn cached_missing_on_tty_cancelled() {
        let err = World::cached()
            .browse(vec![prod()])
            .tty(None)
            .run(Request::default())
            .unwrap_err();
        assert_eq!(err, ResolveError::Cancelled);
    }

    #[test]
    fn no_cache_one_node_is_discovered_and_cached() {
        let res = World::new()
            .identity(OLD_IP, id("mdma-stage"))
            .browse(vec![stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, Source::Discovered);
        assert_eq!(res.cache_update, Some(cached_from_discovery()));
    }

    #[test]
    fn no_cache_single_node_answering_as_another_node_is_an_error_and_not_cached() {
        let err = World::new()
            .identity(OLD_IP, id("mdma-prod"))
            .browse(vec![stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap_err();
        assert_eq!(
            err,
            ResolveError::Unverified(VerifyError::IdentityMismatch {
                node: Box::new(stage_at(OLD_IP)),
                reported: Some("mdma-prod".into())
            })
        );
    }

    #[test]
    fn no_cache_single_node_that_does_not_answer_is_an_error() {
        let err = World::new()
            .browse(vec![stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Unverified(VerifyError::Unreachable(_))
        ));
    }

    #[test]
    fn no_cache_gateway_failure_is_an_error() {
        let err = World::new()
            .identity(OLD_IP, IdentityCheck::Failed)
            .browse(vec![stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Unverified(VerifyError::IdentifyFailed(_))
        ));
    }

    #[test]
    fn no_cache_node_with_two_addresses_uses_the_one_that_confirms() {
        let second = Ipv4Addr::new(192, 168, 1, 100);
        let mut node = stage_at(OLD_IP);
        node.ipv4s = vec![OLD_IP, second];
        let res = World::new()
            .identity(OLD_IP, id("mdma-prod"))
            .identity(second, id("mdma-stage"))
            .browse(vec![node])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, Source::Discovered);
        assert_eq!(remote(&res).ip, Some(second));
        assert_eq!(res.cache_update.unwrap().ip, second);
    }

    #[test]
    fn picked_node_is_verified_too() {
        let w = World::new()
            .identity(OLD_IP, id("mdma-prod"))
            .browse(vec![prod(), stage_at(OLD_IP)])
            .tty(Some(1));
        let err = w.run(Request::default()).unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Unverified(VerifyError::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn picked_node_in_a_cached_miss_is_verified_too() {
        let err = World::cached()
            .identity(Ipv4Addr::new(192, 168, 1, 30), id("mdma-stage"))
            .browse(vec![prod()])
            .tty(Some(0))
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Unverified(VerifyError::IdentityMismatch { .. })
        ));
    }

    fn cached_from_discovery() -> NodeCache {
        NodeCache {
            id: "mdma-stage".into(),
            name: "MDMA-STAGE".into(),
            host: "mdma-stage.local".into(),
            ip: OLD_IP,
            port: 5555,
        }
    }

    #[test]
    fn no_cache_several_nodes_on_tty_picks() {
        let w = World::new()
            .identity(OLD_IP, id("mdma-stage"))
            .browse(vec![prod(), stage_at(OLD_IP)])
            .tty(Some(1));
        let res = w.run(Request::default()).unwrap();
        assert_eq!(remote(&res).source, Source::Picked);
        assert_eq!(res.cache_update, Some(cached_from_discovery()));
    }

    #[test]
    fn no_cache_several_nodes_tty_cancel() {
        let err = World::new()
            .browse(vec![prod(), stage_at(OLD_IP)])
            .tty(None)
            .run(Request::default())
            .unwrap_err();
        assert_eq!(err, ResolveError::Cancelled);
    }

    #[test]
    fn no_cache_several_nodes_without_tty_is_ambiguous() {
        let err = World::new()
            .browse(vec![prod(), stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap_err();
        assert!(matches!(err, ResolveError::Ambiguous(n) if n.len() == 2));
    }

    #[test]
    fn no_cache_zero_nodes() {
        let err = World::new().run(Request::default()).unwrap_err();
        assert_eq!(err, ResolveError::NoNodesFound);
    }

    #[test]
    fn beacons_are_never_selected() {
        let beacon = node(None, NEW_IP, NodeKind::Beacon);
        let err = World::new()
            .browse(vec![beacon.clone()])
            .run(Request::default())
            .unwrap_err();
        assert_eq!(err, ResolveError::NoNodesFound);

        let res = World::new()
            .identity(OLD_IP, id("mdma-stage"))
            .browse(vec![beacon, stage_at(OLD_IP)])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, Source::Discovered);
        assert_eq!(remote(&res).ip, Some(OLD_IP));
    }

    #[test]
    fn unconfigured_unverifiable_node_is_usable_for_this_run_but_never_cached() {
        let res = World::new()
            .identity(NEW_IP, IdentityCheck::Unverifiable)
            .browse(vec![node(None, NEW_IP, NodeKind::Node)])
            .run(Request::default())
            .unwrap();
        assert_eq!(remote(&res).source, Source::Discovered);
        assert_eq!(remote(&res).ip, Some(NEW_IP));
        assert_eq!(res.cache_update, None);
    }

    #[test]
    fn unconfigured_node_that_reports_an_id_is_cached_under_it() {
        let res = World::new()
            .identity(NEW_IP, id("mdma-new"))
            .browse(vec![node(None, NEW_IP, NodeKind::Node)])
            .run(Request::default())
            .unwrap();
        assert_eq!(res.cache_update.unwrap().id, "mdma-new");
    }
}
