//! Why a read from the facts failed (ADR-030 §2.3).
//!
//! The cases are told apart because the clients treat them differently: a missing
//! replica says how to get one, an unreachable node names the replica as the
//! alternative, and a refusal of the library is the library's own text. Nothing here
//! falls back to the other source.

use fact_replica::NodeId;
use library_ipc_client::{ClientError, ProtocolError};
use mdma_client::node::NodeError;

/// How a replica comes to exist, for the "no replica yet" messages.
pub(crate) const HOW_TO_GET_ONE: &str = "a background sync creates it, or run `mdma replica sync`";

/// What `plays` says under `--facts=node`, until the library has a plays request.
const PLAYS_FROM_NODE: &str = "`mdma plays` reads only the replica: the node has no plays \
                               request yet; drop --facts=node";

/// Why no replica was found for the node in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// A node was named (by `--node`, else `MDMA_NODE`) and no replica matches it.
    Named(String),
    /// On the node itself, and no sync has made its replica yet.
    LocalNotSynced,
    /// No node is named, remembered or running here.
    NoNode,
    /// The replica of this node is there as a name, and no sync has filled it.
    NotSynced(NodeId),
}

impl Missing {
    /// What was looked for: the `--node` flag; on the node itself, this node;
    /// else `MDMA_NODE`.
    pub(crate) fn named(explicit: Option<String>, local: bool, env: Option<&str>) -> Self {
        match explicit {
            Some(name) => Missing::Named(name),
            None if local => Missing::LocalNotSynced,
            None => env.map_or(Missing::NoNode, |name| Missing::Named(name.to_string())),
        }
    }
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Missing::Named(name) => write!(f, "no replica for '{name}' yet; {HOW_TO_GET_ONE}"),
            Missing::LocalNotSynced => write!(f, "no replica of this node yet; {HOW_TO_GET_ONE}"),
            Missing::NoNode => f.write_str(
                "no node to find a replica for; use --node NAME or run `mdma node select`",
            ),
            Missing::NotSynced(id) => write!(f, "no replica of {id} yet; {HOW_TO_GET_ONE}"),
        }
    }
}

/// The hint line after an error, when there is one.
fn hint_line(hint: &Option<String>) -> String {
    hint.as_ref()
        .map_or_else(String::new, |hint| format!("\n{hint}"))
}

/// Why a read from [`Facts`](crate::Facts) failed.
///
/// No variant has a `source`: the texts of the node's errors are final, so a caller
/// that prints the error chain prints each text once.
#[derive(Debug, thiserror::Error)]
pub enum FactsError {
    /// No replica was found for the node in use, or it is not synced yet; says how
    /// to get one.
    #[error("{0}")]
    NoReplica(Missing),
    /// The replica of `id` is there and cannot be read.
    #[error("the replica of {id} cannot be read: {cause}")]
    Unreadable { id: NodeId, cause: String },
    /// There is no cache directory to keep replicas in.
    #[error("no cache directory is available for the replica")]
    NoCacheDir,
    /// The node cannot be resolved or reached. `hint` names the replica as the
    /// alternative, with its age, when there is one (`--facts=node` only).
    #[error("{cause}{}", hint_line(.hint))]
    NodeUnreachable {
        cause: NodeError,
        hint: Option<String>,
    },
    /// The node could not be named at all (`MDMA_NODE` is invalid).
    #[error("{0}")]
    Node(NodeError),
    /// The request did not get an answer.
    #[error("{0}")]
    Transport(ClientError),
    /// The library refused the request (not found, ambiguous hash, ...).
    #[error("{0}")]
    Library(ProtocolError),
    /// The facts have no single track for the hash given (not there, or ambiguous),
    /// in words. For reads that answer in words of their own (`mdma plays`).
    #[error("{0}")]
    Refused(String),
    /// `plays` was asked of the node.
    #[error("{}", PLAYS_FROM_NODE)]
    PlaysFromNode,
}

impl FactsError {
    /// The replica of `id` could not be read, for `cause`.
    pub(crate) fn unreadable(id: &NodeId, cause: &dyn std::fmt::Display) -> Self {
        FactsError::Unreadable {
            id: id.clone(),
            cause: cause.to_string(),
        }
    }
}

impl From<ProtocolError> for FactsError {
    fn from(refusal: ProtocolError) -> Self {
        FactsError::Library(refusal)
    }
}

impl From<NodeError> for FactsError {
    fn from(error: NodeError) -> Self {
        FactsError::Node(error)
    }
}

impl From<ClientError> for FactsError {
    /// The library's own refusal stays a refusal; anything else is the transport's.
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Protocol(refusal) => FactsError::Library(refusal),
            other => FactsError::Transport(other),
        }
    }
}
