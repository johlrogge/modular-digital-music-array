//! `client-facts`, implemented by the replica (ADR-022 §3, ADR-021, ADR-030 §8).
//!
//! The client-side copy of a node's fact log: which replica a command reads, how it
//! is synced from the node (identify, ACID reads, probes), the completion snapshot
//! built at each sync, and the throttle of the background sync. Moved unchanged out
//! of `mdma-cli`; the client keeps its commands, its detached spawn and its
//! completion wiring.
//!
//! The pure fold is `library_projection`'s. This component depends on `mdma-client`
//! for the `Node`; `mdma-client` does not depend on it.

mod replica;
mod snapshot;
mod throttle;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

// The replica store's types that clients name when they ask about a replica.
pub use fact_replica::{NodeId, Replica, UnixSecs};

pub use replica::{
    ago, fold_current, replica_from_environment, replica_notes, replicas_dir, reset, status, sync,
    Probe, Probed, ReplicaFold, PROBE_TTL_SECS,
};
pub use snapshot::CompletionSnapshot;
pub use throttle::{request_sync, Urgency};
