//! In-process NNG playback simulator for BDD tests.
//!
//! Implements the media_protocol Command/Response contract with an in-memory
//! queue. No PipeWire, no audio hardware — just queue management and now_playing state.

use media_protocol::{Command, ContentHash, Response, ResponseData};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Shared state for the playback simulator.
#[derive(Debug, Default)]
pub struct PlaybackState {
    pub queue: VecDeque<(ContentHash, PathBuf)>,
    pub now_playing: Option<ContentHash>,
}

/// Start a playback simulator on the given NNG IPC address.
/// Returns the shared state (for assertions) and a join handle.
pub fn start_playback_simulator(
    address: &str,
) -> (Arc<Mutex<PlaybackState>>, std::thread::JoinHandle<()>) {
    let state = Arc::new(Mutex::new(PlaybackState::default()));
    let state_clone = Arc::clone(&state);
    let addr = address.to_string();

    let handle = std::thread::spawn(move || {
        run_simulator(&addr, &state_clone);
    });

    // Give the server a moment to bind
    std::thread::sleep(std::time::Duration::from_millis(50));

    (state, handle)
}

fn run_simulator(address: &str, state: &Arc<Mutex<PlaybackState>>) {
    let socket = nng::Socket::new(nng::Protocol::Rep0).expect("failed to create nng socket");
    socket.listen(address).expect("failed to listen on address");

    loop {
        let msg = match socket.recv() {
            Ok(m) => m,
            Err(_) => break,
        };

        let cmd: Command = match serde_json::from_slice(&msg) {
            Ok(c) => c,
            Err(e) => {
                let resp = Response {
                    success: false,
                    error_message: format!("Failed to parse command: {}", e),
                    data: None,
                };
                let _ = send_response(&socket, &resp);
                continue;
            }
        };

        let resp = handle_command(cmd, state);
        let _ = send_response(&socket, &resp);
    }
}

fn send_response(socket: &nng::Socket, resp: &Response) -> Result<(), nng::Error> {
    let data = serde_json::to_vec(resp).expect("failed to serialize response");
    let msg = nng::Message::from(&data[..]);
    socket.send(msg).map_err(|(_, e)| e)
}

fn handle_command(cmd: Command, state: &Arc<Mutex<PlaybackState>>) -> Response {
    let mut s = state.lock().unwrap();

    match cmd {
        Command::QueueAppend { hash, path } => {
            s.queue.push_back((hash, path));
            ok_response()
        }
        Command::QueueNext { hash, path } => {
            s.queue.push_front((hash, path));
            ok_response()
        }
        Command::QueueList => {
            let hashes: Vec<ContentHash> = s.queue.iter().map(|(h, _)| h.clone()).collect();
            Response {
                success: true,
                error_message: String::new(),
                data: Some(ResponseData::Queue(hashes)),
            }
        }
        Command::QueueClear => {
            s.queue.clear();
            ok_response()
        }
        Command::QueueRemove { hashes } => {
            let before = s.queue.len();
            s.queue.retain(|(h, _)| !hashes.contains(h));
            let removed = before - s.queue.len();
            Response {
                success: true,
                error_message: String::new(),
                data: Some(ResponseData::Count(removed)),
            }
        }
        Command::QueueReplace { entries } => {
            s.queue = entries.into_iter().collect();
            ok_response()
        }
        Command::PlayQueue => {
            if let Some((hash, _path)) = s.queue.pop_front() {
                s.now_playing = Some(hash);
                ok_response()
            } else {
                Response {
                    success: false,
                    error_message: "Queue is empty".to_string(),
                    data: None,
                }
            }
        }
        Command::Stop { .. } => {
            s.now_playing = None;
            ok_response()
        }
        Command::NowPlaying => Response {
            success: true,
            error_message: String::new(),
            data: Some(ResponseData::NowPlaying(s.now_playing.clone())),
        },
        _ => Response {
            success: false,
            error_message: format!("Unsupported command in simulator: {:?}", cmd),
            data: None,
        },
    }
}

fn ok_response() -> Response {
    Response {
        success: true,
        error_message: String::new(),
        data: None,
    }
}
