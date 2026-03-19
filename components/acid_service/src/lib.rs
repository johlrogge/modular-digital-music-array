//! ACID service component — NNG server loop backed by swappable fact storage.
//!
//! `fact-store` resolves to `fact_store_memory` in the dev workspace and
//! `fact_store_file` in the `projects/mdma-acid` standalone workspace.

use acid_protocol::{offset_from_cursor, AcidRequest, AcidResponse};
use event_protocol::{acid_event_to_topic_message, AcidEvent};
use fact_store::FactStorage;
use nng::options::{Options, RecvTimeout, SendTimeout};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),
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
/// - `facts_addr`: NNG Rep0 address (e.g. `"ipc:///run/mdma/acid.sock"`)
/// - `events_addr`: NNG Pub0 address (e.g. `"ipc:///run/mdma/acid-events.sock"`)
/// - `metadata_dir`: passed to `FactStorage::new()` (ignored by memory impl)
pub fn start(
    facts_addr: &str,
    events_addr: &str,
    metadata_dir: &Path,
) -> Result<ServerHandle, ServiceError> {
    let storage = FactStorage::new(metadata_dir)?;
    let storage = Arc::new(storage);

    let rep = nng::Socket::new(nng::Protocol::Rep0)?;
    rep.set_opt::<SendTimeout>(Some(Duration::from_secs(5)))?;
    rep.set_opt::<RecvTimeout>(Some(Duration::from_secs(1)))?;
    rep.listen(facts_addr)?;

    let pub_sock = nng::Socket::new(nng::Protocol::Pub0)?;
    pub_sock.listen(events_addr)?;

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
                _ => None,
            };

            let response = handle_request(&request, &storage);

            if let (Some(entity), AcidResponse::WriteOk { facts_written }) =
                (&write_entity, &response)
            {
                line_count += facts_written;
                let cursor = acid_protocol::cursor_from_offset(line_count);
                publish_facts_written(&pub_sock, entity, *facts_written, &cursor);
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

fn handle_request(request: &AcidRequest, storage: &FactStorage) -> AcidResponse {
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

        AcidRequest::ReadStream { cursor, limit } => {
            let after_line = cursor.as_deref().and_then(offset_from_cursor).unwrap_or(0);
            match storage.read_stream(after_line, *limit) {
                Ok(chunk) => AcidResponse::StreamChunk(chunk),
                Err(e) => {
                    tracing::error!(error = %e, "Failed to read stream");
                    AcidResponse::Error {
                        message: e.to_string(),
                    }
                }
            }
        }
    }
}

fn publish_facts_written(pub_sock: &nng::Socket, entity: &str, count: usize, cursor: &str) {
    let event = AcidEvent::FactsWritten {
        entity: entity.to_string(),
        count,
        cursor: cursor.to_string(),
    };
    let bytes = acid_event_to_topic_message(&event);
    if let Err((_, e)) = pub_sock.send(nng::Message::from(&bytes[..])) {
        tracing::warn!(error = %e, "Failed to publish acid/facts event");
    }
}
