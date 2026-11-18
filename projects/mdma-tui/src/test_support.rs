//! Fixtures for tests that need a node that is not there, or one that is, and a
//! replica that is.

use crate::link::Link;
use client_facts::test_support::asserted;
use client_facts::FactsChoice;
use library_ipc_protocol::{LibraryRequest, LibraryResponse, PlaylistEntry, ProtocolError};
use mdma_client::node::{Answer, NodeEffects};
use mdma_client::{
    CanonicalHash, ContentHash, Node, NodeOptions, RemoteTarget, Response, ResponseData, Source,
    Target,
};
use music_facts::{Album, Artist, MusicValue, Title};
use nng::options::{Options, RecvTimeout};
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const BLUE: &str = "sha256:aaaa000000000001";
pub(crate) const CEREMONY: &str = "sha256:aaaa000000000002";
pub(crate) const ATOMIC: &str = "sha256:cccc000000000003";
const FOURTH: &str = "sha256:dddd000000000004";
const AT: &str = "2026-01-02T10:00:00Z";

fn fact(entity: &str, value: &MusicValue) -> String {
    asserted(entity, &serde_json::to_string(value).unwrap(), AT)
}

fn genre(entity: &str, name: &str) -> String {
    fact(entity, &MusicValue::MainGenre(name.to_string()))
}

/// Three tracks: two by New Order (synthpop), one by Blondie (new wave).
pub(crate) fn catalog() -> Vec<String> {
    let title = |t: &str| MusicValue::Title(Title::new(t));
    let artist = |a: &str| MusicValue::Artist(Artist::new(a));
    vec![
        fact(BLUE, &title("Blue Monday")),
        fact(BLUE, &artist("New Order")),
        fact(BLUE, &MusicValue::Album(Album::new("Power"))),
        genre(BLUE, "Synthpop"),
        fact(CEREMONY, &title("Ceremony")),
        fact(CEREMONY, &artist("New Order")),
        genre(CEREMONY, "Synthpop"),
        fact(ATOMIC, &title("Atomic")),
        fact(ATOMIC, &artist("Blondie")),
        genre(ATOMIC, "New Wave"),
    ]
}

/// [`catalog`] and a fourth track, as a later sync would have it.
pub(crate) fn catalog_with_a_fourth() -> Vec<String> {
    let mut lines = catalog();
    lines.push(fact(FOURTH, &MusicValue::Title(Title::new("Fourth"))));
    lines
}

/// [`client_facts::test_support::World`] with what the TUI tests need of a node.
pub(crate) struct World(client_facts::test_support::World);

impl std::ops::Deref for World {
    type Target = client_facts::test_support::World;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl World {
    pub(crate) fn new(lines: Option<Vec<String>>) -> Self {
        World(client_facts::test_support::World::new(lines))
    }

    /// A node with no service behind it: using it fails.
    pub(crate) fn dead_node(&self) -> Rc<Node> {
        Rc::new(self.0.dead_node())
    }

    /// A node whose pick prompt the user cancelled.
    pub(crate) fn cancelled_node(&self) -> Rc<Node> {
        Rc::new(self.0.cancelled_node())
    }

    /// The facts of the replica, and a dead node.
    pub(crate) fn link(&self) -> Link {
        Link::new(self.dead_node(), FactsChoice::Replica)
    }
}

/// A library and a playback service at the sockets of a [`World`], answering what the
/// TUI asks of the node with tracks titled "Node ...". They stop when this is dropped.
pub(crate) struct Services {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// The full hash a queue and a playlist hold in the fake node.
pub(crate) fn node_hash() -> ContentHash {
    ContentHash::new(format!("sha256:{}", "c".repeat(64)))
}

impl Services {
    pub(crate) fn start(world: &World) -> Self {
        let options = world.options();
        let stop = Arc::new(AtomicBool::new(false));
        let library = serve(options.library_socket.unwrap(), &stop, answer_library);
        let playback = serve(options.playback_socket.unwrap(), &stop, answer_playback);
        Services {
            stop,
            threads: vec![library, playback],
        }
    }
}

impl Drop for Services {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn serve(
    address: String,
    stop: &Arc<AtomicBool>,
    answer: fn(&[u8]) -> Vec<u8>,
) -> std::thread::JoinHandle<()> {
    let server = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
    server
        .set_opt::<RecvTimeout>(Some(Duration::from_millis(20)))
        .expect("receive timeout");
    server.listen(&address).expect("listen");
    let stop = Arc::clone(stop);
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            if let Ok(request) = server.recv() {
                let reply = nng::Message::from(&answer(&request)[..]);
                let _ = server.send(reply);
            }
        }
    })
}

fn answer_library(request: &[u8]) -> Vec<u8> {
    let response = match serde_json::from_slice::<LibraryRequest>(request) {
        Ok(LibraryRequest::GetTrack { hash }) => {
            let track =
                crate::rows::make_track_info(hash.as_str(), "Node Artist", "Node Title", 60);
            LibraryResponse::Track(Box::new(track))
        }
        Ok(LibraryRequest::PlaylistEntries { .. }) => {
            let hash = CanonicalHash::try_from(node_hash()).expect("a full hash");
            LibraryResponse::PlaylistEntries(vec![PlaylistEntry::Track { hash }])
        }
        other => LibraryResponse::Error(ProtocolError::Internal {
            message: format!("not served by the fake node: {other:?}"),
        }),
    };
    serde_json::to_vec(&response).unwrap()
}

fn answer_playback(request: &[u8]) -> Vec<u8> {
    let data = match serde_json::from_slice::<mdma_client::Command>(request) {
        Ok(mdma_client::Command::QueueList) => Some(ResponseData::Queue(vec![node_hash()])),
        _ => None,
    };
    serde_json::to_vec(&Response::Ok { data }).unwrap()
}

/// A remote node whose answers to "who are you" and whose clock the test controls, with
/// a service listening at its gateway address.
pub(crate) struct ScriptedNode {
    pub(crate) node: Rc<Node>,
    answers: Arc<AtomicBool>,
    clock: Arc<Mutex<Instant>>,
    _gateway: nng::Socket,
}

impl ScriptedNode {
    /// A node that does not answer yet.
    pub(crate) fn silent() -> Self {
        let gateway = nng::Socket::new(nng::Protocol::Rep0).expect("server socket");
        let listener = nng::ListenerBuilder::new(&gateway, "tcp://127.0.0.1:0")
            .expect("listener")
            .start()
            .map_err(|(_, e)| e)
            .expect("listen");
        let port = listener
            .get_opt::<nng::options::transport::tcp::BoundPort>()
            .expect("bound port");
        let answers = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(Mutex::new(Instant::now()));
        let effects = NodeEffects {
            now: {
                let clock = Arc::clone(&clock);
                Box::new(move || *clock.lock().unwrap())
            },
            resolve: Box::new(|_, _, _, _| unreachable!("the node is resolved")),
            probe: {
                let answers = Arc::clone(&answers);
                Box::new(move |_| {
                    if answers.load(Ordering::SeqCst) {
                        Ok(Answer::Failed("not asked".to_string()))
                    } else {
                        Err("did not answer".to_string())
                    }
                })
            },
        };
        let target = Target::Remote(RemoteTarget {
            host: "localhost".to_string(),
            ip: Some(Ipv4Addr::LOCALHOST),
            port,
            source: Source::Cache,
        });
        let node = Node::with_target(NodeOptions::default(), target).with_effects(effects);
        ScriptedNode {
            node: Rc::new(node),
            answers,
            clock,
            _gateway: gateway,
        }
    }

    /// From now on the node answers.
    pub(crate) fn answer(&self) {
        self.answers.store(true, Ordering::SeqCst);
    }

    /// Let `by` pass.
    pub(crate) fn advance(&self, by: Duration) {
        *self.clock.lock().unwrap() += by;
    }
}
