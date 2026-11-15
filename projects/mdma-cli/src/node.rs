//! The `mdma node` subcommands: list, select, forget and show.
//!
//! Resolving which node to talk to lives in `mdma_client::node`; this module
//! supplies the commands and the user-facing rendering of their output, which is
//! kept in pure functions.

use crate::ctx::Ctx;
use color_eyre::eyre::{bail, eyre};
use mdma_client::node::{
    cache_path, identify_at, ips, is_tty, load_cache, load_cache_quiet, pick_interactive,
    real_effects, request, resolve_error_message, select_error_message, NODE_ENV,
};
use node_discovery::{
    cache, resolve, DiscoveredNode, NodeCache, NodeKind, SelectEffects, Source, Target,
    BROWSE_TIMEOUT,
};

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

/// List MDMA nodes on the network (* marks the remembered node)
#[hicli::command]
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

/// The remembered node's name, id and host that start with `typed`. Local state only
/// (the node cache file, no network), and silent when there is none or it is unreadable.
pub(crate) fn complete_nodes(typed: &str) -> Vec<hicli::Candidate> {
    node_candidates(load_cache_quiet().as_ref(), typed)
}

fn node_candidates(cached: Option<&NodeCache>, typed: &str) -> Vec<hicli::Candidate> {
    let Some(node) = cached else {
        return Vec::new();
    };
    let mut out: Vec<hicli::Candidate> = Vec::new();
    for (value, what) in [(&node.name, "name"), (&node.id, "id"), (&node.host, "host")] {
        if !value.is_empty() && value.starts_with(typed) && !out.iter().any(|(v, _)| v == value) {
            out.push((value.clone(), Some(format!("remembered node ({what})"))));
        }
    }
    out
}

/// Node name or id
// Stays a String: any text is a valid query; no match is reported by the resolver.
#[derive(Debug, Clone, PartialEq, Eq, hicli::Arg)]
#[arg(positional = "NAME", complete = complete_nodes)]
pub(crate) struct NodeQueryArg(pub Option<String>);

/// Remember a node (by name or id; prompts when omitted)
#[hicli::command]
pub(crate) fn select(query: NodeQueryArg, _ctx: &Ctx) -> color_eyre::Result<()> {
    let NodeQueryArg(query) = query;
    let nodes = node_discovery::browse(BROWSE_TIMEOUT);
    let mut effects = SelectEffects {
        identify_at: Box::new(identify_at),
        pick: Box::new(pick_interactive),
        is_tty: is_tty(),
    };
    let entry = node_discovery::select(nodes, query.as_deref(), &mut effects)
        .map_err(|e| eyre!("{}", select_error_message(&e)))?;
    let Some(path) = cache_path() else {
        bail!("No config directory is available to remember the node in.");
    };
    cache::save(&path, &entry).map_err(|e| eyre!("Could not remember node: {e}"))?;
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

/// Forget the remembered node
#[hicli::command]
pub(crate) fn forget(_ctx: &Ctx) -> color_eyre::Result<()> {
    let Some(path) = cache_path() else {
        println!("No config directory; nothing to forget.");
        return Ok(());
    };
    let previous = load_cache();
    cache::forget(&path).map_err(|e| eyre!("Could not forget node: {e}"))?;
    match previous {
        Some(c) => println!("Forgot {} ({})", c.name, c.id),
        None => println!("No remembered node."),
    }
    Ok(())
}

/// Show which node commands would use, and why
///
/// Resolves the node like any command would, but saves nothing.
#[hicli::command]
pub(crate) fn show(ctx: &Ctx) -> color_eyre::Result<()> {
    let resolution = resolve(
        request(ctx.node_flag(), ctx.local_ipc())?,
        &mut real_effects(),
    )
    .map_err(|e| eyre!("{}", resolve_error_message(&e)))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run_arg;
    use mdma_client::test_support::{discovered_prod as prod, discovered_stage as stage};
    use pretty_assertions::assert_eq;

    #[test]
    fn node_query_arg_parser_is_an_optional_positional() {
        assert_eq!(run_arg::<NodeQueryArg>(&[]), Ok(NodeQueryArg(None)));
        assert_eq!(
            run_arg::<NodeQueryArg>(&["johlyroger"]),
            Ok(NodeQueryArg(Some("johlyroger".to_string())))
        );
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
}
