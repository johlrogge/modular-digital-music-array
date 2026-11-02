//! `mdma node select`: choosing a node to remember.
//!
//! Pure like [`crate::resolve`]: the caller supplies the browsed nodes and
//! the effects, and gets back the cache entry to save.
//!
//! As everywhere, the identify answer at an address is the only authority on
//! which node is there; the announced (mDNS) id is only a claim to check.

use crate::browse::DiscoveredNode;
use crate::cache::NodeCache;
use crate::resolve::{selectable, IdentifyFn, PickFn};
use crate::verify::{verify_candidate, VerifyError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    #[error("no MDMA nodes found on the network")]
    NoNodes,
    #[error("no node matches '{query}'")]
    NoMatch {
        query: String,
        nodes: Vec<DiscoveredNode>,
    },
    #[error("'{query}' matches several nodes")]
    AmbiguousMatch {
        query: String,
        matches: Vec<DiscoveredNode>,
    },
    #[error("{} nodes found and no terminal to choose from", .0.len())]
    NeedsChoice(Vec<DiscoveredNode>),
    #[error("node selection cancelled")]
    Cancelled,
    /// The node did not answer at its address.
    #[error("node did not answer")]
    Unreachable(Box<DiscoveredNode>),
    /// The node's gateway answered, but with an error.
    #[error("node's gateway failed to identify it")]
    IdentifyFailed(Box<DiscoveredNode>),
    /// The address answered as a different node than the one announced there,
    /// or could not say who it is.
    #[error("node announced one id but its address answers differently")]
    IdentityMismatch {
        node: Box<DiscoveredNode>,
        reported: Option<String>,
    },
    /// No id (unconfigured) or no address: nothing to remember it by.
    #[error("node cannot be remembered")]
    NotRememberable(Box<DiscoveredNode>),
}

/// The effects `select` needs.
pub struct SelectEffects<'a> {
    pub identify_at: Box<IdentifyFn<'a>>,
    pub pick: Box<PickFn<'a>>,
    pub is_tty: bool,
}

/// Nodes matching `query` by id, name or host (with or without `.local`),
/// ignoring case.
fn matching_nodes<'a>(nodes: &'a [DiscoveredNode], query: &str) -> Vec<&'a DiscoveredNode> {
    let query = query.trim_end_matches(".local");
    nodes
        .iter()
        .filter(|n| {
            n.id.as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(query))
                || n.name
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case(query))
                || n.host
                    .trim_end_matches(".local")
                    .eq_ignore_ascii_case(query)
        })
        .collect()
}

/// Choose a node from `nodes` (beacons are ignored) and return the cache
/// entry for it.
///
/// With a `query`, exactly one node must match its id, name or host. Without
/// one, a single node is taken, several are picked from on a terminal, and
/// otherwise [`SelectError::NeedsChoice`].
///
/// The chosen node is then verified with [`verify_candidate`]: its answer must
/// agree with what it announced (a different id, or no id from a node that
/// announced one, is [`SelectError::IdentityMismatch`]), and only a node that
/// announced no id takes the id it reports.
pub fn select(
    nodes: Vec<DiscoveredNode>,
    query: Option<&str>,
    effects: &mut SelectEffects<'_>,
) -> Result<NodeCache, SelectError> {
    let nodes = selectable(nodes);
    if nodes.is_empty() {
        return Err(SelectError::NoNodes);
    }

    let chosen = match query {
        Some(q) => match matching_nodes(&nodes, q).as_slice() {
            [one] => (*one).clone(),
            [] => {
                return Err(SelectError::NoMatch {
                    query: q.to_string(),
                    nodes,
                })
            }
            many => {
                return Err(SelectError::AmbiguousMatch {
                    query: q.to_string(),
                    matches: many.iter().map(|n| (*n).clone()).collect(),
                })
            }
        },
        None if nodes.len() == 1 => nodes[0].clone(),
        None if effects.is_tty => (effects.pick)(&nodes)
            .and_then(|i| nodes.get(i))
            .cloned()
            .ok_or(SelectError::Cancelled)?,
        None => return Err(SelectError::NeedsChoice(nodes)),
    };

    let (entry, _ip) = verify_candidate(&chosen, &mut *effects.identify_at)?;
    Ok(entry)
}

impl From<VerifyError> for SelectError {
    fn from(e: VerifyError) -> Self {
        match e {
            VerifyError::Unreachable(node) => SelectError::Unreachable(node),
            VerifyError::IdentifyFailed(node) => SelectError::IdentifyFailed(node),
            VerifyError::IdentityMismatch { node, reported } => {
                SelectError::IdentityMismatch { node, reported }
            }
            // Nothing to remember it by.
            VerifyError::NoAddress(node) | VerifyError::UnverifiableUnannounced { node, .. } => {
                SelectError::NotRememberable(node)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::NodeKind;
    use crate::resolve::{IdentityCheck, Unreachable};
    use std::net::Ipv4Addr;

    fn node(id: Option<&str>, name: Option<&str>, ip: u8) -> DiscoveredNode {
        DiscoveredNode {
            id: id.map(Into::into),
            name: name.map(Into::into),
            host: format!("{}.local", id.unwrap_or("unconfigured")),
            ipv4s: vec![Ipv4Addr::new(192, 168, 0, ip)],
            port: 5555,
            version: None,
            kind: NodeKind::Node,
        }
    }

    fn stage() -> DiscoveredNode {
        node(Some("mdma-stage"), Some("Stage"), 10)
    }

    fn prod() -> DiscoveredNode {
        node(Some("mdma-prod"), Some("Prod"), 20)
    }

    fn fx(
        identity: Result<IdentityCheck, Unreachable>,
        tty: bool,
        pick: Option<usize>,
    ) -> SelectEffects<'static> {
        SelectEffects {
            identify_at: Box::new(move |_, _| identity.clone()),
            pick: Box::new(move |_| pick),
            is_tty: tty,
        }
    }

    fn answers(id: &str) -> Result<IdentityCheck, Unreachable> {
        Ok(IdentityCheck::Id(id.into()))
    }

    #[test]
    fn matches_by_id_name_or_host_ignoring_case() {
        for q in [
            "mdma-stage",
            "MDMA-Stage",
            "stage",
            "Stage",
            "mdma-stage.local",
        ] {
            let entry = select(
                vec![stage(), prod()],
                Some(q),
                &mut fx(answers("mdma-stage"), false, None),
            )
            .unwrap();
            assert_eq!(entry.id, "mdma-stage", "query {q}");
        }
    }

    #[test]
    fn no_match_lists_the_nodes() {
        let err = select(
            vec![stage()],
            Some("nope"),
            &mut fx(answers("x"), false, None),
        )
        .unwrap_err();
        assert_eq!(
            err,
            SelectError::NoMatch {
                query: "nope".into(),
                nodes: vec![stage()]
            }
        );
    }

    #[test]
    fn query_matching_two_nodes_is_ambiguous() {
        let mut twin = prod();
        twin.name = Some("Stage".into());
        let err = select(
            vec![stage(), twin.clone()],
            Some("Stage"),
            &mut fx(answers("x"), false, None),
        )
        .unwrap_err();
        assert_eq!(
            err,
            SelectError::AmbiguousMatch {
                query: "Stage".into(),
                matches: vec![stage(), twin]
            }
        );
    }

    #[test]
    fn agreeing_id_is_remembered() {
        let entry = select(
            vec![stage()],
            None,
            &mut fx(answers("mdma-stage"), false, None),
        )
        .unwrap();
        assert_eq!(entry.id, "mdma-stage");
        assert_eq!(entry.name, "Stage");
        assert_eq!(entry.ip, Ipv4Addr::new(192, 168, 0, 10));
    }

    #[test]
    fn address_answering_as_another_node_is_a_mismatch() {
        // Never let the identify answer replace the announced id.
        let err = select(
            vec![stage()],
            None,
            &mut fx(answers("mdma-prod"), false, None),
        )
        .unwrap_err();
        assert_eq!(
            err,
            SelectError::IdentityMismatch {
                node: Box::new(stage()),
                reported: Some("mdma-prod".into())
            }
        );
    }

    #[test]
    fn announced_id_with_unverifiable_answer_is_a_mismatch() {
        let err = select(
            vec![stage()],
            None,
            &mut fx(Ok(IdentityCheck::Unverifiable), false, None),
        )
        .unwrap_err();
        assert_eq!(
            err,
            SelectError::IdentityMismatch {
                node: Box::new(stage()),
                reported: None
            }
        );
    }

    #[test]
    fn unconfigured_node_takes_the_id_it_reports() {
        let entry = select(
            vec![node(None, None, 30)],
            None,
            &mut fx(answers("mdma-new"), false, None),
        )
        .unwrap();
        assert_eq!(entry.id, "mdma-new");
    }

    #[test]
    fn unconfigured_unverifiable_node_cannot_be_remembered() {
        let anon = node(None, None, 30);
        let err = select(
            vec![anon.clone()],
            None,
            &mut fx(Ok(IdentityCheck::Unverifiable), false, None),
        )
        .unwrap_err();
        assert_eq!(err, SelectError::NotRememberable(Box::new(anon)));
    }

    #[test]
    fn node_without_address_cannot_be_remembered() {
        let mut no_ip = stage();
        no_ip.ipv4s.clear();
        let err = select(
            vec![no_ip.clone()],
            None,
            &mut fx(answers("x"), false, None),
        )
        .unwrap_err();
        assert_eq!(err, SelectError::NotRememberable(Box::new(no_ip)));
    }

    #[test]
    fn silent_node_is_unreachable() {
        let err = select(vec![stage()], None, &mut fx(Err(Unreachable), false, None)).unwrap_err();
        assert_eq!(err, SelectError::Unreachable(Box::new(stage())));
    }

    #[test]
    fn gateway_error_is_identify_failed_not_unreachable() {
        let err = select(
            vec![stage()],
            None,
            &mut fx(Ok(IdentityCheck::Failed), false, None),
        )
        .unwrap_err();
        assert_eq!(err, SelectError::IdentifyFailed(Box::new(stage())));
    }

    #[test]
    fn beacons_are_ignored() {
        let mut beacon = stage();
        beacon.kind = NodeKind::Beacon;
        let err = select(
            vec![beacon.clone()],
            None,
            &mut fx(answers("x"), true, Some(0)),
        )
        .unwrap_err();
        assert_eq!(err, SelectError::NoNodes);
        let err = select(
            vec![beacon],
            Some("mdma-stage"),
            &mut fx(answers("x"), false, None),
        )
        .unwrap_err();
        assert_eq!(err, SelectError::NoNodes);
    }

    #[test]
    fn no_nodes() {
        let err = select(vec![], None, &mut fx(answers("x"), true, Some(0))).unwrap_err();
        assert_eq!(err, SelectError::NoNodes);
    }

    #[test]
    fn several_nodes_without_query_pick_on_a_terminal() {
        let entry = select(
            vec![stage(), prod()],
            None,
            &mut fx(answers("mdma-prod"), true, Some(1)),
        )
        .unwrap();
        assert_eq!(entry.id, "mdma-prod");
    }

    #[test]
    fn cancelled_pick() {
        let err = select(
            vec![stage(), prod()],
            None,
            &mut fx(answers("x"), true, None),
        )
        .unwrap_err();
        assert_eq!(err, SelectError::Cancelled);
    }

    #[test]
    fn several_nodes_without_query_or_terminal_need_a_choice() {
        let err = select(
            vec![stage(), prod()],
            None,
            &mut fx(answers("x"), false, None),
        )
        .unwrap_err();
        assert_eq!(err, SelectError::NeedsChoice(vec![stage(), prod()]));
    }
}
