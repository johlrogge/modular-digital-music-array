//! Client-side replica of a node's raw fact-log lines (ADR-021).
//!
//! One replica per node lives in `<nodes dir>/<node-id>/`:
//!
//! - `facts.jsonl`: the log lines, as served.
//! - `meta.json`: the commit point (epoch, line count, digest of the last line,
//!   timestamps, host names).
//! - `sync.lock`: the flock file; one sync (or reset) per node at a time. It is
//!   never deleted, so a lock holder's file is never replaced under it.
//!
//! A sync appends lines to `facts.jsonl` and fsyncs, then rewrites `meta.json`
//! with tmp+rename. Lines past the committed count are an uncommitted torn tail
//! and are dropped on open. The node is read through an injected closure, so the
//! store has no network dependency and tests run against a fake log.
//!
//! Invariant: `meta.json` may go backwards (a lost `last_attempt` or host, a
//! meta older than the data file, a crash between data and meta). Integrity never
//! rests on it: the data file is checked against the meta on every open
//! (`repair`), and the epoch and pinned reads catch a log that changed. Every
//! writer takes the lock and skips the write when it is busy, so writers do not
//! interleave; the invariant covers crashes and anything that slips past.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use acid_protocol::{cursor_from_offset, line_digest, LinePin, PinnedRead};
use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// Lines asked for per page.
pub const PAGE_LINES: usize = 5_000;
/// Minimum time between sync attempts.
pub const BASE_INTERVAL: Duration = Duration::from_secs(30);
/// Longest time between attempts after repeated failures.
pub const MAX_INTERVAL: Duration = Duration::from_secs(300);

const FACTS_FILE: &str = "facts.jsonl";
const META_FILE: &str = "meta.json";
const LOCK_FILE: &str = "sync.lock";

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Why a string is not a usable node id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid node id {0:?}: use letters, digits, '-', '_' and '.', not starting with '.'")]
pub struct InvalidNodeId(String);

/// The id a node reports in `Identify`. It names the replica directory, so it is
/// restricted to characters that cannot escape it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidNodeId> {
        let id = id.into();
        let ok = !id.is_empty()
            && !id.starts_with('.')
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if ok {
            Ok(Self(id))
        } else {
            Err(InvalidNodeId(id))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for NodeId {
    type Err = InvalidNodeId;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identity of the node's fact log; changes when the log is replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(String);

impl Epoch {
    pub fn new(epoch: impl Into<String>) -> Self {
        Self(epoch.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lowercase hex SHA-256 of a log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    pub fn of_line(line: &str) -> Self {
        Self(line_digest(line))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A number of log lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LineCount(usize);

impl LineCount {
    pub fn new(lines: usize) -> Self {
        Self(lines)
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for LineCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Seconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixSecs(u64);

impl UnixSecs {
    pub fn new(secs: u64) -> Self {
        Self(secs)
    }

    pub fn now() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self(secs)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// Seconds from `earlier` to `self`; `None` when the clock went backwards.
    pub fn since(self, earlier: UnixSecs) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }
}

/// A host name a node was reached by (case-insensitive, stored lowercase).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Host(String);

impl Host {
    pub fn new(host: &str) -> Self {
        Self(host.trim().to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Meta and throttle
// ---------------------------------------------------------------------------

/// What the data file holds, as committed. Lines without an epoch and a digest
/// of the last one cannot be represented.
///
/// On disk the fields are flattened into `meta.json` (`epoch`, `lines`,
/// `last_line_sha256`). Format compatibility: a meta.json written by the earlier
/// flat layout still reads when it was committed (`lines > 0` with a digest). One
/// that was not (zero lines, a null digest, or anything unreadable) reads as
/// empty or as no meta at all, and the replica is wiped and refetched, which is
/// the same recovery as for damage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Committed {
    /// At least one line, from the log `epoch`; `last_line_sha256` is the digest
    /// of line `lines - 1`.
    Lines {
        epoch: Epoch,
        lines: NonZeroUsize,
        last_line_sha256: Digest,
    },
    /// No lines. `epoch` is known once the node has answered, even with an empty log.
    Empty {
        #[serde(default)]
        epoch: Option<Epoch>,
    },
}

/// The replica's commit point and bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(flatten)]
    pub committed: Committed,
    pub synced_at: Option<UnixSecs>,
    pub last_attempt: Option<UnixSecs>,
    /// Attempts that failed since the last success; drives the backoff.
    #[serde(default)]
    pub failures: u32,
    #[serde(default)]
    pub hosts: Vec<Host>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            committed: Committed::Empty { epoch: None },
            synced_at: None,
            last_attempt: None,
            failures: 0,
            hosts: Vec::new(),
        }
    }
}

impl Meta {
    /// Identity of the log the lines came from; `None` before the first answer.
    pub fn epoch(&self) -> Option<&Epoch> {
        match &self.committed {
            Committed::Lines { epoch, .. } => Some(epoch),
            Committed::Empty { epoch } => epoch.as_ref(),
        }
    }

    /// Committed lines in `facts.jsonl`.
    pub fn lines(&self) -> LineCount {
        match &self.committed {
            Committed::Lines { lines, .. } => LineCount::new(lines.get()),
            Committed::Empty { .. } => LineCount::new(0),
        }
    }

    /// The pin for the next read: the last committed line.
    fn pin(&self) -> Option<LinePin> {
        match &self.committed {
            Committed::Lines {
                lines,
                last_line_sha256,
                ..
            } => Some(LinePin {
                line: lines.get() - 1,
                sha256: last_line_sha256.as_str().to_string(),
            }),
            Committed::Empty { .. } => None,
        }
    }

    /// What survives a wipe: bookkeeping, not data.
    fn emptied(&self) -> Meta {
        Meta {
            last_attempt: self.last_attempt,
            failures: self.failures,
            hosts: self.hosts.clone(),
            ..Meta::default()
        }
    }

    fn add_host(&mut self, host: &Host) {
        if !self.hosts.contains(host) {
            self.hosts.push(host.clone());
        }
    }
}

/// Time to wait after `failures` failed attempts in a row: 30 s, doubling, up
/// to 5 min.
pub fn interval_after(failures: u32) -> Duration {
    let factor = 1u64 << failures.min(16);
    BASE_INTERVAL
        .saturating_mul(u32::try_from(factor).unwrap_or(u32::MAX))
        .min(MAX_INTERVAL)
}

/// Whether enough time has passed since the last attempt to try again. Without
/// a replica or an attempt, always; a clock that went backwards counts as elapsed.
pub fn should_attempt(now: UnixSecs, meta: Option<&Meta>) -> bool {
    let Some(meta) = meta else { return true };
    let Some(last) = meta.last_attempt else {
        return true;
    };
    match now.since(last) {
        Some(elapsed) => elapsed >= interval_after(meta.failures).as_secs(),
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Errors and effects
// ---------------------------------------------------------------------------

/// A failed read from the node, as text (the transport's own error is not this
/// component's business).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ReadFailure(pub String);

#[derive(Debug, thiserror::Error)]
pub enum ReplicaError {
    #[error("replica I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("node too old for the replica")]
    NodeTooOld,
    #[error("reading the node's fact log failed: {0}")]
    Read(#[from] ReadFailure),
    #[error("the node's fact log kept changing while syncing; try again later")]
    Unstable,
    #[error("a sync of {0} is running; try again")]
    Busy(NodeId),
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> ReplicaError + '_ {
    move |source| ReplicaError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The read effect: one page of the node's log after `cursor`, with the last
/// line we hold pinned.
pub type ReadPage<'a> =
    dyn FnMut(Option<String>, usize, Option<LinePin>) -> Result<PinnedRead, ReadFailure> + 'a;

/// What a completed sync did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    /// Lines appended to the replica.
    pub added: LineCount,
    /// Lines in the replica now.
    pub total: LineCount,
    /// The replica was thrown away and refetched (divergence or new epoch).
    pub reset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// Another sync of this node holds the lock; nothing was done.
    Busy,
    Synced(SyncReport),
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// `dirs::cache_dir()/mdma/nodes`, where every node's replica lives.
pub fn default_nodes_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("mdma").join("nodes"))
}

/// Held for the duration of a sync or reset; dropping it releases the lock.
/// Anything else written into the replica directory (the CLI's probe file) is
/// written while holding one.
#[derive(Debug)]
pub struct SyncLock {
    #[allow(dead_code)]
    file: File,
    /// The replica directory it locks, so a lock cannot be used for another one.
    dir: PathBuf,
}

/// One node's replica directory.
#[derive(Debug, Clone)]
pub struct Replica {
    node: NodeId,
    dir: PathBuf,
}

impl Replica {
    /// The replica of `node` under `nodes_dir`. Touches nothing.
    pub fn new(nodes_dir: &Path, node: &NodeId) -> Self {
        Self {
            node: node.clone(),
            dir: nodes_dir.join(node.as_str()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn facts_path(&self) -> PathBuf {
        self.dir.join(FACTS_FILE)
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join(META_FILE)
    }

    /// The committed meta. `None` when there is none or it cannot be parsed
    /// (the next sync starts fresh).
    pub fn meta(&self) -> Result<Option<Meta>, ReplicaError> {
        let path = self.meta_path();
        match fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_at(&path)(e)),
        }
    }

    /// The committed lines. A read-only view: it never repairs the replica, and
    /// ignores anything past `meta.lines`. Empty when there is no replica; shorter
    /// than `meta.lines` if the data file is damaged (the next sync repairs it).
    pub fn lines(&self) -> Result<Vec<String>, ReplicaError> {
        let Some(meta) = self.meta()? else {
            return Ok(Vec::new());
        };
        let path = self.facts_path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_at(&path)(e)),
        };
        Ok(String::from_utf8_lossy(&bytes)
            .split_terminator('\n')
            .take(meta.lines().get())
            .map(str::to_string)
            .collect())
    }

    fn write_meta(&self, meta: &Meta) -> Result<(), ReplicaError> {
        fs::create_dir_all(&self.dir).map_err(io_at(&self.dir))?;
        let path = self.meta_path();
        let json = serde_json::to_vec_pretty(meta).expect("Meta serializes");
        private_file::write_private_file(&path, &json).map_err(io_at(&path))
    }

    /// Record that a sync is about to be attempted, so the throttle holds across
    /// shells. Keeps everything else in the meta. Does nothing when a sync holds
    /// the lock (that sync records its own attempt).
    pub fn note_attempt(&self, now: UnixSecs) -> Result<(), ReplicaError> {
        let Some(_lock) = self.try_lock()? else {
            return Ok(());
        };
        let mut meta = self.meta()?.unwrap_or_default();
        meta.last_attempt = Some(now);
        self.write_meta(&meta)
    }

    /// Delete the replica: data, meta, probe and temporary files. Nothing is
    /// lost; it is rebuilt from the node by the next sync.
    ///
    /// Takes the sync lock first and fails with [`ReplicaError::Busy`] when a
    /// sync holds it. `sync.lock` itself stays, so a holder's lock is never
    /// replaced under it.
    ///
    /// Returns whether anything was removed (`false`: there was nothing to reset).
    pub fn reset(&self) -> Result<bool, ReplicaError> {
        if !self.dir.exists() {
            return Ok(false);
        }
        let Some(_lock) = self.try_lock()? else {
            return Err(ReplicaError::Busy(self.node.clone()));
        };
        let entries = fs::read_dir(&self.dir).map_err(io_at(&self.dir))?;
        let mut removed_any = false;
        for entry in entries {
            let path = entry.map_err(io_at(&self.dir))?.path();
            if path.file_name().is_some_and(|name| name == LOCK_FILE) {
                continue;
            }
            let removed = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            match removed {
                Ok(()) => removed_any = true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_at(&path)(e)),
            }
        }
        Ok(removed_any)
    }

    /// Take the sync lock without waiting. `None` when someone else holds it.
    pub fn try_lock(&self) -> Result<Option<SyncLock>, ReplicaError> {
        fs::create_dir_all(&self.dir).map_err(io_at(&self.dir))?;
        let path = self.dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(io_at(&path))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(SyncLock {
                file,
                dir: self.dir.clone(),
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(io_at(&path)(e)),
        }
    }

    /// Bring the replica up to date with the node.
    ///
    /// Takes the lock (returning [`SyncOutcome::Busy`] when it is taken), repairs
    /// the replica if it is damaged, then reads pages of [`PAGE_LINES`] lines
    /// after the last committed line, pinned to it. Each page is committed
    /// before the next is asked for. A diverged log or a new epoch discards the
    /// replica and refetches from line 0; a node that sends no epoch is refused
    /// ([`ReplicaError::NodeTooOld`]) and no replica is kept.
    ///
    /// The attempt is recorded either way: `last_attempt`, and `failures` for
    /// the backoff. `host` is remembered as a name the node was reached by.
    pub fn sync(
        &self,
        now: UnixSecs,
        host: Option<&Host>,
        read: &mut ReadPage,
    ) -> Result<SyncOutcome, ReplicaError> {
        let Some(lock) = self.try_lock()? else {
            return Ok(SyncOutcome::Busy);
        };
        self.sync_locked(&lock, now, host, read)
            .map(SyncOutcome::Synced)
    }

    /// [`Replica::sync`] for a caller that already holds the lock (from
    /// [`Replica::try_lock`]) and has more to write into the directory before
    /// releasing it.
    ///
    /// # Panics
    /// If `lock` is the lock of another replica: that is a bug in the caller, and
    /// syncing without this replica's lock would defeat it.
    pub fn sync_locked(
        &self,
        lock: &SyncLock,
        now: UnixSecs,
        host: Option<&Host>,
        read: &mut ReadPage,
    ) -> Result<SyncReport, ReplicaError> {
        assert_eq!(
            lock.dir, self.dir,
            "the sync lock belongs to another replica"
        );
        let mut meta = self.open_validated()?;
        let pulled = self.pull(&mut meta, read);
        meta.last_attempt = Some(now);
        match &pulled {
            Ok(_) => {
                meta.synced_at = Some(now);
                meta.failures = 0;
            }
            Err(_) => meta.failures = meta.failures.saturating_add(1),
        }
        if let Some(host) = host {
            meta.add_host(host);
        }
        let written = self.write_meta(&meta);
        let report = pulled?;
        written?;
        Ok(report)
    }

    /// Load the meta and make the data file agree with it, or start fresh.
    fn open_validated(&self) -> Result<Meta, ReplicaError> {
        let Some(meta) = self.meta()? else {
            self.remove_facts()?;
            return Ok(Meta::default());
        };
        if self.repair(&meta)? {
            Ok(meta)
        } else {
            self.wipe(&meta)
        }
    }

    /// Drop uncommitted lines past the committed count. `false` when the data
    /// cannot be trusted: shorter than the meta says, not UTF-8, or the last
    /// line's digest differs. (Whether each line parses as a fact is for the
    /// fold to decide, not the store.)
    fn repair(&self, meta: &Meta) -> Result<bool, ReplicaError> {
        let path = self.facts_path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(matches!(meta.committed, Committed::Empty { .. }))
            }
            Err(e) => return Err(io_at(&path)(e)),
        };
        let keep = match &meta.committed {
            Committed::Empty { .. } => 0,
            Committed::Lines {
                lines,
                last_line_sha256,
                ..
            } => {
                let Some(end) = nth_line_end(&bytes, lines.get()) else {
                    return Ok(false);
                };
                let Ok(text) = std::str::from_utf8(&bytes[..end]) else {
                    return Ok(false);
                };
                let last = text[..end - 1].rsplit('\n').next().unwrap_or("");
                if *last_line_sha256 != Digest::of_line(last) {
                    return Ok(false);
                }
                end
            }
        };
        if keep < bytes.len() {
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(io_at(&path))?;
            file.set_len(keep as u64).map_err(io_at(&path))?;
            file.sync_all().map_err(io_at(&path))?;
        }
        Ok(true)
    }

    fn remove_facts(&self) -> Result<(), ReplicaError> {
        let path = self.facts_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_at(&path)(e)),
        }
    }

    /// Throw the data away, keeping the bookkeeping; the new meta is committed
    /// at once so meta and data never disagree about what was dropped.
    fn wipe(&self, meta: &Meta) -> Result<Meta, ReplicaError> {
        self.remove_facts()?;
        let fresh = meta.emptied();
        self.write_meta(&fresh)?;
        Ok(fresh)
    }

    fn pull(&self, meta: &mut Meta, read: &mut ReadPage) -> Result<SyncReport, ReplicaError> {
        let mut reset = false;
        let mut added = 0usize;
        loop {
            let held = meta.lines().get();
            let cursor = (held > 0).then(|| cursor_from_offset(held));
            let page = read(cursor, PAGE_LINES, meta.pin())?;
            match page {
                PinnedRead::Unversioned => {
                    *meta = self.wipe(meta)?;
                    return Err(ReplicaError::NodeTooOld);
                }
                PinnedRead::Diverged { .. } => {
                    self.restart(meta, &mut reset, &mut added)?;
                }
                PinnedRead::Chunk {
                    lines,
                    epoch,
                    total_lines,
                    ..
                } => {
                    let epoch = Epoch::new(epoch);
                    if meta.epoch().is_some_and(|held| *held != epoch) {
                        self.restart(meta, &mut reset, &mut added)?;
                        continue;
                    }
                    if lines.is_empty() {
                        if meta.epoch().is_none() {
                            meta.committed = Committed::Empty { epoch: Some(epoch) };
                            self.write_meta(meta)?;
                        }
                        break;
                    }
                    self.append(meta, epoch, &lines)?;
                    added += lines.len();
                    if meta.lines().get() >= total_lines {
                        break;
                    }
                }
            }
        }
        Ok(SyncReport {
            added: LineCount::new(added),
            total: meta.lines(),
            reset,
        })
    }

    /// Discard the replica once per sync; a second time means the node's log is
    /// not holding still.
    fn restart(
        &self,
        meta: &mut Meta,
        reset: &mut bool,
        added: &mut usize,
    ) -> Result<(), ReplicaError> {
        if *reset {
            return Err(ReplicaError::Unstable);
        }
        *meta = self.wipe(meta)?;
        *reset = true;
        *added = 0;
        Ok(())
    }

    /// Append a page and fsync it, then commit it by rewriting the meta.
    fn append(&self, meta: &mut Meta, epoch: Epoch, lines: &[String]) -> Result<(), ReplicaError> {
        fs::create_dir_all(&self.dir).map_err(io_at(&self.dir))?;
        let path = self.facts_path();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_at(&path))?;
        let mut buffer = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
        for line in lines {
            buffer.push_str(line);
            buffer.push('\n');
        }
        file.write_all(buffer.as_bytes()).map_err(io_at(&path))?;
        file.sync_all().map_err(io_at(&path))?;

        let last = lines.last().expect("a page has lines");
        let total = NonZeroUsize::new(meta.lines().get() + lines.len())
            .expect("a page has lines, so the total is not zero");
        meta.committed = Committed::Lines {
            epoch,
            lines: total,
            last_line_sha256: Digest::of_line(last),
        };
        self.write_meta(meta)
    }
}

/// The offset just past the `n`th newline of `bytes` (1-based), if there are `n`.
fn nth_line_end(bytes: &[u8], n: usize) -> Option<usize> {
    bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(n - 1)
        .map(|(i, _)| i + 1)
}

// ---------------------------------------------------------------------------
// Host lookup
// ---------------------------------------------------------------------------

/// The node under `nodes_dir` that was reached as `host`, from the replicas'
/// metas. No network lookup: this is what completion uses.
pub fn id_for_host(nodes_dir: &Path, host: &Host) -> Option<NodeId> {
    holders_of(nodes_dir, host).into_iter().next()
}

/// Every replica under `nodes_dir` that lists `host`, sorted by id.
fn holders_of(nodes_dir: &Path, host: &Host) -> Vec<NodeId> {
    let Ok(entries) = fs::read_dir(nodes_dir) else {
        return Vec::new();
    };
    let mut found: Vec<(NodeId, Option<UnixSecs>)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| NodeId::new(entry.file_name().to_str()?).ok())
        .filter_map(|id| {
            let meta = Replica::new(nodes_dir, &id).meta().ok().flatten()?;
            meta.hosts.contains(host).then_some((id, meta.synced_at))
        })
        .collect();
    // The most recently synced first (a name moves to whichever node answered
    // last); ids break ties so the order is deterministic.
    found.sort_by(|(a_id, a_at), (b_id, b_at)| b_at.cmp(a_at).then_with(|| a_id.cmp(b_id)));
    found.into_iter().map(|(id, _)| id).collect()
}

/// Remove `host` from every replica except `keep`'s: a name now answered by a
/// different node must not still lead to the old one. Returns the nodes it was
/// removed from. A replica whose lock is held (a sync is running) is skipped;
/// `id_for_host` prefers the most recent sync, so the stale entry loses anyway.
pub fn release_host_elsewhere(
    nodes_dir: &Path,
    host: &Host,
    keep: &NodeId,
) -> Result<Vec<NodeId>, ReplicaError> {
    let mut released = Vec::new();
    for id in holders_of(nodes_dir, host) {
        if id == *keep {
            continue;
        }
        let replica = Replica::new(nodes_dir, &id);
        let Some(_lock) = replica.try_lock()? else {
            continue;
        };
        if let Some(mut meta) = replica.meta()? {
            meta.hosts.retain(|h| h != host);
            replica.write_meta(&meta)?;
            released.push(id);
        }
    }
    Ok(released)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeNode, Request};
    use pretty_assertions::assert_eq;

    trait SyncFrom {
        fn sync(&self, replica: &Replica, now: u64) -> Result<SyncOutcome, ReplicaError>;
    }

    impl SyncFrom for FakeNode {
        fn sync(&self, replica: &Replica, now: u64) -> Result<SyncOutcome, ReplicaError> {
            replica.sync(UnixSecs::new(now), None, &mut |c, l, p| self.serve(c, l, p))
        }
    }

    fn node_id() -> NodeId {
        NodeId::new("mdma-test").unwrap()
    }

    fn replica_in(dir: &tempfile::TempDir) -> Replica {
        Replica::new(dir.path(), &node_id())
    }

    fn synced(outcome: Result<SyncOutcome, ReplicaError>) -> SyncReport {
        match outcome.expect("sync succeeds") {
            SyncOutcome::Synced(report) => report,
            SyncOutcome::Busy => panic!("unexpectedly busy"),
        }
    }

    fn facts(replica: &Replica) -> PathBuf {
        replica.dir().join(FACTS_FILE)
    }

    // --- sync ----------------------------------------------------------------

    #[test]
    fn fresh_sync_pages_and_commits() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 12_000);

        let report = synced(node.sync(&replica, 100));

        assert_eq!(report.added, LineCount::new(12_000));
        assert_eq!(report.total, LineCount::new(12_000));
        assert!(!report.reset);
        let requests = node.requests();
        assert_eq!(requests.len(), 3, "5000 + 5000 + 2000");
        assert_eq!(
            requests[0],
            Request {
                cursor: None,
                limit: PAGE_LINES,
                pin: None
            },
            "a read from line 0 is unpinned"
        );
        assert_eq!(requests[1].cursor.as_deref(), Some("line:5000"));
        assert_eq!(requests[1].pin.as_ref().unwrap().line, 4999);
        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(
            meta.committed,
            Committed::Lines {
                epoch: Epoch::new("e1"),
                lines: NonZeroUsize::new(12_000).unwrap(),
                last_line_sha256: Digest::of_line(node.lines.last().unwrap()),
            }
        );
        assert_eq!(meta.synced_at, Some(UnixSecs::new(100)));
        assert_eq!(meta.last_attempt, Some(UnixSecs::new(100)));
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn incremental_sync_pins_the_last_line_and_appends_only_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let mut node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));

        node.lines.push(r#"["track:new1","x"]"#.into());
        node.lines.push(r#"["track:new2","x"]"#.into());
        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        assert_eq!(report.added, LineCount::new(2));
        assert_eq!(report.total, LineCount::new(5));
        assert!(!report.reset);
        let requests = node.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].cursor.as_deref(), Some("line:3"));
        assert_eq!(
            requests[0].pin,
            Some(LinePin {
                line: 2,
                sha256: line_digest(&node.lines[2])
            })
        );
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn sync_with_nothing_new_changes_no_lines_but_records_the_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));

        let report = synced(node.sync(&replica, 150));

        assert_eq!(report.added, LineCount::new(0));
        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(meta.synced_at, Some(UnixSecs::new(150)));
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn an_empty_log_gets_an_epoch_and_no_lines() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 0);

        let report = synced(node.sync(&replica, 100));

        assert_eq!(report.total, LineCount::new(0));
        assert_eq!(
            replica.meta().unwrap().unwrap().epoch(),
            Some(&Epoch::new("e1"))
        );
    }

    #[test]
    fn a_failed_page_keeps_what_was_committed_and_the_next_sync_resumes_there() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 12_000);
        node.fail_request(1);

        let err = node.sync(&replica, 100).unwrap_err();

        assert!(matches!(err, ReplicaError::Read(_)), "{err}");
        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(meta.lines(), LineCount::new(5_000), "the first page held");
        assert_eq!(meta.failures, 1);
        assert_eq!(meta.synced_at, None);
        assert_eq!(replica.lines().unwrap(), node.lines[..5_000]);

        node.stop_failing();
        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        let first = &node.requests()[0];
        assert_eq!(first.cursor.as_deref(), Some("line:5000"));
        assert_eq!(first.pin.as_ref().unwrap().line, 4_999);
        assert_eq!(
            first.pin.as_ref().unwrap().sha256,
            line_digest(&node.lines[4_999])
        );
        assert!(!report.reset, "resumed, not refetched");
        assert_eq!(report.added, LineCount::new(7_000));
        assert_eq!(replica.lines().unwrap(), node.lines);
        assert_eq!(replica.meta().unwrap().unwrap().failures, 0);
    }

    #[test]
    fn a_rewritten_log_diverges_and_the_replica_is_refetched_from_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let mut node = FakeNode::new("e1", 5);
        synced(node.sync(&replica, 100));

        // The log is rewritten in place: same length, different content.
        node.lines[4] = r#"["track:4","rewritten"]"#.into();
        node.lines[1] = r#"["track:1","rewritten"]"#.into();
        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        assert!(report.reset, "a divergence resets");
        assert_eq!(report.added, LineCount::new(5), "everything is refetched");
        assert_eq!(replica.lines().unwrap(), node.lines);
        let requests = node.requests();
        assert_eq!(requests.len(), 2, "the pinned read, then the refetch");
        assert!(requests[0].pin.is_some());
        assert_eq!(
            requests[1],
            Request {
                cursor: None,
                limit: PAGE_LINES,
                pin: None
            }
        );
    }

    #[test]
    fn a_shrunk_log_diverges_instead_of_looking_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let mut node = FakeNode::new("e1", 5);
        synced(node.sync(&replica, 100));

        node.lines.truncate(2);
        let report = synced(node.sync(&replica, 200));

        assert!(report.reset);
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn a_new_epoch_resets_even_when_the_pinned_line_still_matches() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let mut node = FakeNode::new("e1", 4);
        synced(node.sync(&replica, 100));

        node.epoch = "e2".into();
        node.lines.push(r#"["track:more","x"]"#.into());
        let report = synced(node.sync(&replica, 200));

        assert!(report.reset);
        assert_eq!(report.added, LineCount::new(5));
        assert_eq!(
            replica.meta().unwrap().unwrap().epoch(),
            Some(&Epoch::new("e2"))
        );
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn a_log_that_keeps_diverging_is_unstable_not_a_loop() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));

        // On purpose: answers `Diverged` even to the unpinned read after a reset,
        // which ACID never does (an unpinned read cannot diverge). It exists to
        // test the guard that stops a misbehaving node from looping the client.
        let outcome = replica.sync(UnixSecs::new(200), None, &mut |_, _, _| {
            Ok(PinnedRead::Diverged {
                epoch: "e1".into(),
                total_lines: 3,
            })
        });

        assert!(matches!(outcome, Err(ReplicaError::Unstable)));
        assert_eq!(replica.meta().unwrap().unwrap().failures, 1);
    }

    #[test]
    fn an_unversioned_node_is_too_old_and_keeps_no_replica() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let mut node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));
        node.versioned = false;

        let err = node.sync(&replica, 200).unwrap_err();

        assert_eq!(err.to_string(), "node too old for the replica");
        assert_eq!(replica.lines().unwrap(), Vec::<String>::new());
        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(meta.committed, Committed::Empty { epoch: None });
        assert_eq!(meta.failures, 1);
        assert!(!facts(&replica).exists());
    }

    #[test]
    fn a_failed_read_counts_as_a_failure_and_a_success_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 2);

        let err = replica
            .sync(UnixSecs::new(100), None, &mut |_, _, _| {
                Err(ReadFailure("connection refused".into()))
            })
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "reading the node's fact log failed: connection refused"
        );
        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(meta.failures, 1);
        assert_eq!(meta.last_attempt, Some(UnixSecs::new(100)));
        assert_eq!(meta.synced_at, None);

        synced(node.sync(&replica, 200));
        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(meta.failures, 0);
        assert_eq!(meta.synced_at, Some(UnixSecs::new(200)));
    }

    #[test]
    fn a_sync_remembers_the_host_once() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 1);
        let host = Host::new("MDMA-Test.local");
        for now in [100, 200] {
            replica
                .sync(UnixSecs::new(now), Some(&host), &mut |c, l, p| {
                    node.serve(c, l, p)
                })
                .unwrap();
        }
        assert_eq!(
            replica.meta().unwrap().unwrap().hosts,
            vec![Host::new("mdma-test.local")]
        );
    }

    // --- recovery ------------------------------------------------------------

    #[test]
    fn a_torn_tail_past_the_commit_point_is_dropped_and_sync_stays_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let mut node = FakeNode::new("e1", 4);
        synced(node.sync(&replica, 100));
        // A crash after the append but before the meta: lines past meta.lines,
        // the last one cut short.
        let mut file = OpenOptions::new()
            .append(true)
            .open(facts(&replica))
            .unwrap();
        file.write_all(b"[\"track:4\",\"x\"]\n[\"track:5\",\"x")
            .unwrap();
        drop(file);

        node.lines.push(r#"["track:4","x"]"#.into());
        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        assert!(!report.reset, "a torn tail is repaired, not refetched");
        assert_eq!(report.added, LineCount::new(1));
        assert_eq!(node.requests()[0].cursor.as_deref(), Some("line:4"));
        assert_eq!(replica.lines().unwrap(), node.lines);
        let on_disk = fs::read_to_string(facts(&replica)).unwrap();
        assert_eq!(on_disk.lines().count(), 5);
        assert!(on_disk.ends_with('\n'));
    }

    #[test]
    fn a_data_file_shorter_than_the_meta_says_is_wiped_and_refetched() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 5);
        synced(node.sync(&replica, 100));
        let text = fs::read_to_string(facts(&replica)).unwrap();
        let two: String = text.split_inclusive('\n').take(2).collect();
        fs::write(facts(&replica), two).unwrap();

        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        assert_eq!(report.added, LineCount::new(5));
        assert_eq!(node.requests()[0].cursor, None, "refetched from line 0");
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn a_missing_data_file_is_wiped_and_refetched() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));
        fs::remove_file(facts(&replica)).unwrap();

        node.forget_requests();
        synced(node.sync(&replica, 200));

        assert_eq!(node.requests()[0].cursor, None);
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn a_last_line_that_fails_its_checksum_is_wiped_and_refetched() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 5);
        synced(node.sync(&replica, 100));
        // Same length, different byte: only the checksum can tell.
        let text = fs::read_to_string(facts(&replica)).unwrap();
        let damaged = text.replace(r#"["track:4","x"]"#, r#"["track:4","y"]"#);
        assert_ne!(text, damaged);
        fs::write(facts(&replica), damaged).unwrap();

        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        assert_eq!(report.added, LineCount::new(5));
        assert_eq!(node.requests()[0].cursor, None, "refetched from line 0");
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn data_that_is_not_text_is_wiped_and_refetched() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));
        let mut bytes = fs::read(facts(&replica)).unwrap();
        bytes[2] = 0xff;
        fs::write(facts(&replica), bytes).unwrap();

        node.forget_requests();
        synced(node.sync(&replica, 200));

        assert_eq!(node.requests()[0].cursor, None);
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn an_unparseable_meta_starts_fresh_and_a_stale_data_file_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));
        fs::write(replica.dir().join(META_FILE), b"{ not json").unwrap();
        assert_eq!(replica.meta().unwrap(), None);

        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        assert_eq!(report.added, LineCount::new(3));
        assert_eq!(node.requests()[0].cursor, None);
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    #[test]
    fn lines_ignores_anything_past_the_commit_point() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 2);
        synced(node.sync(&replica, 100));
        let mut file = OpenOptions::new()
            .append(true)
            .open(facts(&replica))
            .unwrap();
        file.write_all(b"[\"uncommitted\"]\n").unwrap();

        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    // --- lock ----------------------------------------------------------------

    #[test]
    fn a_held_lock_makes_sync_report_busy_without_touching_the_node() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let first = replica.try_lock().unwrap().expect("first lock is free");

        let outcome = replica
            .sync(UnixSecs::new(100), None, &mut |_, _, _| {
                panic!("a busy sync must not read")
            })
            .unwrap();

        assert_eq!(outcome, SyncOutcome::Busy);
        assert_eq!(replica.meta().unwrap(), None, "and writes nothing");

        drop(first);
        let node = FakeNode::new("e1", 1);
        synced(node.sync(&replica, 200));
    }

    // --- throttle ------------------------------------------------------------

    #[test]
    fn the_interval_backs_off_from_30_seconds_to_5_minutes() {
        let schedule: Vec<u64> = (0..8).map(|f| interval_after(f).as_secs()).collect();
        assert_eq!(schedule, [30, 60, 120, 240, 300, 300, 300, 300]);
        assert_eq!(interval_after(u32::MAX).as_secs(), 300);
    }

    #[test]
    fn an_attempt_is_due_after_the_interval_for_the_failures_so_far() {
        let meta = |failures, last| Meta {
            failures,
            last_attempt: Some(UnixSecs::new(last)),
            ..Meta::default()
        };
        let due = |m: &Meta, now| should_attempt(UnixSecs::new(now), Some(m));

        assert!(should_attempt(UnixSecs::new(0), None), "no replica yet");
        assert!(should_attempt(UnixSecs::new(0), Some(&Meta::default())));
        let fresh = meta(0, 1000);
        assert!(!due(&fresh, 1000));
        assert!(!due(&fresh, 1029));
        assert!(due(&fresh, 1030));
        let twice = meta(2, 1000);
        assert!(!due(&twice, 1119));
        assert!(due(&twice, 1120));
        let many = meta(9, 1000);
        assert!(!due(&many, 1299));
        assert!(due(&many, 1300));
        assert!(due(&meta(0, 5000), 1000), "a clock that went backwards");
    }

    #[test]
    fn note_attempt_keeps_the_rest_of_the_meta() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 2);
        synced(node.sync(&replica, 100));

        replica.note_attempt(UnixSecs::new(500)).unwrap();

        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(meta.last_attempt, Some(UnixSecs::new(500)));
        assert_eq!(meta.lines(), LineCount::new(2));
        assert_eq!(meta.synced_at, Some(UnixSecs::new(100)));
    }

    #[test]
    fn note_attempt_leaves_a_replica_alone_while_a_sync_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 2);
        synced(node.sync(&replica, 100));
        let _held = replica.try_lock().unwrap().unwrap();

        replica.note_attempt(UnixSecs::new(500)).unwrap();

        let meta = replica.meta().unwrap().unwrap();
        assert_eq!(meta.last_attempt, Some(UnixSecs::new(100)));
    }

    // --- meta format ---------------------------------------------------------

    #[test]
    fn a_committed_meta_round_trips_flat_and_the_earlier_flat_layout_still_reads() {
        let earlier = r#"{
            "epoch": "e1", "lines": 3, "last_line_sha256": "abc",
            "synced_at": 5, "last_attempt": 6, "failures": 2, "hosts": ["a.local"]
        }"#;
        let meta: Meta = serde_json::from_str(earlier).unwrap();
        assert_eq!(meta.lines(), LineCount::new(3));
        assert_eq!(meta.epoch(), Some(&Epoch::new("e1")));
        assert_eq!(meta.failures, 2);

        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["lines"], 3);
        assert_eq!(json["epoch"], "e1");
        assert_eq!(json["last_line_sha256"], "abc");
        assert_eq!(serde_json::from_value::<Meta>(json).unwrap(), meta);
    }

    #[test]
    fn lines_without_a_digest_or_an_epoch_cannot_be_committed() {
        // The earlier layout could say this; it now reads as empty, so the
        // replica is rebuilt rather than trusted.
        for text in [
            r#"{"epoch":"e1","lines":3,"last_line_sha256":null,"synced_at":null,"last_attempt":null}"#,
            r#"{"epoch":null,"lines":3,"last_line_sha256":"abc","synced_at":null,"last_attempt":null}"#,
            r#"{"epoch":"e1","lines":0,"last_line_sha256":null,"synced_at":null,"last_attempt":null}"#,
        ] {
            let meta: Meta = serde_json::from_str(text).unwrap();
            assert_eq!(meta.lines(), LineCount::new(0), "{text}");
        }
    }

    #[test]
    fn an_older_meta_that_claimed_lines_without_a_digest_is_wiped_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));
        fs::write(
            replica.dir().join(META_FILE),
            r#"{"epoch":"e1","lines":3,"last_line_sha256":null,"synced_at":null,"last_attempt":null}"#,
        )
        .unwrap();

        node.forget_requests();
        let report = synced(node.sync(&replica, 200));

        assert_eq!(report.added, LineCount::new(3));
        assert_eq!(node.requests()[0].cursor, None);
        assert_eq!(replica.lines().unwrap(), node.lines);
    }

    // --- reset, hosts, ids ---------------------------------------------------

    #[test]
    fn reset_deletes_everything_but_the_lock_file_and_tolerates_none() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        assert!(!replica.reset().unwrap(), "nothing to reset");
        assert!(!replica.dir().exists(), "nothing to reset, nothing created");
        let node = FakeNode::new("e1", 2);
        synced(node.sync(&replica, 100));
        fs::write(replica.dir().join("probe.json"), b"{}").unwrap();
        fs::write(replica.dir().join(".meta.json.abc.tmp"), b"x").unwrap();

        assert!(replica.reset().unwrap(), "something was removed");
        assert!(!replica.reset().unwrap(), "only the lock file is left");

        let mut left: Vec<String> = fs::read_dir(replica.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [LOCK_FILE]);
        assert_eq!(replica.meta().unwrap(), None);
        assert_eq!(replica.lines().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn reset_while_a_sync_holds_the_lock_is_busy_and_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let replica = replica_in(&dir);
        let node = FakeNode::new("e1", 3);
        synced(node.sync(&replica, 100));
        fs::write(replica.dir().join("probe.json"), b"{}").unwrap();
        let held = replica.try_lock().unwrap().expect("lock is free");

        let err = replica.reset().unwrap_err();

        assert!(matches!(err, ReplicaError::Busy(_)));
        assert_eq!(err.to_string(), "a sync of mdma-test is running; try again");
        assert_eq!(replica.lines().unwrap(), node.lines);
        assert!(replica.dir().join("probe.json").exists());
        assert!(replica.dir().join(LOCK_FILE).exists());

        drop(held);
        replica.reset().unwrap();
        assert_eq!(replica.meta().unwrap(), None);
    }

    #[test]
    #[should_panic(expected = "the sync lock belongs to another replica")]
    fn a_lock_of_another_replica_cannot_be_used_to_sync() {
        let dir = tempfile::tempdir().unwrap();
        let mine = replica_in(&dir);
        let other = Replica::new(dir.path(), &NodeId::new("other").unwrap());
        let others_lock = other.try_lock().unwrap().unwrap();

        let _ = mine.sync_locked(&others_lock, UnixSecs::new(1), None, &mut |_, _, _| {
            panic!("must not read")
        });
    }

    #[test]
    fn the_node_synced_most_recently_wins_a_name_two_replicas_claim() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::new("e1", 1);
        let host = Host::new("shared.local");
        for (id, at) in [("zed", 10), ("alpha", 20), ("mid", 5)] {
            Replica::new(dir.path(), &NodeId::new(id).unwrap())
                .sync(UnixSecs::new(at), Some(&host), &mut |c, l, p| {
                    node.serve(c, l, p)
                })
                .unwrap();
        }
        assert_eq!(
            id_for_host(dir.path(), &host),
            Some(NodeId::new("alpha").unwrap())
        );
    }

    #[test]
    fn a_name_is_not_released_from_a_replica_that_is_syncing() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::new("e1", 1);
        let host = Host::new("shared.local");
        let busy = NodeId::new("busy").unwrap();
        let keep = NodeId::new("keep").unwrap();
        for id in [&busy, &keep] {
            Replica::new(dir.path(), id)
                .sync(UnixSecs::new(1), Some(&host), &mut |c, l, p| {
                    node.serve(c, l, p)
                })
                .unwrap();
        }
        let replica = Replica::new(dir.path(), &busy);
        let _held = replica.try_lock().unwrap().unwrap();

        let released = release_host_elsewhere(dir.path(), &host, &keep).unwrap();

        assert!(released.is_empty());
        assert!(replica.meta().unwrap().unwrap().hosts.contains(&host));
    }

    #[test]
    fn id_for_host_finds_the_node_by_a_name_it_was_reached_by() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::new("e1", 1);
        for (id, host) in [("alpha", "alpha.local"), ("beta", "beta.local")] {
            let replica = Replica::new(dir.path(), &NodeId::new(id).unwrap());
            replica
                .sync(UnixSecs::new(1), Some(&Host::new(host)), &mut |c, l, p| {
                    node.serve(c, l, p)
                })
                .unwrap();
        }
        // Not replicas: a stray file and an unusable directory name.
        fs::write(dir.path().join("stray"), b"").unwrap();
        fs::create_dir(dir.path().join(".hidden")).unwrap();

        assert_eq!(
            id_for_host(dir.path(), &Host::new("BETA.local")),
            Some(NodeId::new("beta").unwrap())
        );
        assert_eq!(id_for_host(dir.path(), &Host::new("gamma.local")), None);
        assert_eq!(
            id_for_host(&dir.path().join("missing"), &Host::new("a")),
            None
        );
    }

    #[test]
    fn a_name_that_moves_to_another_node_is_released_from_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let node = FakeNode::new("e1", 1);
        let host = Host::new("shared.local");
        let old = NodeId::new("old").unwrap();
        let new = NodeId::new("new").unwrap();
        for id in [&old, &new] {
            Replica::new(dir.path(), id)
                .sync(UnixSecs::new(1), Some(&host), &mut |c, l, p| {
                    node.serve(c, l, p)
                })
                .unwrap();
        }
        // Both claim the name, synced at the same time: ids break the tie.
        assert_eq!(id_for_host(dir.path(), &host), Some(new.clone()));

        let released = release_host_elsewhere(dir.path(), &host, &old).unwrap();

        assert_eq!(released, vec![new.clone()]);
        assert_eq!(id_for_host(dir.path(), &host), Some(old.clone()));
        assert!(Replica::new(dir.path(), &new)
            .meta()
            .unwrap()
            .unwrap()
            .hosts
            .is_empty());
    }

    #[test]
    fn node_ids_cannot_escape_the_replica_directory() {
        for good in ["mdma-johlyroger", "a1", "node_2", "v1.2"] {
            assert!(NodeId::new(good).is_ok(), "{good}");
        }
        for bad in ["", ".", "..", "../x", "a/b", ".hidden", "a b", "a\\b"] {
            assert!(NodeId::new(bad).is_err(), "{bad:?}");
        }
    }
}
