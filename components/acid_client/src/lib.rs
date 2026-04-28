//! ACID Client
//!
//! NNG client for connecting to the ACID fact store service.

pub use acid_protocol::{AcidRequest, AcidResponse, EntityFacts, FactEntry, StreamChunk};
pub use nng_transport::NngClientError as ClientError;

use nng::options::{Options, RecvTimeout, SendTimeout};
use nng_transport::request_response;
use std::time::Duration;

pub struct AcidClient {
    socket: nng::Socket,
}

impl AcidClient {
    pub fn connect(address: &str) -> Result<Self, ClientError> {
        // Uses dial_async (non-blocking dial) rather than nng_transport::connect()
        // which uses blocking dial. Acid writes are fire-and-forget and benefit
        // from async dialing to avoid blocking startup if the acid service is slow.
        let socket = nng::Socket::new(nng::Protocol::Req0)?;
        socket.set_opt::<SendTimeout>(Some(Duration::from_secs(5)))?;
        socket.set_opt::<RecvTimeout>(Some(Duration::from_secs(5)))?;
        socket.dial_async(address)?;
        Ok(Self { socket })
    }

    fn request(&self, request: &AcidRequest) -> Result<AcidResponse, ClientError> {
        request_response(&self.socket, request)
    }

    pub fn ping(&self) -> Result<(), ClientError> {
        match self.request(&AcidRequest::Ping)? {
            AcidResponse::Pong => Ok(()),
            AcidResponse::Error { message } => Err(ClientError::Service(message)),
            other => Err(ClientError::Service(format!(
                "unexpected response: {:?}",
                other
            ))),
        }
    }

    pub fn write_facts(&self, entity: &str, facts: &[FactEntry]) -> Result<usize, ClientError> {
        let req = AcidRequest::WriteFacts {
            entity: entity.to_string(),
            facts: facts.to_vec(),
        };
        match self.request(&req)? {
            AcidResponse::WriteOk { facts_written } => Ok(facts_written),
            AcidResponse::Error { message } => Err(ClientError::Service(message)),
            other => Err(ClientError::Service(format!(
                "unexpected response: {:?}",
                other
            ))),
        }
    }

    pub fn read_stream(
        &self,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<StreamChunk, ClientError> {
        let req = AcidRequest::ReadStream { cursor, limit };
        match self.request(&req)? {
            AcidResponse::StreamChunk(chunk) => Ok(chunk),
            AcidResponse::Error { message } => Err(ClientError::Service(message)),
            other => Err(ClientError::Service(format!(
                "unexpected response: {:?}",
                other
            ))),
        }
    }

    /// Retract a batch of facts for `entity`. Returns the number of retractions appended.
    pub fn retract_facts(&self, entity: &str, facts: &[FactEntry]) -> Result<usize, ClientError> {
        let req = AcidRequest::RetractFacts {
            entity: entity.to_string(),
            facts: facts.to_vec(),
        };
        match self.request(&req)? {
            AcidResponse::RetractOk { facts_retracted } => Ok(facts_retracted),
            AcidResponse::Error { message } => Err(ClientError::Service(message)),
            other => Err(ClientError::Service(format!(
                "unexpected response: {:?}",
                other
            ))),
        }
    }

    /// Read all stored facts for a single entity. Returns JSONL lines.
    pub fn read_entity(&self, entity: &str) -> Result<Vec<String>, ClientError> {
        let req = AcidRequest::ReadEntity {
            entity: entity.to_string(),
        };
        match self.request(&req)? {
            AcidResponse::EntityFacts(ef) => Ok(ef.lines),
            AcidResponse::Error { message } => Err(ClientError::Service(message)),
            other => Err(ClientError::Service(format!(
                "unexpected response: {:?}",
                other
            ))),
        }
    }
}

// Music-domain helpers behind feature flag
#[cfg(feature = "music")]
mod music {
    use super::*;
    use music_facts::{ContentHash, FactSource, MusicValue};

    impl AcidClient {
        /// Write music facts through ACID, serializing MusicValue and FactSource to JSON strings.
        pub fn write_music_facts(
            &self,
            hash: &ContentHash,
            facts: &[(MusicValue, FactSource)],
        ) -> Result<usize, ClientError> {
            let entries: Vec<FactEntry> = facts
                .iter()
                .map(|(value, source)| -> Result<FactEntry, ClientError> {
                    Ok(FactEntry {
                        value_json: serde_json::to_string(value)?,
                        source_json: serde_json::to_string(source)?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.write_facts(hash.as_str(), &entries)
        }

        /// Retract music facts through ACID, serializing MusicValue and FactSource to JSON strings.
        pub fn retract_music_facts(
            &self,
            hash: &ContentHash,
            facts: &[(MusicValue, FactSource)],
        ) -> Result<usize, ClientError> {
            let entries: Vec<FactEntry> = facts
                .iter()
                .map(|(value, source)| -> Result<FactEntry, ClientError> {
                    Ok(FactEntry {
                        value_json: serde_json::to_string(value)?,
                        source_json: serde_json::to_string(source)?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.retract_facts(hash.as_str(), &entries)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Verify that ClientError::Service variant is constructable and displays correctly.
    #[test]
    fn client_error_service_display() {
        let err = ClientError::Service("something failed".to_string());
        assert!(err.to_string().contains("something failed"));
    }

    /// Verify that ClientError::Serialization wraps serde_json::Error via From.
    #[test]
    fn client_error_serialization_from_serde() {
        let bad_json = b"not valid json {{{";
        let serde_err = serde_json::from_slice::<AcidRequest>(bad_json).unwrap_err();
        let client_err = ClientError::from(serde_err);
        assert!(client_err.to_string().starts_with("serialization error:"));
    }

    /// Verify that protocol types are re-exported from this crate.
    #[test]
    fn reexported_types_are_usable() {
        let entry = FactEntry {
            value_json: r#"{"bpm":128}"#.to_string(),
            source_json: r#"{"source":"analyser"}"#.to_string(),
        };
        let req = AcidRequest::WriteFacts {
            entity: "track:abc".to_string(),
            facts: vec![entry],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("WriteFacts"));

        let chunk = StreamChunk {
            lines: vec!["line".to_string()],
            cursor: "line:1".to_string(),
        };
        assert_eq!(chunk.cursor, "line:1");
    }

    // ---- Integration tests (spin up real in-process ACID server) ---------------

    static ACID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn spawn_acid() -> (acid_service::ServerHandle, String) {
        let id = ACID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let addr = format!("ipc:///tmp/mdma-test-acidclient-{pid}-{id}.sock");
        let events_addr = format!("ipc:///tmp/mdma-test-acidclient-ev-{pid}-{id}.sock");

        let rep = nng::Socket::new(nng::Protocol::Rep0).expect("rep socket");
        rep.listen(&addr).expect("rep listen");
        let pub_sock = nng::Socket::new(nng::Protocol::Pub0).expect("pub socket");
        pub_sock.listen(&events_addr).expect("pub listen");

        let handle = acid_service::start(rep, pub_sock, std::path::Path::new("/tmp"))
            .expect("start acid service");
        std::thread::sleep(std::time::Duration::from_millis(20));
        (handle, addr)
    }

    #[test]
    fn retract_facts_returns_count() {
        let (_handle, addr) = spawn_acid();
        let client = AcidClient::connect(&addr).unwrap();

        let facts = vec![FactEntry {
            value_json: r#"{"genre":"techno"}"#.to_string(),
            source_json: r#"{"source":"tagger"}"#.to_string(),
        }];

        let count = client.retract_facts("entity:retract-test", &facts).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn read_entity_returns_only_matching_entity() {
        let (_handle, addr) = spawn_acid();
        let client = AcidClient::connect(&addr).unwrap();

        let alpha_fact = vec![FactEntry {
            value_json: r#"{"key":"Am"}"#.to_string(),
            source_json: r#"{"source":"analyser"}"#.to_string(),
        }];
        let beta_fact = vec![FactEntry {
            value_json: r#"{"key":"Cm"}"#.to_string(),
            source_json: r#"{"source":"analyser"}"#.to_string(),
        }];

        client.write_facts("entity:alpha", &alpha_fact).unwrap();
        client.write_facts("entity:beta", &beta_fact).unwrap();

        let lines = client.read_entity("entity:alpha").unwrap();
        assert_eq!(lines.len(), 1, "expected only alpha's fact");
        assert!(
            lines[0].contains("entity:alpha"),
            "line should reference entity:alpha"
        );
    }

    #[test]
    fn retract_facts_appear_in_stream_with_retract_operation() {
        let (_handle, addr) = spawn_acid();
        let client = AcidClient::connect(&addr).unwrap();

        let facts = vec![FactEntry {
            value_json: r#""bad-tag""#.to_string(),
            source_json: r#"{"source":"test"}"#.to_string(),
        }];

        client
            .retract_facts("entity:retract-stream", &facts)
            .unwrap();

        let chunk = client.read_stream(None, 100).unwrap();
        assert_eq!(chunk.lines.len(), 1);
        assert!(
            chunk.lines[0].contains("Retract"),
            "stream line should contain Retract, got: {}",
            chunk.lines[0]
        );
    }
}
