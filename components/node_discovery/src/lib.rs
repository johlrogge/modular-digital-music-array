//! Finding the MDMA node a client should talk to.
//!
//! - [`cache`]: the remembered node (`~/.config/mdma/node.json`).
//! - [`browse`]: DNS-SD discovery of nodes on the local network.
//! - [`resolve`]: the pure resolution order, with effects injected.
//! - [`select`]: the pure choice of a node to remember.

pub mod browse;
pub mod cache;
pub mod resolve;
pub mod select;
pub mod verify;

pub use browse::{
    browse, browse_for, browse_until, DiscoveredNode, NodeKind, BEACON_SERVICE_TYPE,
    NODE_SERVICE_TYPE,
};
pub use cache::{CacheError, NodeCache};
pub use resolve::{
    cache_entry, resolve, BrowseFn, Effects, IdentifyFn, IdentityCheck, LoadCacheFn, PickFn,
    RemoteTarget, Request, Resolution, ResolveError, ResolveNameFn, Source, Target, Unreachable,
    BROWSE_TIMEOUT, DEFAULT_PORT,
};
pub use select::{select, SelectEffects, SelectError};
pub use verify::{verify_candidate, VerifyError};
