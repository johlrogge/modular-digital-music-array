//! Where a client reads facts from (ADR-030 §2, §3): the replica or the node.
//!
//! [`Facts`] is a closed enum, not a trait (ADR-030 §2.4). Every read is one `match`
//! with two arms: the replica arm calls the `library_projection` read functions over
//! the folded replica, the node arm sends the `LibraryRequest` the service answers
//! with the same functions. Nothing falls back to the other source.
//!
//! The replica view is a pure function of the replica's lines (ADR-030 §9): it never
//! holds outbox, probe or playlist data, so offline writes can later be folded on top
//! of it per read.

use std::ops::Deref;
use std::str::FromStr;

use fact_replica::UnixSecs;
use library_ipc_client::{ContentHash, FactType, MarkFilter, ProtocolError, TrackInfo, TrackQuery};
use library_projection::{
    entity_facts, resolve_prefix, resolve_visible, track_info, unresolved_error, AlbumCovers,
    IndexedTrackInfo, Projection,
};
use mdma_client::node::NodeError;
use mdma_client::{LibraryBackend, Node};

use crate::error::FactsError;
use crate::replica::{ago, fold_current, replica_hint, ReplicaFold};

/// Which source `--facts` chose. The default is the replica (ADR-030 §2.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FactsChoice {
    #[default]
    Replica,
    Node,
}

/// `--facts` (or `MDMA_FACTS`) was given something other than `replica` or `node`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown facts source '{0}'; use replica or node")]
pub struct UnknownFactsChoice(pub String);

impl FromStr for FactsChoice {
    type Err = UnknownFactsChoice;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "replica" => Ok(FactsChoice::Replica),
            "node" => Ok(FactsChoice::Node),
            other => Err(UnknownFactsChoice(other.to_string())),
        }
    }
}

/// The replica of the node in use, folded, with plays applied and the album covers:
/// everything a read needs, and what the sync said about it for the source note.
pub struct ReplicaView {
    fold: ReplicaFold,
    covers: AlbumCovers,
}

impl ReplicaView {
    /// The view of `fold`: its plays applied to its tracks, and the covers built.
    pub fn new(mut fold: ReplicaFold) -> Self {
        fold.projection.plays.apply_to(&mut fold.projection.tracks);
        let covers = AlbumCovers::build(&fold.projection.tracks);
        Self { fold, covers }
    }

    fn tracks(&self) -> &[IndexedTrackInfo] {
        &self.fold.projection.tracks
    }

    /// The source note (ADR-030 §3): which node, how old, whether it is behind and
    /// whether lines were unreadable; one line each.
    fn note(&self, now: UnixSecs) -> String {
        let fold = &self.fold;
        let behind = fold.last_attempt > fold.synced_at || fold.failures > 0;
        let unreadable = fold.projection.unparseable_lines;
        [
            Some(format!(
                "replica of {}, synced {}",
                fold.id,
                ago(now, fold.synced_at)
            )),
            behind.then(|| "behind: the last sync attempt failed".to_string()),
            (unreadable > 0)
                .then(|| format!("{unreadable} lines this mdma cannot read; upgrade mdma")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
    }
}

/// The library to ask. Only a node that cannot be resolved or reached is
/// unreachable (and names the replica as the alternative); any other failure is not
/// about reaching it.
fn library(node: &Node) -> Result<LibraryBackend, FactsError> {
    node.library().map_err(|error| match error {
        cause @ NodeError::Needed { .. } => FactsError::NodeUnreachable {
            hint: replica_hint(node, UnixSecs::now()),
            cause,
        },
        other => FactsError::Node(other),
    })
}

/// A source of facts. `N` is how the node is held: `&Node` for a command that lives
/// shorter than its node, `Rc<Node>` for the TUI, which keeps both for its whole life.
pub enum Facts<N> {
    /// Boxed: the folded view is large next to a handle on the node.
    Replica(Box<ReplicaView>),
    Node(N),
}

impl<N: Deref<Target = Node>> Facts<N> {
    /// Open the source `choice` names. The replica is folded now; the node is not
    /// touched until a read needs it (it is lazy, ADR-030 §4).
    pub fn open(node: N, choice: FactsChoice) -> Result<Self, FactsError> {
        match choice {
            FactsChoice::Replica => {
                let fold = fold_current(&node)?;
                Ok(Facts::Replica(Box::new(ReplicaView::new(fold))))
            }
            FactsChoice::Node => Ok(Facts::Node(node)),
        }
    }

    /// Tracks matching `query`, in catalog order.
    pub fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, FactsError> {
        match self {
            Facts::Replica(view) => Ok(library_projection::search(
                view.tracks(),
                &view.covers,
                query,
            )),
            Facts::Node(node) => Ok(library(node)?.search(query)?),
        }
    }

    /// All visible tracks, in catalog order, optionally limited.
    pub fn tracks(&self, limit: Option<usize>) -> Result<Vec<TrackInfo>, FactsError> {
        match self {
            Facts::Replica(view) => Ok(library_projection::visible(
                view.tracks(),
                &view.covers,
                limit,
            )),
            Facts::Node(node) => Ok(library(node)?.list_tracks(limit)?),
        }
    }

    /// The visible track `hash` names (a prefix will do).
    pub fn track(&self, hash: &ContentHash) -> Result<TrackInfo, FactsError> {
        match self {
            Facts::Replica(view) => Ok(resolve_visible(view.tracks(), hash.as_str())
                .map(|track| track_info(track, &view.covers))
                .map_err(|unresolved| unresolved_error(hash.as_str(), unresolved))?),
            Facts::Node(node) => Ok(library(node)?.get_track(hash)?),
        }
    }

    /// The visible tracks carrying a mark `filter` admits, in catalog order.
    pub fn marked(&self, filter: &MarkFilter) -> Result<Vec<TrackInfo>, FactsError> {
        match self {
            Facts::Replica(view) => Ok(library_projection::marked(
                view.tracks(),
                &view.covers,
                filter,
            )),
            Facts::Node(node) => Ok(library(node)?.list_marked(filter)?),
        }
    }

    /// The values currently asserted for `fact_type`, sorted.
    pub fn fact_values(&self, fact_type: &FactType) -> Result<Vec<String>, FactsError> {
        match self {
            Facts::Replica(view) => Ok(library_projection::fact_values(
                &view.fold.projection.fact_index,
                fact_type,
            )),
            Facts::Node(node) => Ok(library(node)?.get_fact_values(fact_type.as_str())?),
        }
    }

    /// The facts of the track `hash` names (a prefix will do), formatted for display.
    pub fn facts_of(&self, hash: &ContentHash) -> Result<Vec<(String, String)>, FactsError> {
        match self {
            Facts::Replica(view) => {
                let full = resolve_prefix(view.tracks(), hash.as_str())
                    .map(|track| track.content_hash.clone())
                    .map_err(|unresolved| unresolved_error(hash.as_str(), unresolved))?;
                // The lines are read again, from disk, not kept from the fold (they
                // would double its memory). A sync appends and a reset deletes, so a
                // reset between the fold and here is read as "unreadable".
                let lines = view
                    .fold
                    .replica
                    .lines()
                    .map_err(|e| FactsError::unreadable(&view.fold.id, &e))?;
                let facts = entity_facts(lines.iter().map(String::as_str), &full);
                // As the service answers: a track without a fact is not found.
                if facts.is_empty() {
                    return Err(FactsError::Library(ProtocolError::TrackNotFound {
                        hash: full.as_str().to_owned(),
                    }));
                }
                Ok(facts)
            }
            Facts::Node(node) => Ok(library(node)?.get_facts(hash)?.1),
        }
    }

    /// The fold with every play: tracks, their play times and the play log. Only the
    /// replica has it; the node has no plays request yet (ADR-030 §2.2).
    pub fn plays(&self) -> Result<&Projection, FactsError> {
        match self {
            Facts::Replica(view) => Ok(&view.fold.projection),
            Facts::Node(_) => Err(FactsError::PlaysFromNode),
        }
    }

    /// What to say on stderr about where an answer comes from (ADR-030 §3); one line
    /// each for the replica and its age, for being behind, and for lines this mdma
    /// cannot read. The node says nothing.
    pub fn source_note(&self, now: UnixSecs) -> Option<String> {
        match self {
            Facts::Replica(view) => Some(view.note(now)),
            Facts::Node(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        asserted, env_lock, failure_text, nodes_dir, offline_env, replica_with_lines, EnvVars,
    };
    use fact_replica::{NodeId, Replica};
    use library_ipc_client::StringQuery;
    use mdma_client::node::NeededBy;
    use mdma_client::NodeOptions;
    use music_facts::{Album, Artist, Mark, MusicValue, Title};
    use pretty_assertions::assert_eq;
    use std::path::Path;

    const BLUE: &str = "sha256:aaaa000000000001";
    const CEREMONY: &str = "sha256:aaaa000000000002";
    const ATOMIC: &str = "sha256:cccc000000000003";
    const AT: &str = "2026-01-02T10:00:00Z";

    fn secs(n: u64) -> UnixSecs {
        UnixSecs::new(n)
    }

    fn fact(entity: &str, value: &MusicValue) -> String {
        asserted(entity, &serde_json::to_string(value).unwrap(), AT)
    }

    /// Three tracks, one marked, one played.
    fn catalog() -> Vec<String> {
        let title = |t: &str| MusicValue::Title(Title::new(t));
        let artist = |a: &str| MusicValue::Artist(Artist::new(a));
        let started = r#"{"t":"TrackStarted","v":"OnRequest"}"#;
        vec![
            fact(BLUE, &title("Blue Monday")),
            fact(BLUE, &artist("New Order")),
            fact(BLUE, &MusicValue::Album(Album::new("Power"))),
            fact(BLUE, &Mark::Default.fact_value(AT.parse().unwrap())),
            asserted(BLUE, started, AT),
            fact(CEREMONY, &title("Ceremony")),
            fact(CEREMONY, &artist("New Order")),
            fact(ATOMIC, &title("Atomic")),
            fact(ATOMIC, &artist("Blondie")),
        ]
    }

    /// A node with no service behind it: reading from it fails.
    fn dead_node(dir: &Path) -> Node {
        dead_node_named(dir, "mdma-stage2.local")
    }

    fn dead_node_named(dir: &Path, name: &str) -> Node {
        let options = NodeOptions {
            node: Some(name.parse().unwrap()),
            library_socket: Some(format!("ipc://{}/none.sock", dir.display())),
            ..NodeOptions::default()
        };
        Node::with_target(options, node_discovery::Target::LocalIpc)
    }

    /// The cache and config in a temp dir, and the replica of `lines` synced into it
    /// (none when there are no lines). Fields drop in order: the environment is put
    /// back before the lock is released.
    struct World {
        _env: EnvVars,
        dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl World {
        fn new(lines: Option<Vec<String>>) -> Self {
            let lock = env_lock();
            let dir = tempfile::tempdir().unwrap();
            let env = offline_env(dir.path());
            if let Some(lines) = lines {
                let host = "mdma-stage2.local";
                replica_with_lines(&nodes_dir(dir.path()), "stage", host, lines);
            }
            World {
                _env: env,
                dir,
                _lock: lock,
            }
        }

        /// A node with no service behind it: reading from it fails.
        fn dead_node(&self) -> Node {
            dead_node(self.dir.path())
        }
    }

    /// Runs `check` in a [`World`], with a dead node.
    fn with_world(lines: Option<Vec<String>>, check: impl FnOnce(&Node)) {
        let world = World::new(lines);
        check(&world.dead_node());
    }

    fn replica_of(node: &Node) -> Facts<&Node> {
        Facts::open(node, FactsChoice::Replica).expect("the replica opens")
    }

    fn titles(tracks: &[TrackInfo]) -> Vec<&str> {
        tracks.iter().filter_map(|t| t.title.as_deref()).collect()
    }

    // --- choosing the source -------------------------------------------------

    #[test]
    fn the_facts_choice_is_replica_or_node_and_defaults_to_the_replica() {
        assert_eq!("replica".parse(), Ok(FactsChoice::Replica));
        assert_eq!("node".parse(), Ok(FactsChoice::Node));
        assert_eq!(FactsChoice::default(), FactsChoice::Replica);
        let refused = "cache".parse::<FactsChoice>().unwrap_err();
        assert_eq!(
            refused.to_string(),
            "unknown facts source 'cache'; use replica or node"
        );
        assert!(
            "Replica".parse::<FactsChoice>().is_err(),
            "exactly as spelled"
        );
        assert!("".parse::<FactsChoice>().is_err());
    }

    // --- the replica arm ------------------------------------------------------

    #[test]
    fn search_and_tracks_answer_from_the_replica_in_catalog_order() {
        with_world(Some(catalog()), |node| {
            let facts = replica_of(node);
            let new_order = TrackQuery {
                artist: Some(StringQuery::Contains("new order".into())),
                ..TrackQuery::default()
            };

            let found = facts.search(&new_order).unwrap();
            let all = facts.tracks(None).unwrap();
            let first = facts.tracks(Some(1)).unwrap();

            assert_eq!(titles(&found), ["Blue Monday", "Ceremony"]);
            assert_eq!(titles(&all), ["Atomic", "Blue Monday", "Ceremony"]);
            assert_eq!(titles(&first), ["Atomic"]);
        });
    }

    #[test]
    fn track_resolves_a_prefix_with_the_errors_of_the_node() {
        with_world(Some(catalog()), |node| {
            let facts = replica_of(node);
            let get = |partial: &str| facts.track(&ContentHash::new(partial));

            assert_eq!(get("cccc").unwrap().title.as_deref(), Some("Atomic"));
            assert_eq!(
                failure_text(get("dddd")),
                "track not found: dddd",
                "unknown"
            );
            assert_eq!(
                failure_text(get("aaaa")),
                format!("ambiguous hash 'aaaa' matches 2 tracks: {BLUE}, {CEREMONY}")
            );
        });
    }

    #[test]
    fn marked_lists_the_tracks_with_a_mark() {
        with_world(Some(catalog()), |node| {
            let facts = replica_of(node);

            let any = facts.marked(&MarkFilter::Any).unwrap();
            let default = facts.marked(&MarkFilter::Only(Mark::Default)).unwrap();

            assert_eq!(titles(&any), ["Blue Monday"]);
            assert_eq!(titles(&default), ["Blue Monday"]);
        });
    }

    #[test]
    fn fact_values_are_the_values_asserted_for_a_type_sorted() {
        with_world(Some(catalog()), |node| {
            let facts = replica_of(node);

            let artists = facts.fact_values(&FactType::new("Artist")).unwrap();
            let none = facts.fact_values(&FactType::new("Label")).unwrap();

            assert_eq!(artists, ["Blondie", "New Order"]);
            assert!(none.is_empty());
        });
    }

    #[test]
    fn facts_of_reads_the_lines_of_one_track_by_prefix() {
        with_world(Some(catalog()), |node| {
            let facts = replica_of(node);

            let of_atomic = facts.facts_of(&ContentHash::new("cccc")).unwrap();
            let unknown = facts.facts_of(&ContentHash::new("dddd"));

            let kinds: Vec<&str> = of_atomic.iter().map(|(kind, _)| kind.as_str()).collect();
            assert_eq!(kinds, ["Title", "Artist"]);
            assert_eq!(of_atomic[0].1, "Atomic");
            assert_eq!(failure_text(unknown), "track not found: dddd");
        });
    }

    #[test]
    fn plays_are_the_replicas_with_the_play_times_applied_to_the_tracks() {
        with_world(Some(catalog()), |node| {
            let facts = replica_of(node);

            let projection = facts.plays().unwrap();
            let played = facts.track(&ContentHash::new("sha256:aaaa000000000001"));

            let histories = projection.plays.histories(&projection.tracks);
            assert_eq!(histories.iter().map(|h| h.count()).sum::<usize>(), 1);
            assert!(
                played.unwrap().started.is_some(),
                "plays applied to the view"
            );
        });
    }

    // --- the note ---------------------------------------------------------------

    fn view(
        synced_at: Option<u64>,
        last_attempt: Option<u64>,
        failures: u32,
        lines: &[&str],
    ) -> ReplicaView {
        let id = NodeId::new("mdma-stage").unwrap();
        let fold = ReplicaFold {
            replica: Replica::new(Path::new("/nonexistent"), &id),
            id,
            synced_at: synced_at.map(secs),
            last_attempt: last_attempt.map(secs),
            failures,
            projection: library_projection::project_lines(lines.iter().copied()),
        };
        ReplicaView::new(fold)
    }

    fn note(view: ReplicaView, now: u64) -> Option<String> {
        Facts::<&Node>::Replica(Box::new(view)).source_note(secs(now))
    }

    #[test]
    fn the_note_names_the_replica_and_its_age() {
        let synced = view(Some(1_000), Some(1_000), 0, &[]);

        assert_eq!(
            note(synced, 1_000 + 7_200).as_deref(),
            Some("replica of mdma-stage, synced 2 h ago")
        );
    }

    #[test]
    fn a_replica_that_never_synced_says_never_and_is_behind_when_it_tried() {
        let untried = view(None, None, 0, &[]);
        let tried = view(None, Some(3), 0, &[]);

        assert_eq!(
            note(untried, 5).as_deref(),
            Some("replica of mdma-stage, synced never")
        );
        assert_eq!(
            note(tried, 5).as_deref(),
            Some("replica of mdma-stage, synced never\nbehind: the last sync attempt failed")
        );
    }

    #[test]
    fn the_note_says_behind_when_an_attempt_came_after_the_sync_or_failures_count() {
        let after = view(Some(1_000), Some(1_060), 0, &[]);
        let failing = view(Some(1_000), Some(1_000), 2, &[]);
        let same = view(Some(1_000), Some(1_000), 0, &[]);
        let behind =
            "replica of mdma-stage, synced 1 min ago\nbehind: the last sync attempt failed";

        assert_eq!(note(after, 1_100).as_deref(), Some(behind));
        assert_eq!(note(failing, 1_100).as_deref(), Some(behind));
        assert!(!note(same, 1_100).unwrap().contains("behind"));
    }

    #[test]
    fn the_note_counts_the_lines_this_mdma_cannot_read() {
        let lines = [r#"["from-a-newer-node"]"#, "garbage"];
        let unreadable = view(Some(1_000), Some(1_000), 0, &lines);

        assert_eq!(
            note(unreadable, 1_005).as_deref(),
            Some("replica of mdma-stage, synced 5 s ago\n2 lines this mdma cannot read; upgrade mdma")
        );
    }

    #[test]
    fn the_node_has_no_note() {
        let dir = tempfile::tempdir().unwrap();
        let node = dead_node(dir.path());

        let facts = Facts::open(&node, FactsChoice::Node).unwrap();

        assert_eq!(facts.source_note(secs(1)), None);
    }

    // --- no replica -------------------------------------------------------------

    /// A library that listens where the node's library socket is, to see whether
    /// anything reaches it.
    fn listening(world: &World) -> nng::Socket {
        let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        let address = format!("ipc://{}/none.sock", world.dir.path().display());
        server.listen(&address).expect("listen");
        server
    }

    #[test]
    fn no_replica_says_how_to_get_one_and_never_asks_the_node() {
        let world = World::new(None);
        let server = listening(&world);
        let node = world.dead_node();

        let refused = Facts::open(&node, FactsChoice::Replica)
            .err()
            .expect("an error");

        assert!(matches!(refused, FactsError::NoReplica(_)), "{refused:?}");
        assert_eq!(
            refused.to_string(),
            "no replica for 'mdma-stage2.local' yet; \
             a background sync creates it, or run `mdma replica sync`"
        );
        assert!(matches!(server.try_recv(), Err(nng::Error::TryAgain)));
    }

    #[test]
    fn reading_the_replica_sends_nothing_to_the_node() {
        let world = World::new(Some(catalog()));
        let server = listening(&world);
        let node = world.dead_node();
        let facts = replica_of(&node);
        let hash = ContentHash::new(ATOMIC);

        facts.search(&TrackQuery::default()).unwrap();
        facts.tracks(None).unwrap();
        facts.track(&hash).unwrap();
        facts.marked(&MarkFilter::Any).unwrap();
        facts.fact_values(&FactType::new("Artist")).unwrap();
        facts.facts_of(&hash).unwrap();

        assert!(matches!(server.try_recv(), Err(nng::Error::TryAgain)));
    }

    #[test]
    fn with_no_node_known_at_all_the_error_says_how_to_choose_one() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _env = offline_env(dir.path());
        let options = NodeOptions::default();
        let node = Node::with_target(options, node_discovery::Target::LocalIpc);

        let refused = Facts::open(&node, FactsChoice::Replica)
            .err()
            .expect("an error");

        assert_eq!(
            refused.to_string(),
            "no node to find a replica for; use --node NAME or run `mdma node select`"
        );
    }

    #[test]
    fn a_named_replica_directory_no_sync_has_filled_is_not_synced() {
        with_world(None, |_| {
            let nodes_dir = crate::replicas_dir().unwrap();
            let id = NodeId::new("stage").unwrap();
            std::fs::create_dir_all(Replica::new(&nodes_dir, &id).dir()).unwrap();
            let node = dead_node_named(&nodes_dir, "stage");

            let refused = Facts::open(&node, FactsChoice::Replica);

            assert_eq!(
                failure_text(refused),
                "no replica of stage yet; a background sync creates it, or run `mdma replica sync`"
            );
        });
    }

    // --- the node arm -----------------------------------------------------------

    #[test]
    fn an_unreachable_node_names_the_replica_as_the_alternative() {
        with_world(Some(catalog()), |node| {
            let facts = Facts::open(node, FactsChoice::Node).expect("lazy: nothing connects");

            let refused = facts.search(&TrackQuery::default()).unwrap_err();

            assert!(
                matches!(refused, FactsError::NodeUnreachable { .. }),
                "{refused:?}"
            );
            let text = refused.to_string();
            assert!(text.starts_with("mdma needs the node: "), "{text}");
            let hint = "\n--facts=replica answers from the local replica (synced ";
            assert!(text.contains(hint) && text.ends_with(" d ago)"), "{text}");
        });
    }

    #[test]
    fn a_node_that_cannot_be_named_is_not_unreachable_and_gets_no_hint() {
        let _world = World::new(Some(catalog()));
        let mut env = EnvVars::default();
        env.set("MDMA_NODE", "mdma.local:notaport");
        let node = Node::needed_by(NeededBy::new("mdma"), NodeOptions::default());
        let facts = Facts::open(&node, FactsChoice::Node).expect("lazy: nothing resolves");

        let refused = facts.search(&TrackQuery::default()).unwrap_err();

        assert!(
            matches!(refused, FactsError::Node(NodeError::InvalidEnv(_))),
            "{refused:?}"
        );
        assert!(
            !refused.to_string().contains("--facts=replica"),
            "{refused}"
        );
    }

    #[test]
    fn an_unreachable_node_with_no_replica_has_no_hint() {
        with_world(None, |node| {
            let facts = Facts::open(node, FactsChoice::Node).unwrap();

            let refused = facts.tracks(None).unwrap_err();

            assert!(
                !refused.to_string().contains("--facts=replica"),
                "{refused}"
            );
        });
    }

    #[test]
    fn plays_from_the_node_is_an_error_that_says_so() {
        with_world(Some(catalog()), |node| {
            let facts = Facts::open(node, FactsChoice::Node).unwrap();

            let refused = facts.plays().err().expect("an error");

            assert_eq!(
                refused.to_string(),
                "`mdma plays` reads only the replica: the node has no plays request yet; drop --facts=node"
            );
        });
    }
}
