//! ACID Client
//!
//! NNG client for connecting to the ACID fact store service.

pub use acid_protocol::{AcidRequest, AcidResponse, FactEntry, StreamChunk};

use nng::options::{Options, RecvTimeout, SendTimeout};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("NNG error: {0}")]
    Nng(#[from] nng::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("ACID error: {0}")]
    Acid(String),
}

pub struct AcidClient {
    socket: nng::Socket,
}

impl AcidClient {
    pub fn connect(address: &str) -> Result<Self, ClientError> {
        let socket = nng::Socket::new(nng::Protocol::Req0)?;
        socket.set_opt::<SendTimeout>(Some(Duration::from_secs(5)))?;
        socket.set_opt::<RecvTimeout>(Some(Duration::from_secs(5)))?;
        socket.dial_async(address)?;
        Ok(Self { socket })
    }

    fn request(&self, request: &AcidRequest) -> Result<AcidResponse, ClientError> {
        let data = serde_json::to_vec(request)?;
        let msg = nng::Message::from(&data[..]);
        self.socket
            .send(msg)
            .map_err(|(_, e)| ClientError::Nng(e))?;
        let response_msg = self.socket.recv()?;
        let response: AcidResponse = serde_json::from_slice(&response_msg)?;
        Ok(response)
    }

    pub fn ping(&self) -> Result<(), ClientError> {
        match self.request(&AcidRequest::Ping)? {
            AcidResponse::Pong => Ok(()),
            AcidResponse::Error { message } => Err(ClientError::Acid(message)),
            other => Err(ClientError::Acid(format!(
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
            AcidResponse::Error { message } => Err(ClientError::Acid(message)),
            other => Err(ClientError::Acid(format!(
                "unexpected response: {:?}",
                other
            ))),
        }
    }

    pub fn read_stream(&self, after_line: usize, limit: usize) -> Result<StreamChunk, ClientError> {
        let req = AcidRequest::ReadStream { after_line, limit };
        match self.request(&req)? {
            AcidResponse::StreamChunk(chunk) => Ok(chunk),
            AcidResponse::Error { message } => Err(ClientError::Acid(message)),
            other => Err(ClientError::Acid(format!(
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
            self.write_facts(&hash.0, &entries)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that ClientError variants are constructable and display correctly.
    #[test]
    fn client_error_acid_display() {
        let err = ClientError::Acid("something failed".to_string());
        assert_eq!(err.to_string(), "ACID error: something failed");
    }

    /// Verify that ClientError::Serialization wraps serde_json::Error via From.
    #[test]
    fn client_error_serialization_from_serde() {
        let bad_json = b"not valid json {{{";
        let serde_err = serde_json::from_slice::<AcidRequest>(bad_json).unwrap_err();
        let client_err = ClientError::from(serde_err);
        assert!(client_err.to_string().starts_with("Serialization error:"));
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
            next_offset: 1,
        };
        assert_eq!(chunk.next_offset, 1);
    }
}
