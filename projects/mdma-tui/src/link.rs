//! How the TUI reaches the node and reads facts (ADR-030 §2, §4, §5).
//!
//! Two things are kept apart. Facts for the browser and search (the (a) reads) come
//! from the source `--facts` chose, held open for the whole run. Everything that
//! feeds a node write or serves a node-only pane (the queue, the playlists, the row
//! metadata they show) is read from the node, whatever `--facts` says. The node is
//! reached lazily, on first use, and a node that cannot be reached is a state the
//! panes show, never a reason to exit.
//!
//! A [`Link`] is cheap to clone: the panes share one node and one fold.

use client_facts::{Facts, FactsChoice, FactsError, UnixSecs};
use mdma_client::{
    ContentHash, LibraryBackend, Node, NodeError, NodeSpec, PlaybackBackend, ProtocolError,
    TrackInfo, TrackQuery,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What the user is told when they cancelled the pick prompt.
const NO_NODE_SELECTED: &str = "no node selected; run `mdma node select`";

/// Why the node could not be used.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The user cancelled the pick prompt: they chose no node, for the whole run.
    #[error("{NO_NODE_SELECTED}")]
    NoNodeSelected,
    /// The node cannot be resolved or reached; the text names what needed it.
    #[error("{0}")]
    Node(NodeError),
}

impl From<NodeError> for LinkError {
    fn from(error: NodeError) -> Self {
        if error.is_cancelled() {
            LinkError::NoNodeSelected
        } else {
            LinkError::Node(error)
        }
    }
}

/// What the TUI last learned about the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    Reached,
    /// The pick prompt was cancelled.
    NoneSelected,
    Unreachable,
}

impl NodeState {
    /// What the status line says about the node; nothing when it is fine.
    fn text(self) -> Option<&'static str> {
        match self {
            NodeState::Reached => None,
            NodeState::NoneSelected => Some("no node selected"),
            NodeState::Unreachable => Some("node unreachable"),
        }
    }

    /// What a failed read from the facts says about the node, if it is about the
    /// node at all. The one place that classifies a failure.
    fn after(error: &FactsError) -> Option<Self> {
        match error {
            FactsError::NodeUnreachable { cause, .. } | FactsError::Node(cause) => {
                Some(Self::from(cause))
            }
            FactsError::Transport(error) if error.is_unreachable() => Some(NodeState::Unreachable),
            _ => None,
        }
    }
}

impl From<&NodeError> for NodeState {
    fn from(error: &NodeError) -> Self {
        if error.is_cancelled() {
            NodeState::NoneSelected
        } else {
            NodeState::Unreachable
        }
    }
}

impl<T> From<&Result<T, LinkError>> for NodeState {
    fn from(result: &Result<T, LinkError>) -> Self {
        match result {
            Ok(_) => NodeState::Reached,
            Err(LinkError::NoNodeSelected) => NodeState::NoneSelected,
            Err(LinkError::Node(_)) => NodeState::Unreachable,
        }
    }
}

/// Why facts could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The facts could not be opened (no replica, say); the text says how to fix it.
    #[error("{0}")]
    Closed(Rc<FactsError>),
    #[error(transparent)]
    Facts(#[from] FactsError),
}

impl ReadError {
    /// Whether the answer is "there is no such track", as opposed to not being able
    /// to ask.
    pub fn is_track_not_found(&self) -> bool {
        matches!(
            self,
            ReadError::Facts(FactsError::Library(ProtocolError::TrackNotFound { .. }))
        )
    }
}

/// The facts as opened, or why they could not be.
type FactsSlot = Result<Facts<Rc<Node>>, Rc<FactsError>>;

/// The node and the facts of one TUI run.
#[derive(Clone)]
pub struct Link {
    node: Rc<Node>,
    choice: FactsChoice,
    facts: Rc<RefCell<FactsSlot>>,
    state: Rc<Cell<NodeState>>,
}

impl Link {
    /// Resolve the node and probe it once, then open the facts `choice` names. Call
    /// it before raw mode, so the pick prompt still works: the probe of a remote
    /// node takes at most 2 s. A node that cannot be chosen or reached is the node's
    /// state, not the end of the TUI (it is tried again when something needs it),
    /// and facts that cannot be opened are shown by the panes that read them. The
    /// replica is folded now.
    pub fn new(node: Rc<Node>, choice: FactsChoice) -> Self {
        // Resolving first: the pick may remember a node, and the replica read is
        // that node's.
        let reached = node.gateway().map(|_| ()).map_err(LinkError::from);
        let state = NodeState::from(&reached);
        let facts = Facts::open(Rc::clone(&node), choice).map_err(Rc::new);
        Self {
            node,
            choice,
            facts: Rc::new(RefCell::new(facts)),
            state: Rc::new(Cell::new(state)),
        }
    }

    /// Where to subscribe to the node's events, once the node is resolved (whether it
    /// answers or not: a subscriber connects, and reconnects, by itself). Never
    /// resolves, so it is cheap to ask on every tick.
    pub fn event_address(&self) -> Option<String> {
        self.node.event_address()
    }

    /// The `--node` the TUI was given, if any.
    pub fn node_flag(&self) -> Option<&NodeSpec> {
        self.node.node_flag()
    }

    /// Whether the node was last found to answer.
    pub fn node_reached(&self) -> bool {
        self.state.get() == NodeState::Reached
    }

    /// Record what `result` says about the node, and say it in the TUI's terms.
    fn noted<T>(&self, result: Result<T, NodeError>) -> Result<T, LinkError> {
        let result = result.map_err(LinkError::from);
        self.state.set(NodeState::from(&result));
        result
    }

    /// The library, for what only the node does (playlists, marks, resolving for
    /// playback).
    pub fn library(&self) -> Result<LibraryBackend, LinkError> {
        self.noted(self.node.library())
    }

    /// Playback, for the queue and the transport.
    pub fn playback(&self) -> Result<PlaybackBackend, LinkError> {
        self.noted(self.node.playback())
    }

    /// The node as a source of facts, whatever `--facts` says: for what feeds a node
    /// write or serves a node-only pane (ADR-030 §5), such as the rows of the queue
    /// and of a playlist, which must be what the node has and never what a replica
    /// remembers.
    pub fn node_facts(&self) -> Facts<Rc<Node>> {
        Facts::Node(Rc::clone(&self.node))
    }

    /// The answer of a read, and what it says about the node. A read from the node
    /// that succeeded is the node answering; one from the replica says nothing of it.
    fn settled<T>(&self, result: Result<T, FactsError>, from_node: bool) -> Result<T, ReadError> {
        match result {
            Ok(value) => {
                if from_node {
                    self.state.set(NodeState::Reached);
                }
                Ok(value)
            }
            Err(error) => {
                if let Some(state) = NodeState::after(&error) {
                    self.state.set(state);
                }
                Err(ReadError::Facts(error))
            }
        }
    }

    fn read<T>(
        &self,
        read: impl FnOnce(&Facts<Rc<Node>>) -> Result<T, FactsError>,
    ) -> Result<T, ReadError> {
        let slot = self.facts.borrow();
        let facts = slot
            .as_ref()
            .map_err(|error| ReadError::Closed(Rc::clone(error)))?;
        self.settled(read(facts), matches!(facts, Facts::Node(_)))
    }

    /// Tracks matching `query`, in catalog order.
    pub fn search(&self, query: &TrackQuery) -> Result<Vec<TrackInfo>, ReadError> {
        self.read(|facts| facts.search(query))
    }

    /// All visible tracks, in catalog order. Genre comes with each track.
    pub fn tracks(&self) -> Result<Vec<TrackInfo>, ReadError> {
        self.read(|facts| facts.tracks(None))
    }

    /// The track `hash` names, from the node: the metadata of a row of the queue or
    /// of a playlist.
    pub fn node_track(&self, hash: &ContentHash) -> Result<TrackInfo, ReadError> {
        self.settled(self.node_facts().track(hash), true)
    }

    /// Read the facts again (the replica folds anew). A view that cannot be read
    /// again stays as it was; one that was closed says why it still is.
    pub fn refold(&self) -> Result<(), ReadError> {
        match Facts::open(Rc::clone(&self.node), self.choice) {
            Ok(facts) => {
                *self.facts.borrow_mut() = Ok(facts);
                Ok(())
            }
            Err(error) => {
                let mut slot = self.facts.borrow_mut();
                let error = Rc::new(error);
                if slot.is_err() {
                    *slot = Err(Rc::clone(&error));
                }
                Err(ReadError::Closed(error))
            }
        }
    }

    /// What the status line says: where the facts come from and how old they are
    /// (one part per line of the source note), then what is wrong with the node.
    pub fn status_text(&self) -> Option<String> {
        let note = match &*self.facts.borrow() {
            Ok(facts) => facts.source_note(UnixSecs::now()),
            Err(_) => None,
        };
        let parts: Vec<&str> = note
            .iter()
            .flat_map(|note| note.lines())
            .chain(self.state.get().text())
            .collect();
        (!parts.is_empty()).then(|| parts.join("  ·  "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{catalog, catalog_with_a_fourth, node_hash, Services, World};
    use crate::test_support::{ScriptedNode, ATOMIC};
    use std::time::Duration;

    fn text_of<T>(result: Result<T, impl std::fmt::Display>) -> String {
        match result {
            Ok(_) => panic!("an error"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn a_dead_node_does_not_stop_facts_from_being_read() {
        let world = World::new(Some(catalog()));
        let link = world.link();

        let tracks = link.tracks().unwrap();
        let reached = link.library();

        assert_eq!(tracks.len(), 3);
        assert!(text_of(reached).contains("needs the node"));
    }

    #[test]
    fn a_node_that_fails_is_unreachable_in_the_status_line() {
        let world = World::new(Some(catalog()));
        let link = world.link();

        let before = link.status_text().unwrap();
        link.playback().err().expect("the node is dead");
        let after = link.status_text().unwrap();

        assert!(!before.contains("unreachable"), "{before}");
        assert!(after.starts_with("replica of stage, synced "), "{after}");
        assert!(after.ends_with("node unreachable"), "{after}");
    }

    #[test]
    fn a_cancelled_pick_prompt_is_no_node_selected_and_is_not_asked_again() {
        let world = World::new(Some(catalog()));
        let link = Link::new(world.cancelled_node(), FactsChoice::Replica);

        let at_start = link.status_text().unwrap();
        let library = link.library().err().expect("still cancelled");

        assert!(at_start.ends_with("no node selected"), "{at_start}");
        assert!(matches!(library, LinkError::NoNodeSelected), "{library:?}");
        assert_eq!(
            library.to_string(),
            "no node selected; run `mdma node select`"
        );
        assert_eq!(link.event_address(), None, "no node, no events");
        assert!(link.tracks().is_ok(), "the facts do not need the node");
    }

    #[test]
    fn facts_that_cannot_be_opened_say_how_to_get_a_replica() {
        let world = World::new(None);
        let link = world.link();

        let refused = link.tracks().unwrap_err();

        assert!(matches!(refused, ReadError::Closed(_)));
        assert!(
            refused.to_string().contains("`mdma replica sync`"),
            "{refused}"
        );
        assert_eq!(link.status_text(), None);
    }

    #[test]
    fn an_unknown_track_is_not_found_and_other_failures_are_not() {
        let world = World::new(Some(catalog()));
        let link = world.link();

        let unknown = link
            .read(|facts| facts.track(&ContentHash::new("dddd")))
            .unwrap_err();
        let closed = ReadError::Closed(Rc::new(FactsError::NoCacheDir));

        assert!(unknown.is_track_not_found(), "{unknown}");
        assert!(!closed.is_track_not_found());
    }

    #[test]
    fn refolding_picks_up_what_a_sync_added() {
        let world = World::new(Some(catalog()));
        let link = world.link();
        assert_eq!(link.tracks().unwrap().len(), 3);

        world.sync(catalog_with_a_fourth());
        link.refold().unwrap();

        assert_eq!(link.tracks().unwrap().len(), 4);
    }

    /// The rows of the queue and of a playlist are the node's, whatever the replica
    /// holds: with no replica at all, and the node up, they load.
    #[test]
    fn node_rows_come_from_the_node_when_there_is_no_replica() {
        let world = World::new(None);
        let _services = Services::start(&world);
        let link = world.link();

        let track = link.node_track(&node_hash()).unwrap();

        assert_eq!(track.title.as_deref(), Some("Node Title"));
        assert!(link.tracks().is_err(), "the browser has no replica to read");
        assert_eq!(link.status_text(), None, "and the node is fine");
    }

    /// A row read from a replica that has the track is not what a node row shows.
    #[test]
    fn node_rows_are_not_read_from_the_replica() {
        let world = World::new(Some(catalog()));
        let link = world.link();

        let from_replica = link.tracks().unwrap();
        let from_node = link.node_track(&ContentHash::new(ATOMIC));

        assert_eq!(from_replica.len(), 3);
        assert!(text_of(from_node).contains("needs the node"));
    }

    #[test]
    fn a_node_read_that_succeeds_recovers_the_status_line_and_a_replica_read_does_not() {
        let world = World::new(Some(catalog()));
        let link = Link::new(world.dead_node(), FactsChoice::Replica);
        assert!(link.node_track(&node_hash()).is_err());
        assert!(!link.node_reached());

        link.tracks().unwrap();
        assert!(!link.node_reached(), "the replica says nothing of the node");

        let _services = Services::start(&world);
        link.node_track(&node_hash()).unwrap();
        assert!(link.node_reached());
        assert!(!link.status_text().unwrap().contains("unreachable"));
    }

    #[test]
    fn a_failed_read_is_classified_once_by_what_it_says_about_the_node() {
        let cancelled = NodeError::Needed {
            needed_by: mdma_client::NeededBy::new("mdma-tui"),
            cause: Box::new(NodeError::Resolve(node_discovery::ResolveError::Cancelled)),
        };
        let down = NodeError::Connect("down".to_string());

        assert_eq!(NodeState::from(&cancelled), NodeState::NoneSelected);
        assert_eq!(NodeState::from(&down), NodeState::Unreachable);
        assert_eq!(
            NodeState::after(&FactsError::Node(down)),
            Some(NodeState::Unreachable)
        );
        assert_eq!(NodeState::after(&FactsError::NoCacheDir), None);
        assert_eq!(NodeState::after(&FactsError::PlaysFromNode), None);
    }

    /// The address of events is there once the node is resolved, whether it answers
    /// or not: a subscriber does not wait for the node, so nothing is checked.
    #[test]
    fn the_address_of_events_is_there_whether_or_not_the_node_answers() {
        let _world = World::new(Some(catalog()));
        let scripted = ScriptedNode::silent();
        let link = Link::new(Rc::clone(&scripted.node), FactsChoice::Replica);
        assert!(!link.node_reached(), "the node did not answer at startup");

        let silent = link.event_address().expect("the node is resolved");
        scripted.answer();
        scripted.advance(Duration::from_secs(10));
        assert!(link.library().is_ok());

        assert!(link.node_reached());
        assert!(silent.starts_with("tcp://127.0.0.1:"), "{silent}");
        assert_eq!(
            link.event_address(),
            Some(silent),
            "the same, answering or not"
        );
    }

    /// Local services have no events to subscribe to.
    #[test]
    fn local_services_have_no_address_of_events() {
        let world = World::new(Some(catalog()));

        assert_eq!(world.link().event_address(), None);
    }
}
