use serde::{Deserialize, Serialize};

/// A unique audio fingerprint generated from audio content
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioFingerprint(Vec<u8>);

impl AudioFingerprint {
    pub fn new(data: Vec<u8>) -> Self {
        Self(data)
    }

    pub fn raw_data(&self) -> &[u8] {
        &self.0
    }

    pub fn to_hex_string(&self) -> String {
        hex::encode(&self.0)
    }

    pub fn from_hex_string(hex: &str) -> Result<Self, crate::FingerprintError> {
        hex::decode(hex)
            .map(Self::new)
            .map_err(|_| crate::FingerprintError::InvalidFingerprint)
    }
}

/// AcoustID identifier from the AcoustID database
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AcoustId(String);

impl AcoustId {
    pub fn new(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Duration of audio track in seconds (for fingerprinting accuracy)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioDuration(f64);

impl AudioDuration {
    pub fn new(seconds: f64) -> Self {
        Self(seconds)
    }

    pub fn seconds(&self) -> f64 {
        self.0
    }
}
