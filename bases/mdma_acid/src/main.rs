use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use acid_protocol::{AcidRequest, AcidResponse, StreamChunk};
use chrono::Utc;
use clap::Parser;
use color_eyre::Result;
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

    /// nng IPC socket path
    #[arg(long, default_value = "ipc:///run/mdma/acid.sock")]
    socket: String,

    /// Also listen on TCP for remote connections (e.g., "tcp://0.0.0.0:5560")
    #[arg(long)]
    tcp: Option<String>,
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

        AcidRequest::ReadStream { after_line, limit } => {
            match read_stream(*after_line, *limit, metadata_dir) {
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
            next_offset: after_line,
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
    Ok(StreamChunk { lines, next_offset })
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
        "Starting MDMA ACID service"
    );

    std::fs::create_dir_all(&args.metadata_dir)?;

    if args.socket.starts_with("ipc://") {
        if let Some(path) = args.socket.strip_prefix("ipc://") {
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
    }

    let socket = nng::Socket::new(nng::Protocol::Rep0)?;
    socket.listen(&args.socket)?;
    tracing::info!(address = %args.socket, "ACID service listening");

    if let Some(ref tcp) = args.tcp {
        socket.listen(tcp)?;
        tracing::info!(address = %tcp, "ACID service also listening on TCP");
    }

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
        let response = handle_request(&request, &args.metadata_dir);
        let data = serde_json::to_vec(&response)?;
        let out = nng::Message::from(&data[..]);
        socket.send(out).map_err(|(_, e)| e)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acid_protocol::FactEntry;
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
            after_line: 0,
            limit: 10,
        };
        let response = handle_request(&request, dir.path());
        match response {
            AcidResponse::StreamChunk(chunk) => {
                assert!(chunk.lines.is_empty());
                assert_eq!(chunk.next_offset, 0);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn handle_read_stream_returns_written_lines() {
        let dir = temp_metadata();

        // Write some facts first
        let write_req = AcidRequest::WriteFacts {
            entity: "track:test".to_string(),
            facts: vec![FactEntry {
                value_json: r#"{"bpm": 140}"#.to_string(),
                source_json: r#"{"source": "test"}"#.to_string(),
            }],
        };
        handle_request(&write_req, dir.path());

        let read_req = AcidRequest::ReadStream {
            after_line: 0,
            limit: 10,
        };
        let response = handle_request(&read_req, dir.path());
        match response {
            AcidResponse::StreamChunk(chunk) => {
                assert!(!chunk.lines.is_empty(), "should have at least one line");
                assert_eq!(chunk.next_offset, chunk.lines.len());
            }
            other => panic!("unexpected response: {other:?}"),
        }
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
}
