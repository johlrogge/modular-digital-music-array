#![allow(dead_code)]
use mdma_client::TrackInfo;

/// Which field the browser is currently grouping or browsing by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowseField {
    Artist,
    Album,
    Genre,
    Title,
}

impl BrowseField {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Artist => "Artists",
            Self::Album => "Albums",
            Self::Genre => "Genres",
            Self::Title => "Tracks",
        }
    }

    /// Extract the grouping value from a TrackInfo for this field.
    /// Returns None if the field is not set on the track.
    pub fn extract(&self, track: &TrackInfo) -> Option<String> {
        match self {
            Self::Artist => track.artist.clone(),
            Self::Album => track.album.clone(),
            // Genre is not directly in TrackInfo — handled via get_fact_values
            Self::Genre => None,
            Self::Title => track.title.clone(),
        }
    }
}

/// Find the first index in `names` whose name starts with `ch` (case-insensitive).
/// Returns None if no match is found.
pub fn find_first_by_letter(names: &[impl AsRef<str>], ch: char) -> Option<usize> {
    let target = ch.to_ascii_lowercase();
    names.iter().position(|n| {
        n.as_ref()
            .chars()
            .next()
            .map(|c| c.to_ascii_lowercase() == target)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdma_client::ContentHash;

    fn make_track(artist: Option<&str>, album: Option<&str>, title: Option<&str>) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new("sha256:test"),
            title: title.map(String::from),
            artist: artist.map(String::from),
            album: album.map(String::from),
            duration: None,
            bpm: None,
            key: None,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
            memory_cues: vec![],
            beat_grid: None,
            role: None,
            energy: None,
        }
    }

    #[test]
    fn browse_field_display_names() {
        assert_eq!(BrowseField::Artist.display_name(), "Artists");
        assert_eq!(BrowseField::Album.display_name(), "Albums");
        assert_eq!(BrowseField::Genre.display_name(), "Genres");
        assert_eq!(BrowseField::Title.display_name(), "Tracks");
    }

    #[test]
    fn browse_field_extract_artist_returns_artist() {
        let track = make_track(
            Some("Carbon Based Lifeforms"),
            Some("Twentythree"),
            Some("Abiogenesis"),
        );
        assert_eq!(
            BrowseField::Artist.extract(&track),
            Some("Carbon Based Lifeforms".to_string())
        );
    }

    #[test]
    fn browse_field_extract_artist_returns_none_when_missing() {
        let track = make_track(None, Some("Twentythree"), Some("Abiogenesis"));
        assert_eq!(BrowseField::Artist.extract(&track), None);
    }

    #[test]
    fn browse_field_extract_album_returns_album() {
        let track = make_track(Some("CBL"), Some("Twentythree"), Some("Abiogenesis"));
        assert_eq!(
            BrowseField::Album.extract(&track),
            Some("Twentythree".to_string())
        );
    }

    #[test]
    fn browse_field_extract_title_returns_title() {
        let track = make_track(Some("CBL"), Some("Twentythree"), Some("Abiogenesis"));
        assert_eq!(
            BrowseField::Title.extract(&track),
            Some("Abiogenesis".to_string())
        );
    }

    #[test]
    fn browse_field_extract_genre_returns_none() {
        // Genre is not in TrackInfo directly; always returns None
        let track = make_track(Some("CBL"), Some("Twentythree"), Some("Abiogenesis"));
        assert_eq!(BrowseField::Genre.extract(&track), None);
    }

    #[test]
    fn find_first_by_letter_finds_match() {
        let names = vec!["Alpha", "Beta", "Charlie", "Delta"];
        assert_eq!(find_first_by_letter(&names, 'b'), Some(1));
    }

    #[test]
    fn find_first_by_letter_case_insensitive() {
        let names = vec!["alpha", "Beta", "charlie"];
        assert_eq!(find_first_by_letter(&names, 'B'), Some(1));
        assert_eq!(find_first_by_letter(&names, 'c'), Some(2));
    }

    #[test]
    fn find_first_by_letter_no_match_returns_none() {
        let names = vec!["Alpha", "Beta"];
        assert_eq!(find_first_by_letter(&names, 'z'), None);
    }

    #[test]
    fn find_first_by_letter_empty_list_returns_none() {
        let names: Vec<String> = vec![];
        assert_eq!(find_first_by_letter(&names, 'a'), None);
    }
}
