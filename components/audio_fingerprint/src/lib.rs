mod error;
mod types;

pub use error::FingerprintError;
pub use types::{AcoustId, AudioFingerprint};

// Basic API we're aiming for
use std::path::Path;

pub async fn generate_fingerprint(path: &Path) -> Result<AudioFingerprint, FingerprintError> {
    todo!("Implementation coming next")
}

pub async fn lookup_acoustid(
    fingerprint: &AudioFingerprint,
) -> Result<Option<AcoustId>, FingerprintError> {
    todo!("AcoustID API lookup")
}
