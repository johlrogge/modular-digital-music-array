//! `mdma replica`: the client-side copy of a node's fact log (ADR-021).
//!
//! The store and the sync rules live in `fact_replica`; this module supplies the
//! effects (identify, ACID reads, playlist and source probes) and the rendering.
//! Nothing here blocks completion: `sync` is meant to run detached.

use std::path::{Path, PathBuf};
use std::time::Duration;

use acid_protocol::AcidRequest;
use color_eyre::eyre::eyre;
use color_eyre::Result;
use fact_replica::{
    default_nodes_dir, id_for_host, release_host_elsewhere, should_attempt, Host, Meta, NodeId,
    ReadFailure, Replica, SyncLock, SyncReport, UnixSecs,
};
use gateway_client::{GatewayClient, IdentifyError, NodeIdentity};
use hicli::Arg;
use library_ipc_client::{LibraryRequest, LibraryResponse};
use library_projection::{project_lines, Projection};
use mdma_client::LibraryBackend;
use serde::{Deserialize, Serialize};

use crate::ctx::{connect_failed, library_connect_failed, Ctx};
use crate::node;
use crate::output::interpret_library;
use crate::snapshot::CompletionSnapshot;

/// How long the replica waits for the node, per request.
const LINK_TIMEOUT: Duration = Duration::from_secs(10);

/// The name a node reached over local IPC is recorded under, so that `status`
/// and `reset` can find its replica again without a network.
const LOCAL_HOST: &str = "localhost";

/// How a replica comes to exist, for the "no replica yet" messages.
const HOW_TO_GET_ONE: &str = "a background sync creates it, or run `mdma replica sync`";

/// How long probed playlist and source names are served.
pub(crate) const PROBE_TTL_SECS: u64 = 24 * 60 * 60;

const PROBE_FILE: &str = "probe.json";

// ---------------------------------------------------------------------------
// Probe: data that is not facts
// ---------------------------------------------------------------------------

/// Names the node answered with at `at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Probed {
    pub at: UnixSecs,
    pub names: Vec<String>,
}

/// Playlists are files and sources are sockets, not facts, so they are probed on
/// each sync and kept for [`PROBE_TTL_SECS`]. Raw names; the completion snapshot
/// is built from them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Probe {
    pub playlists: Option<Probed>,
    pub sources: Option<Probed>,
}

fn live(probed: Option<Probed>, now: UnixSecs) -> Option<Probed> {
    probed.filter(|p| now.since(p.at).is_none_or(|age| age < PROBE_TTL_SECS))
}

impl Probe {
    /// Only what is still within its TTL.
    pub(crate) fn live(self, now: UnixSecs) -> Probe {
        Probe {
            playlists: live(self.playlists, now),
            sources: live(self.sources, now),
        }
    }

    /// Replace each list that was answered; keep the old one when the probe failed
    /// (it is served until its TTL runs out), and drop what has expired.
    pub(crate) fn updated(
        self,
        now: UnixSecs,
        playlists: Option<Vec<String>>,
        sources: Option<Vec<String>>,
    ) -> Probe {
        let fresh = |names| Probed { at: now, names };
        Probe {
            playlists: playlists.map(fresh).or(self.playlists),
            sources: sources.map(fresh).or(self.sources),
        }
        .live(now)
    }

    fn path(replica: &Replica) -> PathBuf {
        replica.dir().join(PROBE_FILE)
    }

    /// The stored probe; empty when there is none or it is unreadable.
    pub(crate) fn load(replica: &Replica) -> Probe {
        std::fs::read(Self::path(replica))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, replica: &Replica) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).expect("Probe serializes");
        private_file::write_private_file(&Self::path(replica), &json)
            .map_err(|e| eyre!("could not write the probe file: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The node id from an identify answer; a node that cannot say gets no replica.
fn node_id_from(answer: Result<NodeIdentity, IdentifyError>) -> Result<NodeId> {
    match answer {
        Ok(NodeIdentity { id: Some(id), .. }) => {
            NodeId::new(id).map_err(|e| eyre!("the node reported an unusable id: {e}"))
        }
        Ok(NodeIdentity { id: None, .. }) => Err(eyre!(
            "the node has no id configured, so it gets no replica"
        )),
        Err(IdentifyError::Unsupported(_)) => Err(eyre!(
            "the node's gateway is too old to say which node it is, so it gets no replica"
        )),
        Err(IdentifyError::Client(e)) => Err(eyre!("could not identify the node: {e}")),
    }
}

/// The replica a name stands for: a host name some replica was reached by, or the
/// id of an existing replica.
fn replica_named(nodes_dir: &Path, name: &str) -> Option<NodeId> {
    id_for_host(nodes_dir, &Host::new(name)).or_else(|| {
        let id = NodeId::new(name).ok()?;
        Replica::new(nodes_dir, &id).dir().is_dir().then_some(id)
    })
}

/// The replica of the node commands talk to, decided offline (no mDNS, no
/// network):
///
/// 1. an explicit name must match a replica, and never falls back to anything else;
/// 2. on the node itself (`local`), the replica synced as [`LOCAL_HOST`];
/// 3. `MDMA_NODE` matched against the replicas' hosts, else the remembered node
///    (`node.json`: id and host) only when its host is the one `MDMA_NODE` names;
/// 4. without `MDMA_NODE`, the remembered node's id.
fn pick_replica(
    nodes_dir: &Path,
    explicit: Option<&str>,
    local: bool,
    env: Option<&str>,
    remembered: Option<(&str, &str)>,
) -> Option<NodeId> {
    if let Some(name) = explicit {
        return replica_named(nodes_dir, name);
    }
    if local {
        if let Some(id) = id_for_host(nodes_dir, &Host::new(LOCAL_HOST)) {
            return Some(id);
        }
    }
    let remembered_id = |only_for_host: Option<&str>| {
        let (id, host) = remembered?;
        if only_for_host.is_some_and(|name| Host::new(name) != Host::new(host)) {
            return None;
        }
        NodeId::new(id).ok()
    };
    match env {
        Some(name) => replica_named(nodes_dir, name).or_else(|| remembered_id(Some(name))),
        None => remembered_id(None),
    }
}

/// "5 s ago", "3 min ago", "2 h ago", "4 d ago"; "never" without a time.
pub(crate) fn ago(now: UnixSecs, then: Option<UnixSecs>) -> String {
    let Some(then) = then else {
        return "never".to_string();
    };
    let Some(secs) = now.since(then) else {
        return "in the future (clock changed)".to_string();
    };
    match secs {
        0..=89 => format!("{secs} s ago"),
        90..=5399 => format!("{} min ago", secs / 60),
        5400..=129_599 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

fn probe_line(label: &str, probed: Option<&Probed>, now: UnixSecs) -> String {
    match probed {
        Some(p) => format!(
            "{label} {} names, probed {}",
            p.names.len(),
            ago(now, Some(p.at))
        ),
        None => format!("{label} none"),
    }
}

/// The text of `mdma replica status`. `unreadable` is the number of lines the fold
/// could not read; they are kept and reported, never dropped from the replica.
fn render_status(
    id: &NodeId,
    meta: &Meta,
    probe: &Probe,
    unreadable: usize,
    now: UnixSecs,
) -> String {
    let epoch = meta.epoch().map_or("-".to_string(), |e| e.to_string());
    let hosts = if meta.hosts.is_empty() {
        "-".to_string()
    } else {
        meta.hosts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let attempts = if should_attempt(now, Some(meta)) {
        "due now".to_string()
    } else {
        let wait = fact_replica::interval_after(meta.failures).as_secs();
        let elapsed = meta
            .last_attempt
            .and_then(|last| now.since(last))
            .unwrap_or(0);
        format!("next in {} s", wait.saturating_sub(elapsed))
    };
    let backoff = if meta.failures == 0 {
        attempts
    } else {
        format!("{attempts} ({} failed attempts in a row)", meta.failures)
    };
    let mut rows = vec![
        format!("Node:          {id}"),
        format!("Epoch:         {epoch}"),
        format!("Lines:         {}", meta.lines()),
        format!("Synced:        {}", ago(now, meta.synced_at)),
        format!("Last attempt:  {}", ago(now, meta.last_attempt)),
        format!("Next attempt:  {backoff}"),
        format!("Hosts:         {hosts}"),
        probe_line("Playlists:    ", probe.playlists.as_ref(), now),
        probe_line("Sources:      ", probe.sources.as_ref(), now),
    ];
    if unreadable > 0 {
        rows.push(format!(
            "Unreadable:    {unreadable} lines this mdma cannot read; upgrade mdma"
        ));
    }
    rows.join("\n")
}

/// Sync the replica of node `id`, remembering `host`, and move the name off any
/// other replica that still lists it (a different node now answers to it).
///
/// `under_lock` runs after a successful sync while the sync lock is still held,
/// for anything else written into the replica directory (the probe, the completion
/// snapshot). `None` means another sync holds the lock and nothing was done.
fn sync_replica(
    nodes_dir: &Path,
    id: &NodeId,
    host: Option<&Host>,
    now: UnixSecs,
    read: &mut fact_replica::ReadPage,
    under_lock: impl FnOnce(&Replica, &SyncLock, &SyncReport) -> Result<()>,
) -> Result<Option<SyncReport>> {
    if let Some(host) = host {
        release_host_elsewhere(nodes_dir, host, id).map_err(|e| eyre!("{e}"))?;
    }
    let replica = Replica::new(nodes_dir, id);
    let Some(lock) = replica.try_lock().map_err(|e| eyre!("{e}"))? else {
        return Ok(None);
    };
    let report = replica
        .sync_locked(&lock, now, host, read)
        .map_err(|e| eyre!("{e}"))?;
    under_lock(&replica, &lock, &report)?;
    Ok(Some(report))
}

/// The directory every replica lives in, `None` when there is no cache directory.
///
/// Under test it is also `None` unless the test set `XDG_CACHE_HOME`, so no test can
/// read or write the real cache of whoever runs it.
pub(crate) fn replicas_dir() -> Option<PathBuf> {
    if cfg!(test) && std::env::var_os("XDG_CACHE_HOME").is_none_or(|dir| dir.is_empty()) {
        return None;
    }
    default_nodes_dir()
}

fn nodes_dir() -> Result<PathBuf> {
    replicas_dir().ok_or_else(|| eyre!("no cache directory is available for the replica"))
}

/// Whether the local services are the node in use, as far as the environment says
/// (completion has no `Ctx`): a library or playback socket is given by variable, or
/// the library socket exists here. The same rule as `Ctx::new`, minus the flags.
fn local_from_environment() -> bool {
    let given = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let library = given("MDMA_LIBRARY_SOCKET");
    let sockets_given = library.is_some() || given("MDMA_PLAYBACK_SOCKET").is_some();
    let library = library.unwrap_or_else(|| crate::ctx::DEFAULT_LIBRARY_SOCKET.to_string());
    crate::ctx::local_ipc_for(&library, sockets_given)
}

/// The replica of the node in use, for completion and for the refresh after a
/// command. Decided offline and silently (no mDNS, no network, nothing printed): on
/// the node itself the replica synced as `localhost`, else `MDMA_NODE` against the
/// replicas' hosts, else the remembered node. A malformed `MDMA_NODE` counts as
/// unset (the command itself reports it).
///
/// `named` is the host of a `--node` the command was given. Completion cannot see
/// one (`None`); the refresh after a command can. As everywhere, a named node has no
/// fallback: no replica of that name is `None`.
pub(crate) fn replica_from_environment(nodes_dir: &Path, named: Option<&str>) -> Option<NodeId> {
    let env = node::env_node().ok().flatten();
    let remembered = node::load_cache_quiet();
    pick_replica(
        nodes_dir,
        named,
        local_from_environment(),
        env.as_ref().map(|spec| spec.host()),
        remembered
            .as_ref()
            .map(|c| (c.id.as_str(), c.host.as_str())),
    )
}

/// Why no replica was found for the node in use.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Missing {
    /// A node was named (by `--node`, else `MDMA_NODE`) and no replica matches it.
    Named(String),
    /// On the node itself, and no sync has made its replica yet.
    LocalNotSynced,
    /// No node is named, remembered or running here.
    NoNode,
}

impl Missing {
    /// What was looked for: the `--node` flag; on the node itself, this node;
    /// else `MDMA_NODE`.
    fn named(explicit: Option<String>, local: bool, env: Option<&str>) -> Self {
        match explicit {
            Some(name) => Missing::Named(name),
            None if local => Missing::LocalNotSynced,
            None => env.map_or(Missing::NoNode, |name| Missing::Named(name.to_string())),
        }
    }

    fn message(&self) -> String {
        match self {
            Missing::Named(name) => format!("no replica for '{name}'"),
            Missing::LocalNotSynced => format!("no replica of this node yet; {HOW_TO_GET_ONE}"),
            Missing::NoNode => {
                "no node to find a replica for; use --node NAME or run `mdma node select`"
                    .to_string()
            }
        }
    }
}

/// The replica a command acts on, or what was looked for and not found.
type Chosen = std::result::Result<NodeId, Missing>;

/// The replica commands act on, decided offline; or what was looked for.
fn find_replica(ctx: &Ctx, nodes_dir: &Path) -> Result<Chosen> {
    let explicit = ctx.node_flag().map(|spec| spec.host().to_string());
    let env = node::env_node().map_err(|message| eyre!("{message}"))?;
    let env_host = env.as_ref().map(|spec| spec.host());
    let remembered = node::load_cache();
    let found = pick_replica(
        nodes_dir,
        explicit.as_deref(),
        ctx.local_ipc(),
        env_host,
        remembered
            .as_ref()
            .map(|c| (c.id.as_str(), c.host.as_str())),
    );
    Ok(found.ok_or_else(|| Missing::named(explicit, ctx.local_ipc(), env_host)))
}

/// The replica commands act on, decided offline.
fn current_replica(ctx: &Ctx, nodes_dir: &Path) -> Result<NodeId> {
    find_replica(ctx, nodes_dir)?.map_err(|missing| eyre!("{}", missing.message()))
}

/// The replica of the node in use, folded with `library_projection`.
pub(crate) struct ReplicaFold {
    pub(crate) id: NodeId,
    /// When the replica last synced; `None` if it never has.
    pub(crate) synced_at: Option<UnixSecs>,
    pub(crate) projection: Projection,
}

/// Fold the replica of the node in use (decided offline, like `replica status`), for
/// the commands that answer from the local facts. Reads only the local copy: a
/// missing replica is an error that says how to get one, never an inline sync
/// (ADR-021 §6; `after_command` already starts one in the background).
pub(crate) fn fold_current(ctx: &Ctx) -> Result<ReplicaFold> {
    let nodes_dir = nodes_dir()?;
    let id = current_replica(ctx, &nodes_dir)?;
    fold_replica(&nodes_dir, id)
}

/// Fold the replica of node `id` under `nodes_dir`.
fn fold_replica(nodes_dir: &Path, id: NodeId) -> Result<ReplicaFold> {
    let replica = Replica::new(nodes_dir, &id);
    let Some(meta) = replica.meta().map_err(|e| eyre!("{e}"))? else {
        return Err(eyre!("no replica of {id} yet; {HOW_TO_GET_ONE}"));
    };
    let lines = replica.lines().map_err(|e| eyre!("{e}"))?;
    let projection = project_lines(lines.iter().map(String::as_str));
    Ok(ReplicaFold {
        id,
        synced_at: meta.synced_at,
        projection,
    })
}

// ---------------------------------------------------------------------------
// Effects: reaching the node
// ---------------------------------------------------------------------------

type Reader = Box<fact_replica::ReadPage<'static>>;

/// A connection to the node: who it is, and how to read its fact log.
struct NodeLink {
    id: NodeId,
    host: Option<Host>,
    read: Reader,
}

/// Connect to the node's gateway, which says who the node is and serves its ACID
/// reads. On the node itself that is the local gateway; ACID is never reached
/// any other way, so the link timeout holds in both cases.
fn connect(ctx: &Ctx) -> Result<NodeLink> {
    let remote = ctx.gateway()?;
    let local = remote.is_none();
    let address = sync_gateway(remote);
    let host = if local {
        Some(Host::new(LOCAL_HOST))
    } else {
        ctx.remote_host()?.map(|(host, _)| Host::new(&host))
    };
    // Locally, a dead gateway gets the "Is mdma-gateway running?" hint.
    let client = GatewayClient::connect_with_timeout(&address, LINK_TIMEOUT)
        .map_err(|e| connect_failed("gateway", &e, local))?;
    let id = node_id_from(client.identify())?;
    let read: Reader = Box::new(move |cursor, limit, expect| {
        let request = AcidRequest::ReadStream {
            cursor,
            limit,
            expect,
        };
        let response = client
            .acid_request(&request)
            .map_err(|e| ReadFailure(e.to_string()))?;
        acid_protocol::classify_read(response).map_err(ReadFailure)
    });
    Ok(NodeLink { id, host, read })
}

/// Run `step`, the part of a sync before the store is reached. If it fails, the
/// failure is counted against `throttled`, the replica the attempt was throttled on
/// (nothing is counted when there is none, or when its sync lock is busy). The error
/// is returned unchanged.
///
/// When the node turns out to be another one than `throttled` (the identify answer
/// names a different id), `step` succeeds and the sync goes on to the new node's
/// replica, which takes the host from the old one and records its own outcome.
fn reach<T>(
    nodes_dir: &Path,
    throttled: Option<&NodeId>,
    now: UnixSecs,
    step: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let reached = step();
    if reached.is_err() {
        if let Some(id) = throttled {
            // Best effort: failing to record must not hide the real error.
            let _ = Replica::new(nodes_dir, id).note_failure(now);
        }
    }
    reached
}

/// The gateway a sync talks to, for the ACID reads and the probes alike: the node's
/// (`remote`), or on the node itself the local one, since the direct IPC sockets
/// are not open to every user.
fn sync_gateway(remote: Option<String>) -> String {
    remote.unwrap_or_else(|| format!("tcp://127.0.0.1:{}", node_discovery::DEFAULT_PORT))
}

/// Playlist names, as the library lists them (through the sync's gateway).
fn probe_playlists(ctx: &Ctx) -> Result<Vec<String>> {
    let remote = ctx.gateway()?;
    let local = remote.is_none();
    let gateway = sync_gateway(remote);
    let library = LibraryBackend::connect(Some(&gateway), ctx.library_socket())
        .map_err(|e| library_connect_failed(&e, local))?;
    interpret_library(
        library.request(&LibraryRequest::PlaylistList),
        |response| match response {
            LibraryResponse::PlaylistNames(names) => {
                Some(names.iter().map(ToString::to_string).collect())
            }
            _ => None,
        },
    )
}

/// Source names, as the sync's gateway lists them.
fn probe_sources(ctx: &Ctx) -> Result<Vec<String>> {
    let gateway = sync_gateway(ctx.gateway()?);
    mdma_client::list_available_sources(Some(&gateway), ctx.sources_dir()?)
        .map(|names| names.iter().map(ToString::to_string).collect())
        .map_err(|e| eyre!("could not list sources: {e}"))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Print nothing unless something fails
#[derive(Debug, Clone, Arg)]
#[arg(long = "quiet")]
pub(crate) struct Quiet(pub bool);

/// Bring the replica up to date with the node
///
/// Reads the node's fact log into the local replica, then probes the playlist and
/// source names. Meant to run detached; a sync already running for the node makes
/// this one exit at once.
#[hicli::command(hide)]
pub(crate) fn sync(quiet: Quiet, ctx: &Ctx) -> Result<()> {
    let nodes_dir = nodes_dir()?;
    let now = UnixSecs::now();
    // Resolving, connecting and identifying all happen before the store is reached,
    // so none of them counts a failure there. The replica the throttle ran on (the
    // one `after_command` and completion pick) takes the failure instead. Without
    // one, the unresolved record already counted this attempt as a failure.
    let named = ctx.node_flag().map(|spec| spec.host().to_string());
    let throttled = replica_from_environment(&nodes_dir, named.as_deref());
    let NodeLink { id, host, mut read } = reach(&nodes_dir, throttled.as_ref(), now, || {
        // Resolve first, so `--quiet` also silences the note about an implicitly
        // chosen node; everything after reuses the cached target.
        ctx.target_with(if quiet.0 {
            node::Notes::Silent
        } else {
            node::Notes::Print
        })?;
        connect(ctx)
    })?;

    let warn = |what: &str, e: &color_eyre::Report| {
        if !quiet.0 {
            eprintln!("warning: could not probe {what}: {e}");
        }
    };
    // The probe file and the completion snapshot are written while the sync lock is
    // held, and so are the probes themselves: a slow playlist or source probe
    // extends the window in which a second sync or a reset reports "busy".
    let probe = |replica: &Replica, _lock: &SyncLock, report: &SyncReport| {
        let playlists = probe_playlists(ctx).map_err(|e| warn("playlists", &e)).ok();
        let sources = probe_sources(ctx).map_err(|e| warn("sources", &e)).ok();
        let probe = Probe::load(replica).updated(now, playlists, sources);
        probe.save(replica)?;
        CompletionSnapshot::refresh(replica, report, &probe).map(|_rebuilt| ())
    };
    let Some(report) = sync_replica(&nodes_dir, &id, host.as_ref(), now, &mut *read, probe)? else {
        if !quiet.0 {
            println!("Another sync of {id} is running.");
        }
        return Ok(());
    };

    if !quiet.0 {
        let how = if report.reset { ", refetched" } else { "" };
        println!(
            "Synced {id}: {} new lines, {} in total{how}",
            report.added, report.total
        );
    }
    Ok(())
}

/// Show the replica of the current node
///
/// Which node is decided offline: `--node`; on the node itself, the replica synced
/// as localhost; else `MDMA_NODE`; else the remembered node (when `MDMA_NODE` is
/// unset or names it).
#[hicli::command]
pub(crate) fn status(ctx: &Ctx) -> Result<()> {
    let nodes_dir = nodes_dir()?;
    let id = current_replica(ctx, &nodes_dir)?;
    let replica = Replica::new(&nodes_dir, &id);
    let Some(meta) = replica.meta().map_err(|e| eyre!("{e}"))? else {
        println!("No replica of {id} yet; `mdma replica sync` creates it.");
        return Ok(());
    };
    let now = UnixSecs::now();
    let probe = Probe::load(&replica).live(now);
    let unreadable = CompletionSnapshot::load(&replica).map_or(0, |s| s.unreadable);
    println!("{}", render_status(&id, &meta, &probe, unreadable, now));
    Ok(())
}

/// Delete the replica of the current node
///
/// Nothing is lost: the next sync rebuilds it from the node. Name another node
/// with `--node`. A node that has no replica is not an error: a second reset
/// says there is nothing to reset.
#[hicli::command]
pub(crate) fn reset(ctx: &Ctx) -> Result<()> {
    let nodes_dir = nodes_dir()?;
    let chosen = find_replica(ctx, &nodes_dir)?;
    println!("{}", reset_replica(&nodes_dir, chosen)?);
    Ok(())
}

/// Reset the replica `chosen` names, and say what happened. A name that matches no
/// replica (none was ever made, or it was just reset and no host lists it any more)
/// is "nothing to reset", not a failure; only having no node to look for is.
fn reset_replica(nodes_dir: &Path, chosen: Chosen) -> Result<String> {
    let id = match chosen {
        Ok(id) => id,
        Err(Missing::Named(name)) => {
            return Ok(format!("Nothing to reset: no replica for '{name}'."));
        }
        Err(Missing::LocalNotSynced) => {
            return Ok("Nothing to reset: no replica of this node yet.".to_string());
        }
        Err(missing @ Missing::NoNode) => return Err(eyre!("{}", missing.message())),
    };
    let replica = Replica::new(nodes_dir, &id);
    Ok(if replica.reset().map_err(|e| eyre!("{e}"))? {
        format!("Deleted the replica of {id}")
    } else {
        format!("Nothing to reset: no replica of {id}.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{help, parses};
    use fact_replica::test_support::FakeNode;
    use pretty_assertions::assert_eq;

    fn secs(n: u64) -> UnixSecs {
        UnixSecs::new(n)
    }

    fn id(s: &str) -> NodeId {
        NodeId::new(s).unwrap()
    }

    fn probed(at: u64, names: &[&str]) -> Probed {
        Probed {
            at: secs(at),
            names: names.iter().map(ToString::to_string).collect(),
        }
    }

    // --- the command tree ----------------------------------------------------

    #[test]
    fn the_commands_parse() {
        assert_eq!(parses(&["replica", "sync", "--quiet"]), ["replica", "sync"]);
        assert_eq!(parses(&["replica", "sync"]), ["replica", "sync"]);
        assert_eq!(parses(&["replica", "status"]), ["replica", "status"]);
        assert_eq!(parses(&["replica", "reset"]), ["replica", "reset"]);
        assert_eq!(
            parses(&["replica", "reset", "--node", "mdma-stage.local"]),
            ["replica", "reset"]
        );
    }

    #[test]
    fn sync_is_hidden_from_help() {
        let help = help(&["replica", "--help"]);
        // A command is listed on a line that starts with its name.
        let lists = |name: &str| help.lines().any(|l| l.trim_start().starts_with(name));
        assert!(lists("status") && lists("reset"), "{help}");
        assert!(!lists("sync"), "{help}");
    }

    // --- probe ---------------------------------------------------------------

    #[test]
    fn a_probe_answer_replaces_the_old_list_and_a_failed_probe_keeps_it() {
        let old = Probe {
            playlists: Some(probed(100, &["old"])),
            sources: Some(probed(100, &["bandcamp"])),
        };

        let both = old.clone().updated(
            secs(200),
            Some(vec!["new".into()]),
            Some(vec!["beatport".into()]),
        );
        assert_eq!(both.playlists, Some(probed(200, &["new"])));
        assert_eq!(both.sources, Some(probed(200, &["beatport"])));

        let failed_sources = old.updated(secs(200), Some(vec![]), None);
        assert_eq!(failed_sources.playlists, Some(probed(200, &[])));
        assert_eq!(failed_sources.sources, Some(probed(100, &["bandcamp"])));
    }

    #[test]
    fn probe_data_expires_after_24_hours() {
        let probe = Probe {
            playlists: Some(probed(1000, &["a"])),
            sources: Some(probed(2000, &["b"])),
        };
        let day = PROBE_TTL_SECS;

        let before = probe.clone().live(secs(1000 + day - 1));
        assert!(before.playlists.is_some() && before.sources.is_some());

        let after = probe.clone().live(secs(1000 + day));
        assert_eq!(after.playlists, None);
        assert!(after.sources.is_some(), "its own clock");

        // A failed probe cannot keep expired names alive.
        let kept = probe.updated(secs(1000 + day), None, None);
        assert_eq!(kept.playlists, None);
    }

    #[test]
    fn a_probe_is_stored_with_the_replica_and_goes_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let replica = Replica::new(dir.path(), &id("n1"));
        assert_eq!(Probe::load(&replica), Probe::default());
        std::fs::create_dir_all(replica.dir()).unwrap();

        let probe = Probe::default().updated(secs(5), Some(vec!["p".into()]), None);
        probe.save(&replica).unwrap();
        assert_eq!(Probe::load(&replica), probe);

        replica.reset().unwrap();
        assert_eq!(Probe::load(&replica), Probe::default());
    }

    // --- identity ------------------------------------------------------------

    fn identity(id: Option<&str>) -> NodeIdentity {
        NodeIdentity {
            id: id.map(String::from),
            name: None,
            version: "0.37.0".into(),
            proto: 1,
        }
    }

    #[test]
    fn a_node_that_cannot_say_who_it_is_gets_no_replica() {
        assert_eq!(
            node_id_from(Ok(identity(Some("mdma-stage")))).unwrap(),
            id("mdma-stage")
        );
        let none = crate::test_support::failure_text(node_id_from(Ok(identity(None))));
        assert_eq!(none, "the node has no id configured, so it gets no replica");
        let old = crate::test_support::failure_text(node_id_from(Err(IdentifyError::Unsupported(
            "old".into(),
        ))));
        assert!(old.starts_with("the node's gateway is too old"), "{old}");
        let unsafe_id = crate::test_support::failure_text(node_id_from(Ok(identity(Some("../x")))));
        assert!(unsafe_id.starts_with("the node reported an unusable id"));
    }

    // --- which replica -------------------------------------------------------

    fn replica_with_host(dir: &Path, node: &str, host: &str) {
        let fake = FakeNode::new("e", 0);
        Replica::new(dir, &id(node))
            .sync(secs(1), Some(&Host::new(host)), &mut |c, l, p| {
                fake.serve(c, l, p)
            })
            .unwrap();
    }

    #[test]
    fn the_current_replica_is_found_offline_by_flag_then_local_then_env_then_remembered() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_host(dir.path(), "alpha", "alpha.local");
        replica_with_host(dir.path(), "beta", "beta.local");
        replica_with_host(dir.path(), "pi", LOCAL_HOST);
        let pick = |explicit, local, env, remembered| {
            pick_replica(dir.path(), explicit, local, env, remembered)
        };
        let beta_cached = Some(("beta", "beta.local"));

        // A flag wins, by host or by id, and never falls back to something else.
        assert_eq!(
            pick(Some("beta.local"), true, Some("alpha.local"), beta_cached),
            Some(id("beta"))
        );
        assert_eq!(pick(Some("beta"), false, None, None), Some(id("beta")));
        assert_eq!(
            pick(Some("gamma.local"), true, Some("alpha.local"), beta_cached),
            None
        );
        // On the node itself, the replica synced as localhost beats MDMA_NODE and node.json.
        assert_eq!(
            pick(None, true, Some("alpha.local"), beta_cached),
            Some(id("pi"))
        );
        assert_eq!(pick(None, true, None, None), Some(id("pi")));
        assert_eq!(
            pick(None, false, Some("alpha.local"), beta_cached),
            Some(id("alpha")),
            "not local: localhost is not tried"
        );
        // MDMA_NODE against the hosts.
        assert_eq!(
            pick(None, false, Some("alpha.local"), None),
            Some(id("alpha"))
        );
        // node.json only when MDMA_NODE is unset or names its host.
        assert_eq!(pick(None, false, None, beta_cached), Some(id("beta")));
        assert_eq!(
            pick(None, false, Some("unknown.local"), beta_cached),
            None,
            "MDMA_NODE names another node: node.json's replica is not it"
        );
        assert_eq!(
            pick(
                None,
                false,
                Some("BETA.local"),
                Some(("beta-old", "beta.local"))
            ),
            Some(id("beta")),
            "the hosts match first"
        );
        assert_eq!(
            pick(
                None,
                false,
                Some("gone.local"),
                Some(("zeta", "gone.local"))
            ),
            Some(id("zeta")),
            "node.json's host is the one MDMA_NODE names"
        );
        assert_eq!(pick(None, false, None, None), None);
    }

    // --- reset ---------------------------------------------------------------

    #[test]
    fn what_was_looked_for_is_the_flag_else_this_node_when_local_else_the_environment() {
        assert_eq!(
            Missing::named(Some("flag.local".into()), true, Some("env.local")),
            Missing::Named("flag.local".into()),
            "a flag is looked for even on the node"
        );
        assert_eq!(
            Missing::named(None, false, Some("env.local")),
            Missing::Named("env.local".into())
        );
        assert_eq!(Missing::named(None, false, None), Missing::NoNode);
        assert_eq!(Missing::named(None, true, None), Missing::LocalNotSynced);
        assert_eq!(
            Missing::named(None, true, Some("env.local")),
            Missing::LocalNotSynced,
            "locally the node is this one, as in pick_replica"
        );
    }

    #[test]
    fn with_no_replica_yet_the_node_itself_says_so_and_a_non_local_setup_has_no_node() {
        assert_eq!(
            Missing::LocalNotSynced.message(),
            "no replica of this node yet; a background sync creates it, or run `mdma replica sync`"
        );
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            reset_replica(dir.path(), Err(Missing::LocalNotSynced)).unwrap(),
            "Nothing to reset: no replica of this node yet."
        );
        assert_eq!(
            Missing::named(None, false, None).message(),
            "no node to find a replica for; use --node NAME or run `mdma node select`"
        );
    }

    #[test]
    fn the_probes_use_the_gateway_the_sync_reads_through() {
        assert_eq!(
            sync_gateway(Some("tcp://10.0.0.5:5555".into())),
            "tcp://10.0.0.5:5555"
        );
        assert_eq!(
            sync_gateway(None),
            format!("tcp://127.0.0.1:{}", node_discovery::DEFAULT_PORT)
        );
    }

    #[test]
    fn resetting_a_name_that_matches_no_replica_is_nothing_to_reset_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_host(dir.path(), "stage", "mdma-stage2.local");

        let say = |chosen| reset_replica(dir.path(), chosen).unwrap();

        assert_eq!(
            say(Err(Missing::Named("mdma-gone.local".into()))),
            "Nothing to reset: no replica for 'mdma-gone.local'."
        );
        assert_eq!(say(Ok(id("stage"))), "Deleted the replica of stage");
        assert_eq!(
            say(Ok(id("stage"))),
            "Nothing to reset: no replica of stage."
        );
        let no_node = reset_replica(dir.path(), Err(Missing::NoNode));
        assert_eq!(
            crate::test_support::failure_text(no_node),
            "no node to find a replica for; use --node NAME or run `mdma node select`"
        );
    }

    #[test]
    fn a_second_reset_of_the_same_node_exits_zero_and_status_still_reports_the_missing_replica() {
        use crate::ctx::NodeFlag;
        use crate::test_support::{env_lock, failure_text, globals, EnvVars};

        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut env = EnvVars::default();
        env.set("XDG_CACHE_HOME", dir.path().join("cache"));
        env.set("XDG_CONFIG_HOME", dir.path().join("config"));
        env.set("MDMA_NODE", "");
        let nodes_dir = dir.path().join("cache").join("mdma").join("nodes");
        replica_with_host(&nodes_dir, "stage", "mdma-stage2.local");
        let mut given = globals();
        given.node = NodeFlag(Some("mdma-stage2.local".parse().unwrap()));
        let ctx = Ctx::with_target(given, node_discovery::Target::LocalIpc);

        reset(&ctx).expect("the first reset deletes the replica");
        reset(&ctx).expect("the second has nothing to do, and that is not a failure");

        assert_eq!(
            failure_text(status(&ctx)),
            "no replica for 'mdma-stage2.local'",
            "status keeps its error"
        );
    }

    // --- syncing -------------------------------------------------------------

    fn node_with(lines: &[&str]) -> FakeNode {
        let mut node = FakeNode::new("e1", 0);
        node.lines = lines.iter().map(ToString::to_string).collect();
        node
    }

    fn sync_now(
        dir: &Path,
        node_id: &str,
        host: Option<&Host>,
        node: &FakeNode,
        under_lock: impl FnOnce(&Replica, &SyncLock, &SyncReport) -> Result<()>,
    ) -> Result<Option<SyncReport>> {
        sync_replica(
            dir,
            &id(node_id),
            host,
            secs(10),
            &mut |c, l, p| node.serve(c, l, p),
            under_lock,
        )
    }

    #[test]
    fn syncing_remembers_the_host_and_moves_it_off_the_node_that_used_to_have_it() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_host(dir.path(), "old-node", "shared.local");
        let host = Host::new("shared.local");
        let node = node_with(&["[\"a\"]"]);

        let report = sync_now(dir.path(), "new-node", Some(&host), &node, |_, _, _| Ok(()))
            .unwrap()
            .expect("not busy");

        assert_eq!(report.added, fact_replica::LineCount::new(1));
        let replica = Replica::new(dir.path(), &id("new-node"));
        assert_eq!(replica.lines().unwrap(), ["[\"a\"]"]);
        assert_eq!(id_for_host(dir.path(), &host), Some(id("new-node")));
        let old = Replica::new(dir.path(), &id("old-node"));
        assert!(old.meta().unwrap().unwrap().hosts.is_empty());
    }

    #[test]
    fn the_probe_is_written_while_the_sync_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let node = node_with(&["[\"a\"]"]);
        let mut lock_was_held = false;

        sync_now(dir.path(), "n", None, &node, |replica, _, _| {
            // A reset (or another sync) would get the lock if it were released.
            lock_was_held = replica.try_lock().unwrap().is_none();
            Probe::default()
                .updated(secs(10), Some(vec!["p".into()]), None)
                .save(replica)
        })
        .unwrap();

        assert!(lock_was_held);
        let replica = Replica::new(dir.path(), &id("n"));
        assert!(Probe::load(&replica).playlists.is_some());
        assert!(replica.try_lock().unwrap().is_some(), "released afterwards");
    }

    #[test]
    fn a_busy_replica_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let node = node_with(&["[\"a\"]"]);
        let replica = Replica::new(dir.path(), &id("n"));
        let _held = replica.try_lock().unwrap().unwrap();

        let outcome = sync_now(dir.path(), "n", None, &node, |_, _, _| {
            panic!("nothing to do under a lock we do not hold")
        })
        .unwrap();

        assert_eq!(outcome, None);
        assert!(node.requests().is_empty());
    }

    #[test]
    fn a_sync_error_is_a_single_plain_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut node = node_with(&["[\"a\"]"]);
        node.versioned = false;

        let err = sync_now(dir.path(), "n", None, &node, |_, _, _| Ok(())).unwrap_err();

        assert_eq!(
            crate::test_support::failure_text(Err::<(), _>(err)),
            "node too old for the replica"
        );
    }

    // --- failures before the store ---------------------------------------------

    fn unreachable(dir: &Path, node: &str, now: u64) {
        let failed = reach(dir, Some(&id(node)), secs(now), || {
            Err::<(), _>(eyre!("DNS resolution failed: No IPv4 address found"))
        });
        assert!(failed.is_err(), "the error is passed on");
    }

    fn meta_of(dir: &Path, node: &str) -> Meta {
        Replica::new(dir, &id(node)).meta().unwrap().unwrap()
    }

    #[test]
    fn a_sync_that_cannot_connect_backs_the_replica_off_and_a_success_resets_it() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_host(dir.path(), "n", "nonexistent.invalid");

        unreachable(dir.path(), "n", 1_000);
        let meta = meta_of(dir.path(), "n");
        assert_eq!(meta.failures, 1);
        assert_eq!(meta.last_attempt, Some(secs(1_000)));
        assert!(!should_attempt(secs(1_059), Some(&meta)));
        assert!(should_attempt(secs(1_060), Some(&meta)), "60 s");

        unreachable(dir.path(), "n", 1_060);
        let meta = meta_of(dir.path(), "n");
        assert_eq!(meta.failures, 2);
        assert!(!should_attempt(secs(1_179), Some(&meta)));
        assert!(should_attempt(secs(1_180), Some(&meta)), "120 s");

        let node = node_with(&["[\"a\"]"]);
        sync_now(dir.path(), "n", None, &node, |_, _, _| Ok(()))
            .unwrap()
            .expect("not busy");
        assert_eq!(meta_of(dir.path(), "n").failures, 0);
    }

    #[test]
    fn a_step_that_works_or_has_no_replica_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_host(dir.path(), "n", "h.local");
        let before = meta_of(dir.path(), "n");

        let ok = reach(dir.path(), Some(&id("n")), secs(5_000), || Ok(7)).unwrap();
        assert_eq!(ok, 7);
        assert_eq!(meta_of(dir.path(), "n"), before);

        let failed = reach(dir.path(), None, secs(5_000), || Err::<(), _>(eyre!("no")));
        assert!(failed.is_err());
        assert_eq!(meta_of(dir.path(), "n"), before);
    }

    #[test]
    fn a_failure_is_not_counted_while_another_sync_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_host(dir.path(), "n", "h.local");
        let replica = Replica::new(dir.path(), &id("n"));
        let _running = replica.try_lock().unwrap().unwrap();

        unreachable(dir.path(), "n", 9_000);

        assert_eq!(meta_of(dir.path(), "n").failures, 0);
    }

    // --- status --------------------------------------------------------------

    #[test]
    fn ago_reads_in_the_unit_that_fits() {
        let now = secs(1_000_000);
        let at = |back: u64| ago(now, Some(secs(1_000_000 - back)));
        assert_eq!(at(5), "5 s ago");
        assert_eq!(at(89), "89 s ago");
        assert_eq!(at(120), "2 min ago");
        assert_eq!(at(7200), "2 h ago");
        assert_eq!(at(3 * 86_400), "3 d ago");
        assert_eq!(ago(now, None), "never");
        assert_eq!(
            ago(secs(5), Some(secs(10))),
            "in the future (clock changed)"
        );
    }

    #[test]
    fn status_shows_the_replica_and_the_backoff() {
        let now = secs(10_000);
        let meta = Meta {
            committed: fact_replica::Committed::Lines {
                epoch: fact_replica::Epoch::new("e7"),
                lines: std::num::NonZeroUsize::new(42).unwrap(),
                last_line_sha256: fact_replica::Digest::of_line("x"),
            },
            synced_at: Some(secs(9_000)),
            last_attempt: Some(secs(9_990)),
            failures: 2,
            hosts: vec![Host::new("mdma.local")],
        };
        let probe = Probe {
            playlists: Some(probed(9_000, &["a", "b"])),
            sources: None,
        };

        let text = render_status(&id("mdma-1"), &meta, &probe, 0, now);

        assert_eq!(
            text,
            "Node:          mdma-1\n\
             Epoch:         e7\n\
             Lines:         42\n\
             Synced:        16 min ago\n\
             Last attempt:  10 s ago\n\
             Next attempt:  next in 110 s (2 failed attempts in a row)\n\
             Hosts:         mdma.local\n\
             Playlists:     2 names, probed 16 min ago\n\
             Sources:       none"
        );
    }

    #[test]
    fn status_says_due_now_when_the_interval_has_passed() {
        let meta = Meta {
            last_attempt: Some(secs(1_000)),
            ..Meta::default()
        };
        let text = render_status(&id("n"), &meta, &Probe::default(), 0, secs(2_000));
        assert!(text.contains("Next attempt:  due now\n"), "{text}");
        assert!(text.contains("Synced:        never"), "{text}");
        assert!(!text.contains("Unreadable"), "{text}");
    }

    #[test]
    fn status_reports_lines_this_mdma_cannot_read() {
        let none = Probe::default();

        let text = render_status(&id("n"), &Meta::default(), &none, 3, secs(2_000));

        assert!(
            text.ends_with("Unreadable:    3 lines this mdma cannot read; upgrade mdma"),
            "{text}"
        );
    }

    // --- folding -------------------------------------------------------------

    const TRACK: &str = "sha256:aabbccdd00112233";

    /// A track with one play.
    fn play_lines() -> Vec<String> {
        use crate::test_support::asserted;
        let at = "2026-01-02T10:00:00Z";
        let title = r#"{"t":"Title","v":"Blue Monday"}"#;
        let started = r#"{"t":"TrackStarted","v":"OnRequest"}"#;
        vec![asserted(TRACK, title, at), asserted(TRACK, started, at)]
    }

    fn replica_with_lines(dir: &Path, node: &str, host: &str, lines: Vec<String>) {
        let mut fake = FakeNode::new("e1", 0);
        fake.lines = lines;
        Replica::new(dir, &id(node))
            .sync(secs(1), Some(&Host::new(host)), &mut |c, l, p| {
                fake.serve(c, l, p)
            })
            .unwrap();
    }

    #[test]
    fn folding_a_replica_gives_its_node_its_age_and_its_projection() {
        let dir = tempfile::tempdir().unwrap();
        replica_with_lines(dir.path(), "stage", "mdma-stage2.local", play_lines());

        let fold = fold_replica(dir.path(), id("stage")).unwrap();

        assert_eq!(fold.id, id("stage"));
        assert_eq!(fold.synced_at, Some(secs(1)));
        assert_eq!(fold.projection.unparseable_lines, 0);
        let tracks = &fold.projection.tracks;
        assert_eq!(tracks.len(), 1);
        assert_eq!(fold.projection.plays.histories(tracks)[0].count(), 1);
    }

    #[test]
    fn folding_a_node_with_no_replica_yet_says_how_to_get_one() {
        let dir = tempfile::tempdir().unwrap();

        let result = fold_replica(dir.path(), id("fresh"));

        assert_eq!(
            crate::test_support::failure_text(result),
            "no replica of fresh yet; a background sync creates it, or run `mdma replica sync`"
        );
    }

    #[test]
    fn the_current_replica_is_folded_from_the_cache_and_a_missing_one_is_an_error() {
        use crate::ctx::NodeFlag;
        use crate::test_support::{env_lock, failure_text, globals, EnvVars};

        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut env = EnvVars::default();
        env.set("XDG_CACHE_HOME", dir.path().join("cache"));
        env.set("XDG_CONFIG_HOME", dir.path().join("config"));
        env.set("MDMA_NODE", "");
        let nodes_dir = dir.path().join("cache").join("mdma").join("nodes");
        replica_with_lines(&nodes_dir, "stage", "mdma-stage2.local", play_lines());
        // A replica directory that no sync has filled yet.
        let bare = Replica::new(&nodes_dir, &id("bare"));
        std::fs::create_dir_all(bare.dir()).unwrap();
        let ctx_for = |host: &str| {
            let mut given = globals();
            given.node = NodeFlag(Some(host.parse().unwrap()));
            Ctx::with_target(given, node_discovery::Target::LocalIpc)
        };

        let fold = fold_current(&ctx_for("mdma-stage2.local")).unwrap();
        let unsynced = fold_current(&ctx_for("bare"));

        assert_eq!(fold.id, id("stage"));
        assert_eq!(fold.projection.tracks.len(), 1);
        assert_eq!(
            failure_text(unsynced),
            "no replica of bare yet; a background sync creates it, or run `mdma replica sync`"
        );
    }

    #[test]
    fn an_explicit_node_picks_its_own_replica_with_no_fallback() {
        use crate::test_support::{env_lock, EnvVars};

        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut env = EnvVars::default();
        env.set("XDG_CONFIG_HOME", dir.path().join("config"));
        env.set("MDMA_NODE", "alpha.local");
        env.set("MDMA_LIBRARY_SOCKET", "");
        env.set("MDMA_PLAYBACK_SOCKET", "");
        replica_with_host(dir.path(), "alpha", "alpha.local");
        replica_with_host(dir.path(), "beta", "beta.local");
        let pick = |named| replica_from_environment(dir.path(), named);

        assert_eq!(pick(None), Some(id("alpha")), "MDMA_NODE decides");
        assert_eq!(pick(Some("beta.local")), Some(id("beta")), "--node wins");
        assert_eq!(pick(Some("gamma.local")), None, "and does not fall back");
    }
}
