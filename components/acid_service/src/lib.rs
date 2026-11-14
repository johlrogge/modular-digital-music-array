//! ACID service — append-only fact store backed by the `fact-store` interface.
//! The active implementation is selected via `[workspace.dependencies]`:
//! - Root workspace: `fact_store_memory` (dev/test default)
//! - Production builds: `fact_store_file` via `profiles/production.profile`

use acid_protocol::{
    cursor_from_offset, line_digest, offset_from_cursor, AcidRequest, AcidResponse, EntityFacts,
    StreamChunk,
};
use event_protocol::{acid_event_to_topic_message, AcidEvent};
use fact_store::FactStorage;
use nng::options::{Options, RecvTimeout, SendTimeout};
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("storage error: {0}")]
    Storage(#[from] fact_store::StorageError),
}

/// Handle returned by `start()`. The server thread runs until this is dropped.
pub struct ServerHandle {
    _shutdown: Arc<()>,
}

/// Start the ACID NNG server in a background thread.
///
/// The caller is responsible for creating and binding the sockets before calling
/// this function. `acid_service` owns the socket timeout settings for correct
/// shutdown and flow control.
///
/// - `rep`: bound NNG Rep0 socket (e.g. listening on `"ipc:///run/mdma/acid.sock"`)
/// - `pub_sock`: bound NNG Pub0 socket (e.g. listening on `"ipc:///run/mdma/acid-events.sock"`)
/// - `metadata_dir`: passed to `FactStorage::new()` (ignored by memory impl)
pub fn start(
    rep: nng::Socket,
    pub_sock: nng::Socket,
    metadata_dir: &Path,
) -> Result<ServerHandle, ServiceError> {
    // acid_service owns these socket settings for correct shutdown and flow control
    rep.set_opt::<RecvTimeout>(Some(Duration::from_secs(1)))?;
    rep.set_opt::<SendTimeout>(Some(Duration::from_secs(5)))?;

    let storage = FactStorage::new(metadata_dir)?;
    tracing::info!("ACID backend: {}", storage.backend_name());
    let replayed = storage.replay_from_file(&metadata_dir.join("facts.jsonl"))?;
    if replayed > 0 {
        tracing::info!("ACID startup: replayed {replayed} facts");
    }
    let storage = Arc::new(storage);
    let epoch = load_or_create_epoch(metadata_dir)?;
    tracing::info!("ACID epoch: {epoch}");

    let shutdown = Arc::new(());
    let shutdown_clone = Arc::clone(&shutdown);

    thread::spawn(move || {
        let _shutdown_signal = shutdown_clone;
        let mut line_count = storage.line_count();
        loop {
            let msg = match rep.recv() {
                Ok(m) => m,
                Err(nng::Error::TimedOut) => {
                    if Arc::strong_count(&_shutdown_signal) == 1 {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            };
            let request: AcidRequest = match serde_json::from_slice(&msg) {
                Ok(r) => r,
                Err(e) => {
                    let response = AcidResponse::Error {
                        message: format!("deserialization error: {e}"),
                    };
                    if let Ok(data) = serde_json::to_vec(&response) {
                        let _ = rep.send(nng::Message::from(&data[..]));
                    }
                    continue;
                }
            };

            let write_entity: Option<String> = match &request {
                AcidRequest::WriteFacts { entity, .. } => Some(entity.clone()),
                AcidRequest::RetractFacts { entity, .. } => Some(entity.clone()),
                _ => None,
            };

            let response = handle_request(&request, &storage, &epoch, line_count);

            match (&write_entity, &response) {
                (Some(entity), AcidResponse::WriteOk { facts_written }) => {
                    line_count += facts_written;
                    let cursor = acid_protocol::cursor_from_offset(line_count);
                    publish_acid_event(
                        &pub_sock,
                        AcidEvent::FactsAsserted {
                            entity: entity.clone(),
                            count: *facts_written,
                            cursor,
                        },
                    );
                }
                (Some(entity), AcidResponse::RetractOk { facts_retracted }) => {
                    line_count += facts_retracted;
                    let cursor = acid_protocol::cursor_from_offset(line_count);
                    publish_acid_event(
                        &pub_sock,
                        AcidEvent::FactsRetracted {
                            entity: entity.clone(),
                            count: *facts_retracted,
                            cursor,
                        },
                    );
                }
                _ => {}
            }

            if let Ok(data) = serde_json::to_vec(&response) {
                let _ = rep.send(nng::Message::from(&data[..]));
            }
        }
    });

    Ok(ServerHandle {
        _shutdown: shutdown,
    })
}

fn with_identity(mut chunk: StreamChunk, epoch: &str, total_lines: usize) -> AcidResponse {
    chunk.epoch = Some(epoch.to_string());
    chunk.total_lines = Some(total_lines);
    AcidResponse::StreamChunk(chunk)
}

/// Read `metadata_dir/facts.epoch`, or create it with a random 128-bit hex id.
///
/// The epoch identifies the fact log: it is stable across restarts and changes
/// only if the file is removed (the log was replaced).
fn load_or_create_epoch(metadata_dir: &Path) -> io::Result<String> {
    let path = metadata_dir.join("facts.epoch");
    match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => return Ok(s.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let epoch: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(metadata_dir)?;
    let tmp = metadata_dir.join("facts.epoch.tmp");
    std::fs::write(&tmp, &epoch)?;
    std::fs::rename(&tmp, &path)?;
    Ok(epoch)
}

fn handle_request(
    request: &AcidRequest,
    storage: &FactStorage,
    epoch: &str,
    total_lines: usize,
) -> AcidResponse {
    match request {
        AcidRequest::Ping => AcidResponse::Pong,

        AcidRequest::WriteFacts { entity, facts } => match storage.write_facts(entity, facts) {
            Ok(facts_written) => AcidResponse::WriteOk { facts_written },
            Err(e) => {
                tracing::error!(error = %e, "Failed to write facts");
                AcidResponse::Error {
                    message: e.to_string(),
                }
            }
        },

        AcidRequest::ReadStream {
            cursor,
            limit,
            expect,
        } => {
            let after_line = cursor.as_deref().and_then(offset_from_cursor).unwrap_or(0);
            let read_error = |e: fact_store::StorageError| {
                tracing::error!(error = %e, "Failed to read stream");
                AcidResponse::Error {
                    message: e.to_string(),
                }
            };
            let Some(pin) = expect else {
                return match storage.read_stream(after_line, *limit) {
                    Ok(chunk) => with_identity(chunk, epoch, total_lines),
                    Err(e) => read_error(e),
                };
            };
            if after_line.checked_sub(1) != Some(pin.line) {
                return AcidResponse::Error {
                    message: format!(
                        "expect.line {} does not match the line before cursor line:{after_line}",
                        pin.line
                    ),
                };
            }
            let mut chunk = match storage.read_stream(pin.line, limit.saturating_add(1)) {
                Ok(chunk) => chunk,
                Err(e) => return read_error(e),
            };
            let pinned_line_matches = chunk
                .lines
                .first()
                .is_some_and(|l| line_digest(l) == pin.sha256);
            if !pinned_line_matches {
                return AcidResponse::StreamDiverged {
                    epoch: epoch.to_string(),
                    total_lines,
                };
            }
            chunk.lines.remove(0);
            chunk.lines.truncate(*limit);
            chunk.cursor = cursor_from_offset(after_line + chunk.lines.len());
            with_identity(chunk, epoch, total_lines)
        }

        AcidRequest::RetractFacts { entity, facts } => match storage.retract_facts(entity, facts) {
            Ok(facts_retracted) => AcidResponse::RetractOk { facts_retracted },
            Err(e) => {
                tracing::error!(error = %e, "Failed to retract facts");
                AcidResponse::Error {
                    message: e.to_string(),
                }
            }
        },

        AcidRequest::ReadEntity { entity } => match storage.read_entity(entity) {
            Ok(lines) => AcidResponse::EntityFacts(EntityFacts { lines }),
            Err(e) => {
                tracing::error!(error = %e, "Failed to read entity");
                AcidResponse::Error {
                    message: e.to_string(),
                }
            }
        },
    }
}

fn publish_acid_event(pub_sock: &nng::Socket, event: AcidEvent) {
    let bytes = acid_event_to_topic_message(&event);
    if let Err((_, e)) = pub_sock.send(nng::Message::from(&bytes[..])) {
        tracing::warn!(error = %e, "Failed to publish ACID event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_protocol::{line_digest, FactEntry, LinePin};

    fn write_n(storage: &FactStorage, n: usize) {
        for i in 0..n {
            let facts = [FactEntry {
                value_json: format!("\"v{i}\""),
                source_json: r#""s""#.to_string(),
            }];
            storage.write_facts(&format!("entity:{i}"), &facts).unwrap();
        }
    }

    fn read(
        storage: &FactStorage,
        cursor: Option<&str>,
        limit: usize,
        expect: Option<LinePin>,
    ) -> AcidResponse {
        let req = AcidRequest::ReadStream {
            cursor: cursor.map(str::to_string),
            limit,
            expect,
        };
        handle_request(&req, storage, "epoch-1", storage.line_count())
    }

    fn pin_of(storage: &FactStorage, line: usize) -> LinePin {
        let AcidResponse::StreamChunk(c) = read(storage, None, 1000, None) else {
            panic!("expected chunk");
        };
        LinePin {
            line,
            sha256: line_digest(&c.lines[line]),
        }
    }

    #[test]
    fn epoch_is_stable_across_loads_of_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_epoch(dir.path()).unwrap();
        let second = load_or_create_epoch(dir.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn epoch_differs_between_dirs() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(
            load_or_create_epoch(a.path()).unwrap(),
            load_or_create_epoch(b.path()).unwrap()
        );
    }

    #[test]
    fn unpinned_read_carries_epoch_and_total() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FactStorage::new(dir.path()).unwrap();
        write_n(&storage, 3);
        let AcidResponse::StreamChunk(c) = read(&storage, None, 2, None) else {
            panic!("expected chunk");
        };
        assert_eq!(c.lines.len(), 2);
        assert_eq!(c.cursor, "line:2");
        assert_eq!(c.epoch.as_deref(), Some("epoch-1"));
        assert_eq!(c.total_lines, Some(3));
    }

    #[test]
    fn correct_pin_returns_following_lines() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FactStorage::new(dir.path()).unwrap();
        write_n(&storage, 5);
        let pin = pin_of(&storage, 1);
        let AcidResponse::StreamChunk(c) = read(&storage, Some("line:2"), 2, Some(pin)) else {
            panic!("expected chunk");
        };
        assert_eq!(c.lines.len(), 2);
        assert!(c.lines[0].contains("entity:2"));
        assert!(c.lines[1].contains("entity:3"));
        assert_eq!(c.cursor, "line:4");
        assert_eq!(c.total_lines, Some(5));
    }

    #[test]
    fn correct_pin_at_end_returns_empty_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FactStorage::new(dir.path()).unwrap();
        write_n(&storage, 2);
        let pin = pin_of(&storage, 1);
        let AcidResponse::StreamChunk(c) = read(&storage, Some("line:2"), 10, Some(pin)) else {
            panic!("expected chunk");
        };
        assert!(c.lines.is_empty());
        assert_eq!(c.cursor, "line:2");
    }

    #[test]
    fn wrong_digest_is_diverged() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FactStorage::new(dir.path()).unwrap();
        write_n(&storage, 3);
        let pin = LinePin {
            line: 1,
            sha256: line_digest("something else"),
        };
        match read(&storage, Some("line:2"), 10, Some(pin)) {
            AcidResponse::StreamDiverged { epoch, total_lines } => {
                assert_eq!(epoch, "epoch-1");
                assert_eq!(total_lines, 3);
            }
            other => panic!("expected StreamDiverged, got {other:?}"),
        }
    }

    #[test]
    fn pin_past_end_is_diverged() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FactStorage::new(dir.path()).unwrap();
        write_n(&storage, 2);
        let pin = LinePin {
            line: 5,
            sha256: line_digest("gone"),
        };
        assert!(matches!(
            read(&storage, Some("line:6"), 10, Some(pin)),
            AcidResponse::StreamDiverged { total_lines: 2, .. }
        ));
    }

    #[test]
    fn pin_not_matching_cursor_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FactStorage::new(dir.path()).unwrap();
        write_n(&storage, 3);
        let pin = pin_of(&storage, 0);
        assert!(matches!(
            read(&storage, Some("line:2"), 10, Some(pin)),
            AcidResponse::Error { .. }
        ));
    }
}
