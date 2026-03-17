use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use acid_protocol::{
    cursor_from_offset, offset_from_cursor, AcidRequest, AcidResponse, StreamChunk,
};
use chrono::Utc;
use clap::Parser;
use color_eyre::Result;
use event_protocol::{acid_event_to_topic_message, AcidEvent};
use service::{ServiceConfig, ServiceSockets};
use stainless_facts::{Fact, FactStreamWriter, Operation};
use thiserror::Error;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MDMA ACID - Append-only fact stream writer service"
)]
struct Args {
    /// Path to metadata directory (facts.jsonl will be written here)
    #[arg(long, default_value = "/metadata")]
    metadata_dir: PathBuf,

    /// nng IPC socket path (Rep0 — request/response)
    #[arg(long, default_value = "ipc:///run/mdma/acid.sock")]
    socket: String,

    /// nng IPC socket path for event publishing (Pub0)
    #[arg(long, default_value = "ipc:///run/mdma/acid-events.sock")]
    event_socket: String,
}

#[derive(Debug, Error)]
enum AcidError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Fact write error: {0}")]
    FactWrite(#[from] stainless_facts::WriteError),
}

fn handle_request(request: &AcidRequest, metadata_dir: &Path) -> AcidResponse {
    match request {
        AcidRequest::Ping => AcidResponse::Pong,

        AcidRequest::WriteFacts { entity, facts } => {
            match write_facts(entity, facts, metadata_dir) {
                Ok(facts_written) => AcidResponse::WriteOk { facts_written },
                Err(e) => {
                    tracing::error!(error = %e, "Failed to write facts");
                    AcidResponse::Error {
                        message: e.to_string(),
                    }
                }
            }
        }

        AcidRequest::ReadStream { cursor, limit } => {
            let after_line = cursor.as_deref().and_then(offset_from_cursor).unwrap_or(0);
            match read_stream(after_line, *limit, metadata_dir) {
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

fn write_facts(
    entity: &str,
    facts: &[acid_protocol::FactEntry],
    metadata_dir: &Path,
) -> Result<usize, AcidError> {
    let facts_path = metadata_dir.join("facts.jsonl");
    let now = Utc::now();

    let fact_structs: Vec<Fact<String, serde_json::Value, serde_json::Value>> = facts
        .iter()
        .map(|entry| {
            let value: serde_json::Value = serde_json::from_str(&entry.value_json)?;
            let source: serde_json::Value = serde_json::from_str(&entry.source_json)?;
            Ok(Fact::new(
                entity.to_string(),
                value,
                now,
                source,
                Operation::Assert,
            ))
        })
        .collect::<Result<_, serde_json::Error>>()?;

    let count = fact_structs.len();
    let mut writer = FactStreamWriter::open(&facts_path)?;
    writer.write_batch(&fact_structs)?;

    tracing::debug!(entity = %entity, facts_written = count, "Wrote facts");
    Ok(count)
}

fn read_stream(
    after_line: usize,
    limit: usize,
    metadata_dir: &Path,
) -> Result<StreamChunk, AcidError> {
    let facts_path = metadata_dir.join("facts.jsonl");

    if !facts_path.exists() {
        return Ok(StreamChunk {
            lines: vec![],
            cursor: cursor_from_offset(after_line),
        });
    }

    let file = std::fs::File::open(&facts_path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader
        .lines()
        .skip(after_line)
        .take(limit)
        .collect::<Result<_, _>>()?;

    let next_offset = after_line + lines.len();
    Ok(StreamChunk {
        lines,
        cursor: cursor_from_offset(next_offset),
    })
}

fn publish_facts_written(event_pub: &nng::Socket, entity: &str, count: usize, cursor: &str) {
    let event = AcidEvent::FactsWritten {
        entity: entity.to_string(),
        count,
        cursor: cursor.to_string(),
    };
    let bytes = acid_event_to_topic_message(&event);
    let msg = nng::Message::from(&bytes[..]);
    if let Err((_, e)) = event_pub.send(msg) {
        tracing::warn!(error = %e, "Failed to publish acid/facts event");
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_acid=info".into()),
        )
        .init();

    let args = Args::parse();

    tracing::info!(
        metadata_dir = %args.metadata_dir.display(),
        socket = %args.socket,
        event_socket = %args.event_socket,
        "Starting MDMA ACID service"
    );

    std::fs::create_dir_all(&args.metadata_dir)?;

    let ServiceSockets {
        rep_socket: socket,
        event_socket: event_pub_opt,
    } = service::create_sockets(&ServiceConfig {
        socket_address: args.socket.clone(),
        event_address: Some(args.event_socket.clone()),
    })?;
    tracing::info!(address = %args.socket, "ACID service listening");

    let event_pub = event_pub_opt.expect("event socket configured for mdma-acid");
    tracing::info!(address = %args.event_socket, "ACID event socket listening");

    let mut line_count = count_facts_lines(&args.metadata_dir);
    tracing::debug!(line_count, "Initial facts line count");

    loop {
        let msg = socket.recv()?;
        let request: AcidRequest = match serde_json::from_slice(&msg) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to deserialize request");
                let response = AcidResponse::Error {
                    message: format!("deserialization error: {e}"),
                };
                let data = serde_json::to_vec(&response)?;
                let out = nng::Message::from(&data[..]);
                socket.send(out).map_err(|(_, e)| e)?;
                continue;
            }
        };

        tracing::debug!(request = ?request, "Received request");

        let write_entity: Option<String> = match &request {
            AcidRequest::WriteFacts { entity, .. } => Some(entity.clone()),
            _ => None,
        };

        let response = handle_request(&request, &args.metadata_dir);

        if let (Some(entity), AcidResponse::WriteOk { facts_written }) = (&write_entity, &response)
        {
            line_count += facts_written;
            let cursor = acid_protocol::cursor_from_offset(line_count);
            publish_facts_written(&event_pub, entity, *facts_written, &cursor);
        }

        let data = serde_json::to_vec(&response)?;
        let out = nng::Message::from(&data[..]);
        socket.send(out).map_err(|(_, e)| e)?;
    }
}

fn count_facts_lines(metadata_dir: &Path) -> usize {
    let facts_path = metadata_dir.join("facts.jsonl");
    if !facts_path.exists() {
        return 0;
    }
    match std::fs::File::open(&facts_path) {
        Ok(file) => BufReader::new(file).lines().count(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_protocol::FactEntry;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn temp_metadata() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn handle_ping_returns_pong() {
        let dir = temp_metadata();
        let response = handle_request(&AcidRequest::Ping, dir.path());
        assert!(matches!(response, AcidResponse::Pong));
    }

    #[test]
    fn handle_write_facts_returns_write_ok() {
        let dir = temp_metadata();
        let request = AcidRequest::WriteFacts {
            entity: "track:sha256:abc123".to_string(),
            facts: vec![FactEntry {
                value_json: r#"{"bpm": 128}"#.to_string(),
                source_json: r#"{"source": "analyser", "version": "1.0"}"#.to_string(),
            }],
        };
        let response = handle_request(&request, dir.path());
        match response {
            AcidResponse::WriteOk { facts_written } => assert_eq!(facts_written, 1),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn handle_write_facts_creates_facts_file() {
        let dir = temp_metadata();
        let request = AcidRequest::WriteFacts {
            entity: "track:sha256:abc123".to_string(),
            facts: vec![
                FactEntry {
                    value_json: r#"{"bpm": 128}"#.to_string(),
                    source_json: r#"{"source": "analyser"}"#.to_string(),
                },
                FactEntry {
                    value_json: r#"{"key": "Am"}"#.to_string(),
                    source_json: r#"{"source": "analyser"}"#.to_string(),
                },
            ],
        };
        handle_request(&request, dir.path());

        let facts_path = dir.path().join("facts.jsonl");
        assert!(facts_path.exists(), "facts.jsonl should be created");
        let content = std::fs::read_to_string(&facts_path).unwrap();
        assert!(!content.is_empty(), "facts.jsonl should have content");
    }

    #[test]
    fn handle_read_stream_missing_file_returns_empty() {
        let dir = temp_metadata();
        let request = AcidRequest::ReadStream {
            cursor: None,
            limit: 10,
        };
        let response = handle_request(&request, dir.path());
        match response {
            AcidResponse::StreamChunk(chunk) => {
                assert_eq!(chunk.lines, Vec::<String>::new());
                assert_eq!(acid_protocol::offset_from_cursor(&chunk.cursor), Some(0));
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn handle_read_stream_returns_written_lines() {
        let dir = temp_metadata();

        let write_req = AcidRequest::WriteFacts {
            entity: "track:test".to_string(),
            facts: vec![FactEntry {
                value_json: r#"{"bpm": 140}"#.to_string(),
                source_json: r#"{"source": "test"}"#.to_string(),
            }],
        };
        handle_request(&write_req, dir.path());

        let read_req = AcidRequest::ReadStream {
            cursor: None,
            limit: 10,
        };
        let response = handle_request(&read_req, dir.path());
        match response {
            AcidResponse::StreamChunk(chunk) => {
                assert!(!chunk.lines.is_empty(), "should have at least one line");
                let offset = acid_protocol::offset_from_cursor(&chunk.cursor).unwrap();
                assert_eq!(offset, chunk.lines.len());
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn handle_read_stream_with_cursor_continues_from_offset() {
        let dir = temp_metadata();

        let write_req = AcidRequest::WriteFacts {
            entity: "track:test".to_string(),
            facts: vec![
                FactEntry {
                    value_json: r#"{"bpm": 140}"#.to_string(),
                    source_json: r#"{"source": "test"}"#.to_string(),
                },
                FactEntry {
                    value_json: r#"{"key": "Cm"}"#.to_string(),
                    source_json: r#"{"source": "test"}"#.to_string(),
                },
            ],
        };
        handle_request(&write_req, dir.path());

        let read1 = AcidRequest::ReadStream {
            cursor: None,
            limit: 1,
        };
        let chunk1 = match handle_request(&read1, dir.path()) {
            AcidResponse::StreamChunk(c) => c,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(chunk1.lines.len(), 1);

        let read2 = AcidRequest::ReadStream {
            cursor: Some(chunk1.cursor.clone()),
            limit: 10,
        };
        let chunk2 = match handle_request(&read2, dir.path()) {
            AcidResponse::StreamChunk(c) => c,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(
            chunk2.lines.len(),
            1,
            "should read exactly one remaining line"
        );
    }

    #[test]
    fn handle_write_facts_invalid_json_returns_error() {
        let dir = temp_metadata();
        let request = AcidRequest::WriteFacts {
            entity: "track:test".to_string(),
            facts: vec![FactEntry {
                value_json: "not-valid-json{{{".to_string(),
                source_json: r#"{"source": "test"}"#.to_string(),
            }],
        };
        let response = handle_request(&request, dir.path());
        assert!(
            matches!(response, AcidResponse::Error { .. }),
            "should return error for invalid JSON"
        );
    }

    #[test]
    fn count_facts_lines_returns_line_count() {
        let dir = temp_metadata();

        assert_eq!(count_facts_lines(dir.path()), 0);

        let write_req = AcidRequest::WriteFacts {
            entity: "track:test".to_string(),
            facts: vec![
                FactEntry {
                    value_json: r#"{"x": 1}"#.to_string(),
                    source_json: r#"{"source": "test"}"#.to_string(),
                },
                FactEntry {
                    value_json: r#"{"x": 2}"#.to_string(),
                    source_json: r#"{"source": "test"}"#.to_string(),
                },
            ],
        };
        handle_request(&write_req, dir.path());

        assert_eq!(
            count_facts_lines(dir.path()),
            2,
            "should count both written lines"
        );
    }
}
