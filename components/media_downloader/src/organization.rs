// components/media_downloader/src/organization.rs
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrackLocation {
    pub artist: String,
    pub album: Option<String>,
    pub title: String,
}

impl TrackLocation {
    pub fn new(artist: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            artist: artist.into(),
            album: None,
            title: title.into(),
        }
    }

    pub fn with_album(
        artist: impl Into<String>,
        album: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            artist: artist.into(),
            album: Some(album.into()),
            title: title.into(),
        }
    }

    /// Generate a standard file path for the track
    pub fn to_path(&self, library_root: impl AsRef<Path>) -> PathBuf {
        let artist_dir = sanitize_filename::sanitize(&self.artist);
        let title_file = format!("{}.flac", sanitize_filename::sanitize(&self.title));
        
        let mut components = vec![library_root.as_ref().to_path_buf(), PathBuf::from(artist_dir)];
        
        if let Some(album) = &self.album {
            components.push(PathBuf::from(sanitize_filename::sanitize(album)));
        }
        
        components.push(PathBuf::from(title_file));
        components.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_path_generation_with_album() {
        let location = TrackLocation::with_album(
            "Test Artist",
            "Test Album",
            "Test Song",
        );
        
        let path = location.to_path(Path::new("/music"));
        assert_eq!(
            path,
            Path::new("/music/Test Artist/Test Album/Test Song.flac")
        );
    }

    #[test]
    fn test_path_generation_without_album() {
        let location = TrackLocation::new("Test Artist", "Test Song");
        let path = location.to_path(Path::new("/music"));
        assert_eq!(
            path,
            Path::new("/music/Test Artist/Test Song.flac")
        );
    }

    #[test]
    fn test_sanitization() {
        let location = TrackLocation::with_album(
            "Artist / with / slashes",
            "Album : with : colons",
            "Song * with * stars",
        );
        
        let path = location.to_path(Path::new("/music"));
        assert_eq!(
            path,
            Path::new("/music/Artist  with  slashes/Album  with  colons/Song  with  stars.flac")
        );
    }

    #[test]
    fn test_sanitization_edge_cases() {
        let location = TrackLocation::new(
            "Artist?<>\\/*", 
            "Song|\":\n\t"
        );
        
        let path = location.to_path(Path::new("/music"));
        
        // Get just the filename parts (artist and song)
        let components: Vec<_> = path.components()
            .skip(2) // Skip "/music"
            .map(|c| c.as_os_str().to_string_lossy())
            .collect();
            
        // Should not contain any of these characters
        let forbidden_chars = ['<', '>', '\\', '/', '*', '|', '"', ':', '\n', '\t'];
        
        for component in components {
            for forbidden in forbidden_chars {
                assert!(!component.contains(forbidden), 
                    "Sanitized component '{}' should not contain '{}'", component, forbidden);
            }
        }
    }
}
