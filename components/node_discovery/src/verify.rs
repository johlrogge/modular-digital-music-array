//! Checking a browsed node against what its addresses say.
//!
//! The identify answer at an address is the only authority on which node is
//! there; the announced (mDNS) id is only a claim to check. Both `select` and
//! `resolve` use [`verify_candidate`], so the rule is enforced in one place.

use std::net::Ipv4Addr;

use crate::browse::DiscoveredNode;
use crate::cache::NodeCache;
use crate::resolve::{cache_entry, IdentifyFn, IdentityCheck};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The node has no IPv4 address to ask.
    #[error("node has no address")]
    NoAddress(Box<DiscoveredNode>),
    /// No address of the node answered.
    #[error("node did not answer")]
    Unreachable(Box<DiscoveredNode>),
    /// The node's gateway answered, but with an error.
    #[error("node's gateway failed to identify it")]
    IdentifyFailed(Box<DiscoveredNode>),
    /// No address answered with the announced id: one answered as a different
    /// node (`reported` is its id), or could not say who it is (`None`).
    #[error("node announced one id but its address answers differently")]
    IdentityMismatch {
        node: Box<DiscoveredNode>,
        reported: Option<String>,
    },
    /// A node that announced no id, at an address that cannot say who it is.
    /// Usable for one run at `ip`, but there is nothing to remember it by.
    #[error("node is unconfigured and cannot be verified")]
    UnverifiableUnannounced {
        node: Box<DiscoveredNode>,
        ip: Ipv4Addr,
    },
}

/// Ask each IPv4 address of `node` who it is, and return the cache entry for
/// the first that agrees, together with that confirmed address.
///
/// | announced id | identify answer | result |
/// |---|---|---|
/// | `Some(a)` | `Id(a)` | confirmed |
/// | `Some(a)` | `Id(r)`, r != a | mismatch |
/// | `Some(_)` | unverifiable | mismatch, reported `None` |
/// | `None` | `Id(r)` | confirmed, takes `r` |
/// | `None` | unverifiable | [`VerifyError::UnverifiableUnannounced`] |
///
/// When nothing confirms, a mismatch is reported in preference to a gateway
/// failure, and that to silence.
pub fn verify_candidate(
    node: &DiscoveredNode,
    identify_at: &mut IdentifyFn<'_>,
) -> Result<(NodeCache, Ipv4Addr), VerifyError> {
    if node.ipv4s.is_empty() {
        return Err(VerifyError::NoAddress(Box::new(node.clone())));
    }
    let mut mismatch: Option<Option<String>> = None;
    let mut failed = false;
    let mut anonymous_ip: Option<Ipv4Addr> = None;

    for &ip in &node.ipv4s {
        let id = match (identify_at)(ip, node.port) {
            Err(_) => continue,
            Ok(IdentityCheck::Failed) => {
                failed = true;
                continue;
            }
            Ok(IdentityCheck::Unverifiable) => match node.id {
                Some(_) => {
                    mismatch.get_or_insert(None);
                    continue;
                }
                None => {
                    anonymous_ip.get_or_insert(ip);
                    continue;
                }
            },
            Ok(IdentityCheck::Id(reported)) => match &node.id {
                Some(announced) if *announced != reported => {
                    mismatch.get_or_insert(Some(reported));
                    continue;
                }
                _ => reported,
            },
        };
        let mut confirmed = node.clone();
        confirmed.id = Some(id);
        confirmed.ipv4s = vec![ip];
        let entry = cache_entry(&confirmed).expect("confirmed node has an id and an address");
        return Ok((entry, ip));
    }

    let node = Box::new(node.clone());
    Err(if let Some(reported) = mismatch {
        VerifyError::IdentityMismatch { node, reported }
    } else if failed {
        VerifyError::IdentifyFailed(node)
    } else if let Some(ip) = anonymous_ip {
        VerifyError::UnverifiableUnannounced { node, ip }
    } else {
        VerifyError::Unreachable(node)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::NodeKind;
    use crate::resolve::Unreachable;

    const A: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
    const B: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);

    fn node(id: Option<&str>, ips: &[Ipv4Addr]) -> DiscoveredNode {
        DiscoveredNode {
            id: id.map(Into::into),
            name: Some("Stage".into()),
            host: "stage.local".into(),
            ipv4s: ips.to_vec(),
            port: 5555,
            version: None,
            kind: NodeKind::Node,
        }
    }

    fn id(s: &str) -> Result<IdentityCheck, Unreachable> {
        Ok(IdentityCheck::Id(s.into()))
    }

    fn verify(
        n: &DiscoveredNode,
        answers: Vec<(Ipv4Addr, Result<IdentityCheck, Unreachable>)>,
    ) -> Result<(NodeCache, Ipv4Addr), VerifyError> {
        let mut f = move |ip: Ipv4Addr, _port: u16| {
            answers
                .iter()
                .find(|(i, _)| *i == ip)
                .map(|(_, r)| r.clone())
                .unwrap_or(Err(Unreachable))
        };
        verify_candidate(n, &mut f)
    }

    #[test]
    fn agreeing_answer_is_confirmed_at_that_address() {
        let (entry, ip) = verify(&node(Some("s"), &[A]), vec![(A, id("s"))]).unwrap();
        assert_eq!((entry.id.as_str(), entry.ip, ip), ("s", A, A));
    }

    #[test]
    fn the_confirmed_address_is_used_not_the_first() {
        let (entry, ip) = verify(
            &node(Some("s"), &[A, B]),
            vec![(A, id("other")), (B, id("s"))],
        )
        .unwrap();
        assert_eq!((entry.ip, ip), (B, B));
    }

    #[test]
    fn different_id_is_a_mismatch_never_replaced() {
        let n = node(Some("s"), &[A]);
        assert_eq!(
            verify(&n, vec![(A, id("p"))]).unwrap_err(),
            VerifyError::IdentityMismatch {
                node: Box::new(n),
                reported: Some("p".into())
            }
        );
    }

    #[test]
    fn announced_id_with_unverifiable_answer_is_a_mismatch() {
        let n = node(Some("s"), &[A]);
        assert_eq!(
            verify(&n, vec![(A, Ok(IdentityCheck::Unverifiable))]).unwrap_err(),
            VerifyError::IdentityMismatch {
                node: Box::new(n),
                reported: None
            }
        );
    }

    #[test]
    fn unannounced_node_takes_the_reported_id() {
        let (entry, _) = verify(&node(None, &[A]), vec![(A, id("new"))]).unwrap();
        assert_eq!(entry.id, "new");
    }

    #[test]
    fn unannounced_unverifiable_node_is_usable_but_not_confirmed() {
        let n = node(None, &[A, B]);
        assert_eq!(
            verify(&n, vec![(B, Ok(IdentityCheck::Unverifiable))]).unwrap_err(),
            VerifyError::UnverifiableUnannounced {
                node: Box::new(n),
                ip: B
            }
        );
    }

    #[test]
    fn failures_are_reported_by_priority() {
        let n = node(Some("s"), &[A, B]);
        // mismatch beats gateway failure
        assert!(matches!(
            verify(&n, vec![(A, Ok(IdentityCheck::Failed)), (B, id("p"))]),
            Err(VerifyError::IdentityMismatch { .. })
        ));
        // gateway failure beats silence
        assert!(matches!(
            verify(&n, vec![(A, Ok(IdentityCheck::Failed))]),
            Err(VerifyError::IdentifyFailed(_))
        ));
        assert!(matches!(
            verify(&n, vec![]),
            Err(VerifyError::Unreachable(_))
        ));
    }

    #[test]
    fn no_address() {
        assert!(matches!(
            verify(&node(Some("s"), &[]), vec![]),
            Err(VerifyError::NoAddress(_))
        ));
    }
}
