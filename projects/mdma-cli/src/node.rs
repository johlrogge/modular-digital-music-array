//! Node discovery wiring for the CLI: resolving which node to talk to, and the
//! `mdma node` subcommands.
//!
//! The decisions live in `node_discovery`; this module supplies the real
//! effects (gateway identify, DNS, mDNS browse, TTY prompt) and the user-facing
//! rendering, which is kept in pure functions.

use std::io::{BufRead, IsTerminal, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use crate::ctx::Ctx;
use gateway_client::{GatewayClient, IdentifyError, NodeIdentity};
use node_discovery::{
    cache, resolve, DiscoveredNode, Effects, IdentityCheck, NodeCache, NodeKind, NodeSpec,
    RemoteTarget, Request, ResolveError, SelectEffects, SelectError, Source, Target, Unreachable,
    VerifyError, BROWSE_TIMEOUT,
};

/// How long to wait for a gateway to answer an identify request.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(1);

/// Environment variable naming the node (split from `--node` so the two can be
/// told apart).
const NODE_ENV: &str = "MDMA_NODE";

/// True when the CLI should talk to the local services over IPC instead of a
/// gateway: sockets were given explicitly, or the library socket exists here.
pub fn local_ipc_wanted(sockets_given_explicitly: bool, library_socket_exists: bool) -> bool {
    sockets_given_explicitly || library_socket_exists
}

/// Gateway client settings for a resolved target. Prefers the IP so NNG
/// connects do not depend on mDNS. No target, or local IPC, means direct IPC.
pub fn client_config(target: Option<&Target>) -> client::ClientConfig {
    match target {
        Some(Target::Remote(t)) => client::ClientConfig {
            node: Some(address_of(t)),
            gateway_port: t.port,
            ..Default::default()
        },
        Some(Target::LocalIpc) | None => client::ClientConfig::default(),
    }
}

/// The IP when known, else the host name.
fn address_of(target: &RemoteTarget) -> String {
    target
        .ip
        .map_or_else(|| target.host.clone(), |ip| ip.to_string())
}

/// Host and IP of a remote node, for tools that are not NNG (scp, HTTP).
/// `None` for local IPC or no target.
pub fn remote_host(target: Option<&Target>) -> Option<(String, Option<Ipv4Addr>)> {
    match target {
        Some(Target::Remote(t)) => Some((t.host.clone(), t.ip)),
        Some(Target::LocalIpc) | None => None,
    }
}

/// One-line note for sources the user did not state explicitly.
pub fn source_note(target: &RemoteTarget) -> Option<String> {
    let how = match &target.source {
        Source::CacheUnverified => "cached, identity could not be verified".to_string(),
        Source::CacheRelocated { from, to } => format!("cached, moved from {from} to {to}"),
        Source::Discovered => "discovered".to_string(),
        Source::Picked => "selected".to_string(),
        Source::Explicit | Source::Env | Source::Cache => return None,
    };
    let at = target.ip.map_or_else(String::new, |ip| format!(" ({ip})"));
    Some(format!("Using {}{at}, {how}", target.host))
}

/// Where a target came from, for `mdma node show`.
pub fn source_description(source: &Source) -> String {
    match source {
        Source::Explicit => "--node".to_string(),
        Source::Env => format!("{NODE_ENV} environment variable"),
        Source::Cache => "cached node (verified)".to_string(),
        Source::CacheUnverified => "cached node (identity not verified)".to_string(),
        Source::CacheRelocated { from, to } => format!("cached node (moved from {from} to {to})"),
        Source::Discovered => "discovered on the network".to_string(),
        Source::Picked => "selected from the network".to_string(),
    }
}

fn display_name(node: &DiscoveredNode) -> &str {
    node.name
        .as_deref()
        .or(node.id.as_deref())
        .unwrap_or("(unconfigured)")
}

fn ips(node: &DiscoveredNode) -> String {
    if node.ipv4s.is_empty() {
        "-".to_string()
    } else {
        node.ipv4s
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Numbered list of nodes to choose from.
pub fn render_pick_list(nodes: &[DiscoveredNode]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "  {}) {}  {}  {}  {}",
                i + 1,
                display_name(n),
                n.host,
                ips(n),
                n.version.as_deref().unwrap_or("-"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the user typed at the pick prompt.
#[derive(Debug, PartialEq, Eq)]
pub enum PickInput {
    /// Zero-based index of the chosen node.
    Choice(usize),
    Cancel,
    Invalid,
}

/// Parse a 1-based choice out of `len` nodes. Empty input or `q` cancels.
pub fn parse_pick(input: &str, len: usize) -> PickInput {
    let input = input.trim();
    if input.is_empty() || input.eq_ignore_ascii_case("q") {
        return PickInput::Cancel;
    }
    match input.parse::<usize>() {
        Ok(n) if (1..=len).contains(&n) => PickInput::Choice(n - 1),
        _ => PickInput::Invalid,
    }
}

/// Table for `mdma node list`; the cached node is marked with `*`.
pub fn render_node_table(nodes: &[DiscoveredNode], cached_id: Option<&str>) -> String {
    let header = ["", "ID", "NAME", "HOST", "IP", "VERSION", "KIND"].map(String::from);
    let rows = nodes.iter().map(|n| {
        let marked = n.id.is_some() && n.id.as_deref() == cached_id;
        [
            if marked { "*" } else { "" }.to_string(),
            n.id.clone().unwrap_or_else(|| "-".into()),
            n.name.clone().unwrap_or_else(|| "-".into()),
            n.host.clone(),
            ips(n),
            n.version.clone().unwrap_or_else(|| "-".into()),
            match n.kind {
                NodeKind::Node => "node",
                NodeKind::Beacon => "beacon",
            }
            .to_string(),
        ]
    });
    let rows: Vec<[String; 7]> = std::iter::once(header).chain(rows).collect();
    let widths: Vec<usize> = (0..7)
        .map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();
    rows.iter()
        .map(|r| {
            r.iter()
                .zip(&widths)
                .map(|(cell, w)| format!("{cell:<w$}"))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Message for a resolution failure, with advice on what to do next.
pub fn resolve_error_message(err: &ResolveError) -> String {
    match err {
        ResolveError::CachedNodeNotFound { id, others } => {
            let mut msg = format!("The remembered node '{id}' was not found on the network.");
            if others.is_empty() {
                msg.push_str("\nNo other MDMA nodes were found either.");
                msg.push_str("\nIs it powered on? Use --node <host> to reach it directly.");
            } else {
                msg.push_str("\nOther nodes on the network:\n");
                msg.push_str(&render_pick_list(others));
                msg.push_str("\nRun `mdma node select <name>` to switch to one of them.");
            }
            msg
        }
        ResolveError::NoNodesFound => "No MDMA nodes found on the network.\n\
             Use --node <host> (or set MDMA_NODE) to name one. \
             Discovery only works on the local network."
            .to_string(),
        ResolveError::Ambiguous(nodes) => format!(
            "{} MDMA nodes found and no terminal to choose from:\n{}\n\
             Run `mdma node select <name>` to pick one, or use --node <host>.",
            nodes.len(),
            render_pick_list(nodes)
        ),
        ResolveError::Cancelled => "Node selection cancelled.".to_string(),
        ResolveError::Unverified(e) => verify_error_message(e),
    }
}

/// Message for a failed `mdma node select`.
pub fn select_error_message(err: &SelectError) -> String {
    match err {
        SelectError::NoNodes => resolve_error_message(&ResolveError::NoNodesFound),
        SelectError::NoMatch { query, nodes } => format!(
            "No node matches '{query}'. Nodes found:\n{}",
            render_pick_list(nodes)
        ),
        SelectError::AmbiguousMatch { query, matches } => format!(
            "'{query}' matches several nodes; use the id:\n{}",
            render_pick_list(matches)
        ),
        SelectError::NeedsChoice(nodes) => format!(
            "{} MDMA nodes found and no terminal to choose from:\n{}\n\
             Run `mdma node select <name>` to pick one.",
            nodes.len(),
            render_pick_list(nodes)
        ),
        SelectError::Cancelled => resolve_error_message(&ResolveError::Cancelled),
        SelectError::Unreachable(node) => unreachable_message(node),
        SelectError::IdentifyFailed(node) => identify_failed_message(node),
        SelectError::IdentityMismatch { node, reported } => {
            identity_mismatch_message(node, reported.as_deref())
        }
        SelectError::NotRememberable(node) => format!(
            "{} has no id or address (it is not configured yet), so it cannot be remembered.\n\
             Use --node {} instead.",
            display_name(node),
            node.host
        ),
    }
}

/// Message for a discovered node that could not be verified while resolving
/// (not selecting): the advice is about connecting, not remembering.
pub fn verify_error_message(err: &VerifyError) -> String {
    match err {
        VerifyError::Unreachable(node) => unreachable_message(node),
        VerifyError::IdentifyFailed(node) => identify_failed_message(node),
        VerifyError::IdentityMismatch { node, reported } => {
            identity_mismatch_message(node, reported.as_deref())
        }
        VerifyError::NoAddress(node) => format!(
            "{} was found but has no IPv4 address, so it cannot be connected to.\n\
             Use --node {} instead.",
            display_name(node),
            node.host
        ),
        VerifyError::UnverifiableUnannounced { node, .. } => format!(
            "{} is not configured yet and cannot be verified. Use --node {} instead.",
            display_name(node),
            node.host
        ),
    }
}

fn unreachable_message(node: &DiscoveredNode) -> String {
    format!("{} ({}) did not answer.", display_name(node), ips(node))
}

fn identify_failed_message(node: &DiscoveredNode) -> String {
    format!(
        "{} ({}) answered, but its gateway failed to say who it is. Retry, or use --node {}.",
        display_name(node),
        ips(node),
        node.host
    )
}

fn identity_mismatch_message(node: &DiscoveredNode, reported: Option<&str>) -> String {
    let announced = node.id.as_deref().unwrap_or("-");
    let answer = match reported {
        Some(id) => format!("answers as '{id}'"),
        None => "gives no id".to_string(),
    };
    format!(
        "{} announced id '{announced}' but {} {answer}; \
         the network view is inconsistent. Retry, or use --node {}.",
        display_name(node),
        ips(node),
        node.host
    )
}

/// Map an identify answer to the resolver's view of it. A gateway that
/// answered but cannot say who it is (old, or unconfigured) is unverifiable;
/// one that answered with an error has failed; no transport is unreachable.
pub fn identity_check(
    answer: Result<NodeIdentity, IdentifyError>,
) -> Result<IdentityCheck, Unreachable> {
    use gateway_client::ClientError;
    match answer {
        Ok(NodeIdentity { id: Some(id), .. }) => Ok(IdentityCheck::Id(id)),
        Ok(NodeIdentity { id: None, .. }) | Err(IdentifyError::Unsupported(_)) => {
            Ok(IdentityCheck::Unverifiable)
        }
        Err(IdentifyError::Client(ClientError::Service(_))) => Ok(IdentityCheck::Failed),
        Err(IdentifyError::Client(_)) => Err(Unreachable),
    }
}

/// Ask the gateway at `ip:port` who it is; connecting and the answer are each
/// bounded by [`IDENTIFY_TIMEOUT`].
fn identify_at(ip: Ipv4Addr, port: u16) -> Result<IdentityCheck, Unreachable> {
    let address = format!("tcp://{ip}:{port}");
    let client =
        GatewayClient::connect_with_timeout(&address, IDENTIFY_TIMEOUT).map_err(|_| Unreachable)?;
    identity_check(client.identify())
}

fn resolve_name_fresh(host: &str) -> Option<Ipv4Addr> {
    nng_transport::resolve_hostname_to_ipv4_fresh(host)
        .ok()?
        .parse()
        .ok()
}

/// Ask on stderr/stdin which node to use. `None` on cancel or end of input.
fn pick_interactive(nodes: &[DiscoveredNode]) -> Option<usize> {
    eprintln!("Several MDMA nodes are available:");
    eprintln!("{}", render_pick_list(nodes));
    let stdin = std::io::stdin();
    loop {
        eprint!("Choose a node [1-{}], empty to cancel: ", nodes.len());
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).ok()? == 0 {
            return None;
        }
        match parse_pick(&line, nodes.len()) {
            PickInput::Choice(i) => return Some(i),
            PickInput::Cancel => return None,
            PickInput::Invalid => eprintln!("Not a valid choice."),
        }
    }
}

fn is_tty() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn real_effects() -> Effects<'static> {
    Effects {
        identify_at: Box::new(identify_at),
        resolve_name_fresh: Box::new(resolve_name_fresh),
        browse: Box::new(node_discovery::browse_for),
        pick: Box::new(pick_interactive),
        load_cache: Box::new(load_cache),
        is_tty: is_tty(),
    }
}

/// `MDMA_NODE`. Empty counts as unset, so `MDMA_NODE= mdma ...` overrides a
/// value exported by the environment (devenv sets one).
fn parse_env_node(value: Option<&str>) -> Result<Option<NodeSpec>, String> {
    match value.filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|e| format!("error: invalid {NODE_ENV}: {e}")),
    }
}

fn env_node() -> Result<Option<NodeSpec>, String> {
    parse_env_node(std::env::var(NODE_ENV).ok().as_deref())
}

fn cache_path() -> Option<PathBuf> {
    cache::default_path()
}

fn load_cache() -> Option<NodeCache> {
    let path = cache_path()?;
    match cache::load(&path) {
        Ok(cached) => cached,
        Err(e) => {
            eprintln!("warning: ignoring remembered node: {e}");
            None
        }
    }
}

fn save_cache(update: &NodeCache) {
    let Some(path) = cache_path() else { return };
    if let Err(e) = cache::save(&path, update) {
        eprintln!("warning: could not remember node: {e}");
    }
}

/// The resolution request; a bad `MDMA_NODE` prints the error and exits 1,
/// like an unresolvable node does.
fn request(explicit: Option<&NodeSpec>, local_ipc: bool) -> Request {
    let env = env_node().unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(1);
    });
    Request {
        explicit: explicit.cloned(),
        env,
        local_ipc,
    }
}

/// Resolve the node once at startup, remembering it when the resolver says
/// so. Prints a note for implicit choices and exits with advice on failure.
pub fn resolve_startup(explicit: Option<&NodeSpec>, local_ipc: bool) -> Target {
    match resolve(request(explicit, local_ipc), &mut real_effects()) {
        Ok(resolution) => {
            if let Some(update) = &resolution.cache_update {
                save_cache(update);
            }
            if let Target::Remote(remote) = &resolution.target {
                if let Some(note) = source_note(remote) {
                    eprintln!("{note}");
                }
            }
            resolution.target
        }
        Err(e) => {
            eprintln!("{}", resolve_error_message(&e));
            std::process::exit(1);
        }
    }
}

#[derive(clap::Subcommand, Debug)]
pub(crate) enum Command {
    /// List MDMA nodes on the network (* marks the remembered node)
    List,
    /// Remember a node (by name or id; prompts when omitted)
    Select {
        /// Node name or id
        name: Option<String>,
    },
    /// Forget the remembered node
    Forget,
    /// Show which node commands would use, and why
    Show,
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> color_eyre::Result<()> {
    match command {
        Command::List => list(ctx),
        Command::Select { name } => select(OptNodeQueryArg(name), ctx),
        Command::Forget => forget(ctx),
        Command::Show => show(ctx),
    }
}

/// `mdma node list`
pub(crate) fn list(_ctx: &Ctx) -> color_eyre::Result<()> {
    let nodes = node_discovery::browse(BROWSE_TIMEOUT);
    if nodes.is_empty() {
        println!("No MDMA nodes found on the network.");
        return Ok(());
    }
    let cached = load_cache();
    println!(
        "{}",
        render_node_table(&nodes, cached.as_ref().map(|c| c.id.as_str()))
    );
    Ok(())
}

/// A node name or id as typed, matched against the discovered nodes (none: prompt).
// Stays a String: any text is a valid query; no match is reported by the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptNodeQueryArg(pub Option<String>);

/// `mdma node select [name|id]`
pub(crate) fn select(query: OptNodeQueryArg, _ctx: &Ctx) -> color_eyre::Result<()> {
    let OptNodeQueryArg(query) = query;
    let nodes = node_discovery::browse(BROWSE_TIMEOUT);
    let mut effects = SelectEffects {
        identify_at: Box::new(identify_at),
        pick: Box::new(pick_interactive),
        is_tty: is_tty(),
    };
    let entry = match node_discovery::select(nodes, query.as_deref(), &mut effects) {
        Ok(entry) => entry,
        Err(e) => {
            eprintln!("{}", select_error_message(&e));
            std::process::exit(1);
        }
    };
    let Some(path) = cache_path() else {
        eprintln!("No config directory is available to remember the node in.");
        std::process::exit(1);
    };
    if let Err(e) = cache::save(&path, &entry) {
        eprintln!("Could not remember node: {e}");
        std::process::exit(1);
    }
    println!(
        "Selected {} ({}, {}:{}), saved to {}",
        entry.name,
        entry.id,
        entry.ip,
        entry.port,
        path.display()
    );
    Ok(())
}

/// `mdma node forget`
pub(crate) fn forget(_ctx: &Ctx) -> color_eyre::Result<()> {
    let Some(path) = cache_path() else {
        println!("No config directory; nothing to forget.");
        return Ok(());
    };
    let previous = load_cache();
    if let Err(e) = cache::forget(&path) {
        eprintln!("Could not forget node: {e}");
        std::process::exit(1);
    }
    match previous {
        Some(c) => println!("Forgot {} ({})", c.name, c.id),
        None => println!("No remembered node."),
    }
    Ok(())
}

/// `mdma node show`: resolve like any command would, but save nothing.
pub(crate) fn show(ctx: &Ctx) -> color_eyre::Result<()> {
    match resolve(
        request(ctx.node_flag(), ctx.local_ipc()),
        &mut real_effects(),
    ) {
        Ok(resolution) => {
            match &resolution.target {
                Target::LocalIpc => println!("Local services over IPC (no node)"),
                Target::Remote(t) => {
                    println!("Node:   {}", t.host);
                    println!(
                        "IP:     {}",
                        t.ip.map_or("-".to_string(), |ip| ip.to_string())
                    );
                    println!("Port:   {}", t.port);
                    println!("Source: {}", source_description(&t.source));
                }
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("{}", resolve_error_message(&e));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn env_node_unset_or_empty_is_not_given() {
        assert_eq!(parse_env_node(None), Ok(None));
        assert_eq!(parse_env_node(Some("")), Ok(None));
    }

    #[test]
    fn env_node_is_parsed_as_host_and_port() {
        let spec = parse_env_node(Some("mdma.local:6000")).unwrap().unwrap();
        assert_eq!((spec.host(), spec.port()), ("mdma.local", Some(6000)));
        let spec = parse_env_node(Some("mdma.local")).unwrap().unwrap();
        assert_eq!((spec.host(), spec.port()), ("mdma.local", None));
    }

    #[test]
    fn bad_env_node_names_the_variable() {
        assert_eq!(
            parse_env_node(Some("mdma.local:nope")),
            Err(
                "error: invalid MDMA_NODE: invalid port 'nope' in node 'mdma.local:nope', \
                 expected 1-65535"
                    .to_string()
            )
        );
    }

    fn stage() -> DiscoveredNode {
        DiscoveredNode {
            id: Some("mdma-stage".into()),
            name: Some("Stage".into()),
            host: "mdma-stage.local".into(),
            ipv4s: vec![Ipv4Addr::new(192, 168, 0, 238)],
            port: 5555,
            version: Some("0.29.0".into()),
            kind: NodeKind::Node,
        }
    }

    fn prod() -> DiscoveredNode {
        DiscoveredNode {
            id: Some("mdma-prod".into()),
            name: None,
            host: "mdma-prod.local".into(),
            ipv4s: vec![Ipv4Addr::new(192, 168, 0, 10)],
            port: 5555,
            version: None,
            kind: NodeKind::Node,
        }
    }

    fn remote_target(source: Source) -> RemoteTarget {
        RemoteTarget {
            host: "mdma-stage.local".into(),
            ip: Some(Ipv4Addr::new(192, 168, 0, 238)),
            port: 5555,
            source,
        }
    }

    fn target(source: Source) -> Target {
        Target::Remote(remote_target(source))
    }

    // --- local_ipc ---------------------------------------------------------

    #[test]
    fn local_ipc_when_sockets_given_or_library_socket_exists() {
        assert!(local_ipc_wanted(true, false));
        assert!(local_ipc_wanted(false, true));
        assert!(local_ipc_wanted(true, true));
        assert!(!local_ipc_wanted(false, false));
    }

    // --- client_config -----------------------------------------------------

    #[test]
    fn client_config_prefers_ip_and_uses_target_port() {
        let mut t = remote_target(Source::Cache);
        t.port = 6000;
        let cfg = client_config(Some(&Target::Remote(t)));
        assert_eq!(cfg.gateway_addr(), Some("tcp://192.168.0.238:6000".into()));
        assert_eq!(cfg.event_addr(), Some("tcp://192.168.0.238:5556".into()));
    }

    #[test]
    fn client_config_falls_back_to_host_without_ip() {
        let mut t = remote_target(Source::Explicit);
        t.ip = None;
        assert_eq!(
            client_config(Some(&Target::Remote(t))).gateway_addr(),
            Some("tcp://mdma-stage.local:5555".into())
        );
    }

    #[test]
    fn client_config_is_direct_ipc_for_local_ipc_or_no_target() {
        assert_eq!(client_config(Some(&Target::LocalIpc)).gateway_addr(), None);
        assert_eq!(client_config(None).gateway_addr(), None);
        assert_eq!(client_config(None).event_addr(), None);
    }

    #[test]
    fn remote_host_is_absent_for_local_ipc() {
        assert_eq!(remote_host(None), None);
        assert_eq!(remote_host(Some(&Target::LocalIpc)), None);
        assert_eq!(
            remote_host(Some(&target(Source::Env))),
            Some((
                "mdma-stage.local".to_string(),
                Some(Ipv4Addr::new(192, 168, 0, 238))
            ))
        );
    }

    // --- notes -------------------------------------------------------------

    #[test]
    fn notes_only_for_implicit_sources() {
        assert_eq!(source_note(&remote_target(Source::Explicit)), None);
        assert_eq!(source_note(&remote_target(Source::Env)), None);
        assert_eq!(source_note(&remote_target(Source::Cache)), None);
        assert_eq!(
            source_note(&remote_target(Source::Discovered)).unwrap(),
            "Using mdma-stage.local (192.168.0.238), discovered"
        );
        assert!(source_note(&remote_target(Source::Picked))
            .unwrap()
            .ends_with("selected"));
        assert!(source_note(&remote_target(Source::CacheUnverified))
            .unwrap()
            .contains("could not be verified"));
        let moved = Source::CacheRelocated {
            from: Ipv4Addr::new(192, 168, 0, 20),
            to: Ipv4Addr::new(192, 168, 0, 238),
        };
        assert!(source_note(&remote_target(moved))
            .unwrap()
            .contains("moved from 192.168.0.20 to 192.168.0.238"));
    }

    // --- pick list ---------------------------------------------------------

    #[test]
    fn pick_list_shows_name_host_ip_and_version() {
        assert_eq!(
            render_pick_list(&[stage(), prod()]),
            "  1) Stage  mdma-stage.local  192.168.0.238  0.29.0\n  \
             2) mdma-prod  mdma-prod.local  192.168.0.10  -"
        );
    }

    #[test]
    fn parse_pick_accepts_one_based_numbers() {
        assert_eq!(parse_pick("1\n", 2), PickInput::Choice(0));
        assert_eq!(parse_pick(" 2 ", 2), PickInput::Choice(1));
    }

    #[test]
    fn parse_pick_cancels_on_empty_or_q() {
        assert_eq!(parse_pick("\n", 2), PickInput::Cancel);
        assert_eq!(parse_pick("q", 2), PickInput::Cancel);
    }

    #[test]
    fn parse_pick_rejects_out_of_range_and_garbage() {
        assert_eq!(parse_pick("0", 2), PickInput::Invalid);
        assert_eq!(parse_pick("3", 2), PickInput::Invalid);
        assert_eq!(parse_pick("two", 2), PickInput::Invalid);
        assert_eq!(parse_pick("-1", 2), PickInput::Invalid);
    }

    // --- table -------------------------------------------------------------

    #[test]
    fn table_marks_cached_node_and_aligns_columns() {
        let mut beacon = prod();
        beacon.kind = NodeKind::Beacon;
        beacon.id = None;
        let table = render_node_table(&[stage(), beacon], Some("mdma-stage"));
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("   ID"));
        assert!(lines[1].starts_with("*  mdma-stage"));
        assert!(lines[1].contains("192.168.0.238"));
        assert!(lines[2].starts_with("   -"));
        assert!(lines[2].ends_with("beacon"));
        // KIND column lines up across rows.
        let col = lines[0].find("KIND").unwrap();
        assert_eq!(lines[1].find("node"), Some(col));
        assert_eq!(lines[2].find("beacon"), Some(col));
    }

    #[test]
    fn table_marks_nothing_when_nothing_is_cached() {
        let table = render_node_table(&[stage()], None);
        assert!(!table.contains('*'));
    }

    // --- error messages ----------------------------------------------------

    #[test]
    fn cached_node_not_found_lists_others_and_suggests_select() {
        let msg = resolve_error_message(&ResolveError::CachedNodeNotFound {
            id: "mdma-stage".into(),
            others: vec![prod()],
        });
        assert!(msg.contains("'mdma-stage'"));
        assert!(msg.contains("mdma-prod.local"));
        assert!(msg.contains("mdma node select"));
    }

    #[test]
    fn cached_node_not_found_without_others_suggests_node_flag() {
        let msg = resolve_error_message(&ResolveError::CachedNodeNotFound {
            id: "mdma-stage".into(),
            others: vec![],
        });
        assert!(msg.contains("--node"));
        assert!(!msg.contains("mdma node select"));
    }

    #[test]
    fn no_nodes_found_mentions_node_flag_and_local_network() {
        let msg = resolve_error_message(&ResolveError::NoNodesFound);
        assert!(msg.contains("--node"));
        assert!(msg.contains("local network"));
    }

    #[test]
    fn ambiguous_lists_nodes_and_suggests_select() {
        let msg = resolve_error_message(&ResolveError::Ambiguous(vec![stage(), prod()]));
        assert!(msg.starts_with("2 MDMA nodes found"));
        assert!(msg.contains("mdma-stage.local"));
        assert!(msg.contains("mdma-prod.local"));
        assert!(msg.contains("mdma node select <name>"));
    }

    #[test]
    fn unverified_discovered_node_explains_the_mismatch() {
        let msg = resolve_error_message(&ResolveError::Unverified(
            node_discovery::VerifyError::IdentityMismatch {
                node: Box::new(stage()),
                reported: Some("mdma-prod".into()),
            },
        ));
        assert!(msg.contains("answers as 'mdma-prod'"));
        assert!(msg.contains("--node"));
    }

    #[test]
    fn resolve_time_no_address_talks_about_connecting_not_remembering() {
        let mut no_ip = stage();
        no_ip.ipv4s.clear();
        let msg = resolve_error_message(&ResolveError::Unverified(VerifyError::NoAddress(
            Box::new(no_ip.clone()),
        )));
        assert!(msg.contains("no IPv4 address") && msg.contains("--node mdma-stage.local"));
        assert!(!msg.contains("remembered"));
        // The same node while selecting is about remembering.
        let select =
            select_error_message(&SelectError::from(VerifyError::NoAddress(Box::new(no_ip))));
        assert!(select.contains("cannot be remembered"));
    }

    #[test]
    fn select_errors_explain_what_to_do() {
        let no_match = select_error_message(&SelectError::NoMatch {
            query: "nope".into(),
            nodes: vec![stage()],
        });
        assert!(no_match.contains("'nope'") && no_match.contains("mdma-stage.local"));

        let ambiguous = select_error_message(&SelectError::AmbiguousMatch {
            query: "x".into(),
            matches: vec![stage(), prod()],
        });
        assert!(ambiguous.contains("use the id") && ambiguous.contains("mdma-prod.local"));

        let choice = select_error_message(&SelectError::NeedsChoice(vec![stage(), prod()]));
        assert!(choice.starts_with("2 MDMA nodes") && choice.contains("mdma node select <name>"));

        let silent = select_error_message(&SelectError::Unreachable(Box::new(stage())));
        assert_eq!(silent, "Stage (192.168.0.238) did not answer.");

        let mismatch = select_error_message(&SelectError::IdentityMismatch {
            node: Box::new(stage()),
            reported: Some("mdma-prod".into()),
        });
        assert!(mismatch.contains("announced id 'mdma-stage'"));
        assert!(mismatch.contains("answers as 'mdma-prod'"));
        assert!(mismatch.contains("--node mdma-stage.local"));
        let silent_id = select_error_message(&SelectError::IdentityMismatch {
            node: Box::new(stage()),
            reported: None,
        });
        assert!(silent_id.contains("gives no id"));

        let failed = select_error_message(&SelectError::IdentifyFailed(Box::new(stage())));
        assert!(failed.contains("answered") && !failed.contains("did not answer"));

        let anon = select_error_message(&SelectError::NotRememberable(Box::new(prod())));
        assert!(anon.contains("--node mdma-prod.local"));

        assert!(select_error_message(&SelectError::NoNodes).contains("local network"));
    }

    // --- identify mapping ----------------------------------------------------

    fn identity(id: Option<&str>) -> NodeIdentity {
        NodeIdentity {
            id: id.map(String::from),
            name: None,
            version: "0.29.0".into(),
            proto: 1,
        }
    }

    #[test]
    fn identity_with_id_is_verified() {
        assert_eq!(
            identity_check(Ok(identity(Some("mdma-stage")))),
            Ok(IdentityCheck::Id("mdma-stage".into()))
        );
    }

    #[test]
    fn identity_without_id_or_old_gateway_is_unverifiable() {
        assert_eq!(
            identity_check(Ok(identity(None))),
            Ok(IdentityCheck::Unverifiable)
        );
        assert_eq!(
            identity_check(Err(IdentifyError::Unsupported("old".into()))),
            Ok(IdentityCheck::Unverifiable)
        );
    }

    #[test]
    fn gateway_error_response_is_failed_not_unverifiable() {
        let err = gateway_client::ClientError::Service("boom".into());
        assert_eq!(
            identity_check(Err(IdentifyError::Client(err))),
            Ok(IdentityCheck::Failed)
        );
    }

    #[test]
    fn transport_failure_is_unreachable() {
        let err = gateway_client::ClientError::Connection(
            nng_transport::ConnectionError::ConnectionFailed("refused".into()),
        );
        assert_eq!(
            identity_check(Err(IdentifyError::Client(err))),
            Err(Unreachable)
        );
    }
}
