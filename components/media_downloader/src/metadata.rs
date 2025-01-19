// components/media_downloader/src/metadata.rs
use crate::organization::TrackLocation;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackMetadata {
    /// Location information (artist, album, title)
    pub location: TrackLocation,
    
    /// Duration in seconds
    pub duration: f64,
    
    /// Original URL the track was downloaded from
    pub source_url: String,
    
    /// When the track was downloaded
    pub download_time: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_metadata_serialization() {
        let location = TrackLocation::new(
            "Test Artist",
            "Test Song",
        );
        
        let metadata = TrackMetadata {
            location,
            duration: 180.5,
            source_url: "https://example.com/song".to_string(),
            download_time: Utc::now(),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        let decoded: TrackMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.duration, 180.5);
        assert_eq!(decoded.source_url, "https://example.com/song");
        assert_eq!(decoded.location.artist, "Test Artist");
        assert_eq!(decoded.location.title, "Test Song");
    }

    #[test]
    fn test_metadata_with_album() {
        let location = TrackLocation::with_album(
            "Test Artist",
            "Test Album",
            "Test Song",
        );
        
        let metadata = TrackMetadata {
            location,
            duration: 180.5,
            source_url: "https://example.com/song".to_string(),
            download_time: Utc::now(),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        let decoded: TrackMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.location.artist, "Test Artist");
        assert_eq!(decoded.location.album, Some("Test Album".to_string()));
        assert_eq!(decoded.location.title, "Test Song");
    }
}
