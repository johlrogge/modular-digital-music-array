//! Node discovery wiring: resolving which node to talk to.
//!
//! The decisions live in `node_discovery`; this module supplies the real
//! effects (gateway identify, DNS, mDNS browse, TTY prompt) and the user-facing
//! rendering, which is kept in pure functions.

use std::io::{BufRead, IsTerminal, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Answer, NodeError};
use crate::error::connect_failure;
use crate::ClientConfig;
use gateway_client::{GatewayClient, IdentifyError, NodeIdentity};
use node_discovery::{
    cache, resolve, DiscoveredNode, Effects, IdentityCheck, NodeCache, NodeSpec, RemoteTarget,
    Request, ResolveError, SelectError, Source, Target, Unreachable, VerifyError,
};

/// How long to wait for a gateway to answer an identify request.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(1);

/// Environment variable naming the node (split from `--node` so the two can be
/// told apart).
pub const NODE_ENV: &str = "MDMA_NODE";

/// Whether resolving the node may print the note about an implicit choice
/// ("Using <node>, discovered"). Commands that print nothing on success
/// (`replica sync --quiet`) ask for `Silent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notes {
    Print,
    Silent,
}

/// The note to print for `target` under `notes`, if any.
fn note_to_print(target: &Target, notes: Notes) -> Option<String> {
    match (target, notes) {
        (Target::Remote(remote), Notes::Print) => source_note(remote),
        _ => None,
    }
}

/// True when the CLI should talk to the local services over IPC instead of a
/// gateway: sockets were given explicitly, or the library socket exists here.
pub(super) fn local_ipc_wanted(
    sockets_given_explicitly: bool,
    library_socket_exists: bool,
) -> bool {
    sockets_given_explicitly || library_socket_exists
}

/// Gateway client settings for a resolved target. Prefers the IP so NNG
/// connects do not depend on mDNS. No target, or local IPC, means direct IPC.
pub(super) fn client_config(target: Option<&Target>) -> ClientConfig {
    match target {
        Some(Target::Remote(t)) => ClientConfig {
            node: Some(address_of(t)),
            gateway_port: t.port,
            ..Default::default()
        },
        Some(Target::LocalIpc) | None => ClientConfig::default(),
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
pub(super) fn remote_host(target: Option<&Target>) -> Option<(String, Option<Ipv4Addr>)> {
    match target {
        Some(Target::Remote(t)) => Some((t.host.clone(), t.ip)),
        Some(Target::LocalIpc) | None => None,
    }
}

/// One-line note for sources the user did not state explicitly.
fn source_note(target: &RemoteTarget) -> Option<String> {
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

fn display_name(node: &DiscoveredNode) -> &str {
    node.name
        .as_deref()
        .or(node.id.as_deref())
        .unwrap_or("(unconfigured)")
}

pub fn ips(node: &DiscoveredNode) -> String {
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
fn render_pick_list(nodes: &[DiscoveredNode]) -> String {
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
enum PickInput {
    /// Zero-based index of the chosen node.
    Choice(usize),
    Cancel,
    Invalid,
}

/// Parse a 1-based choice out of `len` nodes. Empty input or `q` cancels.
fn parse_pick(input: &str, len: usize) -> PickInput {
    let input = input.trim();
    if input.is_empty() || input.eq_ignore_ascii_case("q") {
        return PickInput::Cancel;
    }
    match input.parse::<usize>() {
        Ok(n) if (1..=len).contains(&n) => PickInput::Choice(n - 1),
        _ => PickInput::Invalid,
    }
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
fn verify_error_message(err: &VerifyError) -> String {
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
    format!("{}.", did_not_answer(display_name(node), &ips(node)))
}

/// `<who> (<ips>) did not answer`, the start of every text about a silent node.
fn did_not_answer(who: &str, ips: &str) -> String {
    format!("{who} ({ips}) did not answer")
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
fn identity_check(
    answer: Result<NodeIdentity, IdentifyError>,
) -> Result<IdentityCheck, Unreachable> {
    match Answer::from_identify(answer) {
        Ok(Answer::Identity(NodeIdentity { id: Some(id), .. })) => Ok(IdentityCheck::Id(id)),
        Ok(Answer::Identity(_) | Answer::Unsupported(_)) => Ok(IdentityCheck::Unverifiable),
        Ok(Answer::Failed(_)) => Ok(IdentityCheck::Failed),
        Err(_) => Err(Unreachable),
    }
}

/// Ask the gateway at `address` who it is. Connecting does not wait for the
/// connection: the request is bounded by [`IDENTIFY_TIMEOUT`] to send and again
/// to receive, so the call takes at most twice that, plus name resolution for a
/// host name.
fn identify_address(address: &str) -> Result<NodeIdentity, IdentifyError> {
    GatewayClient::connect_with_timeout(address, IDENTIFY_TIMEOUT)
        .map_err(IdentifyError::Client)?
        .identify()
}

/// Ask the gateway at `ip:port` who it is, bounded as [`identify_address`].
fn identify_raw(ip: Ipv4Addr, port: u16) -> Result<NodeIdentity, IdentifyError> {
    identify_address(&format!("tcp://{ip}:{port}"))
}

/// Ask the gateway at `ip:port` who it is, as the resolver wants the answer.
pub fn identify_at(ip: Ipv4Addr, port: u16) -> Result<IdentityCheck, Unreachable> {
    identity_check(identify_raw(ip, port))
}

/// Ask the gateway of `target` who it is: the reachability check. Bounded as
/// [`identify_address`]. The error is the text for the user.
pub(super) fn probe_node(target: &RemoteTarget) -> Result<Answer, String> {
    let address = format!("tcp://{}:{}", address_of(target), target.port);
    probe_outcome(identify_address(&address), target)
}

/// What the answer to "who are you" says about the node at `target`. Any answer
/// means the node is there, even "I cannot say" from an old gateway or an error
/// from one that is unconfigured. Silence is `did not answer in time`: the dial
/// is asynchronous, so a refused or unreachable node shows up as a timeout, never
/// as "refused". A dial that fails outright (a bad address) and any other transport
/// failure keep their wording.
fn probe_outcome(
    answer: Result<NodeIdentity, IdentifyError>,
    target: &RemoteTarget,
) -> Result<Answer, String> {
    use gateway_client::ClientError;
    Answer::from_identify(answer).map_err(|e| match e {
        ClientError::Nng(nng::Error::TimedOut) => {
            let ips = target
                .ip
                .map_or_else(|| "-".to_string(), |ip| ip.to_string());
            format!("{} in time", did_not_answer(&target.host, &ips))
        }
        ClientError::Connection(e) => connect_failure("node", &e),
        other => format!(
            "Failed to connect to node at tcp://{}:{}: {other}",
            address_of(target),
            target.port
        ),
    })
}

fn resolve_name_fresh(host: &str) -> Option<Ipv4Addr> {
    nng_transport::resolve_hostname_to_ipv4_fresh(host)
        .ok()?
        .parse()
        .ok()
}

/// Ask on stderr/stdin which node to use. `None` on cancel or end of input.
pub fn pick_interactive(nodes: &[DiscoveredNode]) -> Option<usize> {
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

pub fn is_tty() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn real_effects() -> Effects<'static> {
    Effects {
        identify_at: Box::new(identify_at),
        resolve_name_fresh: Box::new(resolve_name_fresh),
        browse: Box::new(node_discovery::browse_for),
        pick: Box::new(pick_interactive),
        load_cache: Box::new(load_cache),
        is_tty: is_tty(),
    }
}

/// How the error for a bad `MDMA_NODE` starts.
const INVALID_ENV_NODE: &str = "invalid MDMA_NODE";

/// `MDMA_NODE`. Empty counts as unset, so `MDMA_NODE= mdma ...` overrides a
/// value exported by the environment (devenv sets one).
fn parse_env_node(value: Option<&str>) -> Result<Option<NodeSpec>, String> {
    match value.filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|e| format!("{INVALID_ENV_NODE}: {e}")),
    }
}

pub fn env_node() -> Result<Option<NodeSpec>, String> {
    parse_env_node(std::env::var(NODE_ENV).ok().as_deref())
}

pub fn cache_path() -> Option<PathBuf> {
    cache::default_path()
}

pub fn load_cache() -> Option<NodeCache> {
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

/// The remembered node, read silently: `None` when there is none or it is unreadable.
/// For completion and the background refresh, which must not print.
pub fn load_cache_quiet() -> Option<NodeCache> {
    cache::load(&cache_path()?).ok().flatten()
}

/// The resolution request; a bad `MDMA_NODE` is an error.
pub fn request(explicit: Option<&NodeSpec>, local_ipc: bool) -> Result<Request, NodeError> {
    let env = env_node().map_err(NodeError::InvalidEnv)?;
    Ok(Request {
        explicit: explicit.cloned(),
        env,
        local_ipc,
    })
}

/// Whether resolving may ask the user to pick a node. Only the first resolution
/// of a process may: a retry after a failure runs in the background of whatever
/// the client is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interactive {
    Yes,
    No,
}

/// Resolve the node once at startup, remembering it when the resolver says
/// so. Prints a note for implicit choices; fails with advice when no node
/// can be resolved.
///
/// A gateway that answered "who are you" while the resolver verified the node
/// is a node that has been reached: its answer comes back with the target, so
/// the reachability check does not ask again.
pub(super) fn resolve_startup(
    explicit: Option<&NodeSpec>,
    local_ipc: bool,
    notes: Notes,
    interactive: Interactive,
) -> Result<Resolved, NodeError> {
    let request = request(explicit, local_ipc)?;
    // The effects outlive this scope's borrows, so the answers go through a shared cell.
    let heard: Arc<Mutex<Option<(Ipv4Addr, u16, NodeIdentity)>>> = Arc::new(Mutex::new(None));
    let mut effects = real_effects();
    if interactive == Interactive::No {
        effects.is_tty = false;
    }
    let sink = Arc::clone(&heard);
    effects.identify_at = Box::new(move |ip, port| {
        let answer = identify_raw(ip, port);
        if let Ok(identity) = &answer {
            *sink.lock().unwrap_or_else(|p| p.into_inner()) = Some((ip, port, identity.clone()));
        }
        identity_check(answer)
    });
    match resolve(request, &mut effects) {
        Ok(resolution) => {
            if let Some(update) = &resolution.cache_update {
                save_cache(update);
            }
            if let Some(note) = note_to_print(&resolution.target, notes) {
                eprintln!("{note}");
            }
            let answer = match (
                &resolution.target,
                heard.lock().ok().and_then(|h| h.clone()),
            ) {
                (Target::Remote(remote), Some((ip, port, identity)))
                    if remote.ip == Some(ip) && remote.port == port =>
                {
                    Some(Answer::Identity(identity))
                }
                _ => None,
            };
            Ok(Resolved {
                target: resolution.target,
                answer,
            })
        }
        Err(e) => Err(NodeError::Resolve(e)),
    }
}

/// A resolved node, and what its gateway said if the resolver asked it.
#[derive(Debug)]
pub struct Resolved {
    pub target: Target,
    pub answer: Option<Answer>,
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
                "invalid MDMA_NODE: invalid port 'nope' in node 'mdma.local:nope', \
                 expected 1-65535"
                    .to_string()
            )
        );
    }

    #[test]
    fn resolving_with_a_bad_env_node_fails_naming_the_variable() {
        // One test, because it touches a process-wide environment variable (which
        // devenv sets, so it is put back).
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _env = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = std::env::var_os(NODE_ENV);
        std::env::set_var(NODE_ENV, "mdma.local:nope");
        let result = resolve_startup(None, false, Notes::Print, Interactive::Yes);
        match original {
            Some(value) => std::env::set_var(NODE_ENV, value),
            None => std::env::remove_var(NODE_ENV),
        }
        assert_eq!(
            result.unwrap_err().to_string(),
            "invalid MDMA_NODE: invalid port 'nope' in node 'mdma.local:nope', expected 1-65535"
        );
    }

    use crate::test_support::{discovered_prod as prod, discovered_stage as stage};

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
    fn a_silent_resolution_prints_no_note_even_for_an_implicit_choice() {
        let discovered = target(Source::Discovered);
        assert!(note_to_print(&discovered, Notes::Print).is_some());
        assert_eq!(note_to_print(&discovered, Notes::Silent), None);
        assert_eq!(note_to_print(&Target::LocalIpc, Notes::Print), None);
    }

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
    fn an_identity_is_an_answer() {
        assert_eq!(
            probe_outcome(
                Ok(identity(Some("mdma-stage"))),
                &remote_target(Source::Cache)
            ),
            Ok(Answer::Identity(identity(Some("mdma-stage"))))
        );
    }

    #[test]
    fn an_old_gateway_is_still_an_answer() {
        assert_eq!(
            probe_outcome(
                Err(IdentifyError::Unsupported("old".into())),
                &remote_target(Source::Cache)
            ),
            Ok(Answer::Unsupported("old".into()))
        );
    }

    #[test]
    fn a_gateway_error_is_still_an_answer() {
        let err = gateway_client::ClientError::Service("boom".into());
        assert_eq!(
            probe_outcome(
                Err(IdentifyError::Client(err)),
                &remote_target(Source::Cache)
            ),
            Ok(Answer::Failed("boom".into()))
        );
    }

    #[test]
    fn silence_says_who_did_not_answer_in_time() {
        let err = gateway_client::ClientError::Nng(nng::Error::TimedOut);
        assert_eq!(
            probe_outcome(
                Err(IdentifyError::Client(err)),
                &remote_target(Source::Cache)
            ),
            Err("mdma-stage.local (192.168.0.238) did not answer in time".to_string())
        );
    }

    #[test]
    fn a_dial_that_fails_outright_keeps_its_wording() {
        let err = gateway_client::ClientError::Connection(nng_transport::ConnectionError::Dial {
            address: "tcp://192.168.0.238:5555".into(),
            cause: nng::Error::ConnectionRefused,
        });
        assert_eq!(
            probe_outcome(
                Err(IdentifyError::Client(err)),
                &remote_target(Source::Cache)
            ),
            Err(
                "Failed to connect to node at tcp://192.168.0.238:5555: Connection refused"
                    .to_string()
            )
        );
    }

    #[test]
    fn transport_failure_is_unreachable() {
        let err = gateway_client::ClientError::Connection(nng_transport::ConnectionError::Dial {
            address: "tcp://10.0.0.2:5555".into(),
            cause: nng::Error::ConnectionRefused,
        });
        assert_eq!(
            identity_check(Err(IdentifyError::Client(err))),
            Err(Unreachable)
        );
    }
}
