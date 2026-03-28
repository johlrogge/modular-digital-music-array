/// Rekordbox XML export utilities.
///
/// Generates Pioneer Rekordbox-compatible XML library files for import
/// via Rekordbox File → Import Library.

// =============================================================================
// tonality module — converts MDMA Key to Camelot notation
// =============================================================================

pub mod tonality {
    use music_primitives::Key;

    /// Convert an MDMA Key to Camelot notation string (e.g. "8B", "8A").
    pub fn key_to_tonality(key: &Key) -> String {
        key.to_camelot()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use music_primitives::Key;

        #[test]
        fn c_major_is_8b() {
            let key = Key::from_traditional("C Major").unwrap();
            assert_eq!(key_to_tonality(&key), "8B");
        }

        #[test]
        fn a_minor_is_8a() {
            let key = Key::from_traditional("A Minor").unwrap();
            assert_eq!(key_to_tonality(&key), "8A");
        }

        #[test]
        fn g_major_is_9b() {
            let key = Key::from_traditional("G Major").unwrap();
            assert_eq!(key_to_tonality(&key), "9B");
        }

        #[test]
        fn e_minor_is_9a() {
            let key = Key::from_traditional("E Minor").unwrap();
            assert_eq!(key_to_tonality(&key), "9A");
        }
    }
}

// =============================================================================
// kind module — maps file extension to Rekordbox "Kind" string
// =============================================================================

pub mod kind {
    /// Map a file extension (lowercase, without dot) to a Rekordbox Kind string.
    pub fn ext_to_kind(ext: &str) -> &'static str {
        match ext {
            "aiff" | "aif" => "AIFF File",
            "wav" => "WAV File",
            "flac" => "FLAC File",
            "mp3" => "MP3 File",
            "ogg" => "OGG File",
            "opus" => "OPUS File",
            "m4a" => "M4A File",
            _ => "Unknown File",
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rstest::rstest;

        #[rstest]
        #[case("aiff", "AIFF File")]
        #[case("aif", "AIFF File")]
        #[case("wav", "WAV File")]
        #[case("flac", "FLAC File")]
        #[case("mp3", "MP3 File")]
        #[case("ogg", "OGG File")]
        #[case("opus", "OPUS File")]
        #[case("m4a", "M4A File")]
        #[case("xyz", "Unknown File")]
        #[case("", "Unknown File")]
        fn ext_to_kind_maps_correctly(#[case] ext: &str, #[case] expected: &str) {
            assert_eq!(ext_to_kind(ext), expected);
        }
    }
}

// =============================================================================
// path_uri module — converts filesystem paths to file://localhost/ URIs
// =============================================================================

pub mod path_uri {
    /// Convert a filesystem path to a Rekordbox-compatible file URI.
    ///
    /// Produces `file://localhost/<encoded-path>` with forward slashes.
    /// Percent-encodes: space→`%20`, `#`→`%23`, `?`→`%3F`, `%`→`%25`,
    /// `&`→`%26`, `'`→`%27`, `[`→`%5B`, `]`→`%5D`.
    pub fn path_to_file_uri(path: &std::path::Path) -> String {
        let path_str = path.to_string_lossy();
        // Use forward slashes
        let forward_slashes = path_str.replace('\\', "/");
        let encoded = percent_encode(&forward_slashes);
        format!("file://localhost{}", encoded)
    }

    fn percent_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '%' => out.push_str("%25"),
                ' ' => out.push_str("%20"),
                '#' => out.push_str("%23"),
                '?' => out.push_str("%3F"),
                '&' => out.push_str("%26"),
                '\'' => out.push_str("%27"),
                '[' => out.push_str("%5B"),
                ']' => out.push_str("%5D"),
                c => out.push(c),
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pretty_assertions::assert_eq;
        use std::path::Path;

        #[test]
        fn plain_path_no_encoding() {
            let path = Path::new("/home/user/music/track.aiff");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/home/user/music/track.aiff"
            );
        }

        #[test]
        fn space_is_percent_encoded() {
            let path = Path::new("/home/user/music/My Track.aiff");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/home/user/music/My%20Track.aiff"
            );
        }

        #[test]
        fn hash_is_percent_encoded() {
            let path = Path::new("/music/track#1.wav");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/music/track%231.wav"
            );
        }

        #[test]
        fn question_mark_is_percent_encoded() {
            let path = Path::new("/music/what?.flac");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/music/what%3F.flac"
            );
        }

        #[test]
        fn percent_is_double_encoded() {
            let path = Path::new("/music/100%.flac");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/music/100%25.flac"
            );
        }

        #[test]
        fn ampersand_is_percent_encoded() {
            let path = Path::new("/music/Simon & Garfunkel/track.mp3");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/music/Simon%20%26%20Garfunkel/track.mp3"
            );
        }

        #[test]
        fn apostrophe_is_percent_encoded() {
            let path = Path::new("/music/Rock'n'Roll.flac");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/music/Rock%27n%27Roll.flac"
            );
        }

        #[test]
        fn square_brackets_are_percent_encoded() {
            let path = Path::new("/music/track[1].wav");
            assert_eq!(
                path_to_file_uri(path),
                "file://localhost/music/track%5B1%5D.wav"
            );
        }
    }
}

// =============================================================================
// xml module — data structures and XML generation
// =============================================================================

pub mod xml {
    /// A single track entry for the Rekordbox XML collection.
    pub struct RekordboxTrack {
        pub track_id: u32,
        pub name: String,
        pub artist: String,
        pub album: String,
        pub genre: String,
        pub kind: String,
        pub size: u64,
        pub total_time: u32,
        pub average_bpm: Option<f32>,
        pub tonality: Option<String>,
        pub track_number: Option<u32>,
        pub disc_number: Option<u32>,
        pub year: Option<String>,
        pub label: Option<String>,
        pub comment: Option<String>,
        pub date_added: Option<String>, // "YYYY-MM-DD"
        pub bitrate: Option<u32>,
        pub sample_rate: Option<u32>,
        pub location: String, // file://localhost/ URI
    }

    /// A playlist entry referencing tracks by ID.
    pub struct RekordboxPlaylist {
        pub name: String,
        pub track_ids: Vec<u32>,
    }

    /// A full Rekordbox library with tracks and playlists.
    pub struct RekordboxLibrary {
        pub tracks: Vec<RekordboxTrack>,
        pub playlists: Vec<RekordboxPlaylist>,
    }

    /// XML-escape a string for use in attribute values and text content.
    fn xml_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                c => out.push(c),
            }
        }
        out
    }

    impl RekordboxLibrary {
        /// Generate a Rekordbox-compatible XML string.
        pub fn to_xml(&self) -> String {
            let mut out = String::new();

            out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
            out.push_str("<DJ_PLAYLISTS Version=\"1.0.0\">\n");
            out.push_str(
                "  <PRODUCT Name=\"rekordbox\" Version=\"6.0.0\" Company=\"AlphaTheta\"/>\n",
            );

            // COLLECTION
            let entry_count = self.tracks.len();
            out.push_str(&format!(
                "  <COLLECTION Entries=\"{}\">\n",
                entry_count
            ));

            for track in &self.tracks {
                write_track(&mut out, track);
            }

            out.push_str("  </COLLECTION>\n");

            // PLAYLISTS
            out.push_str("  <PLAYLISTS>\n");

            if self.playlists.is_empty() {
                out.push_str("    <NODE Type=\"0\" Name=\"ROOT\" Count=\"0\"/>\n");
            } else {
                let playlist_count = self.playlists.len();
                out.push_str(&format!(
                    "    <NODE Type=\"0\" Name=\"ROOT\" Count=\"{}\">\n",
                    playlist_count
                ));
                out.push_str(&format!(
                    "      <NODE Name=\"MDMA Export\" Type=\"0\" Count=\"{}\">\n",
                    playlist_count
                ));

                for playlist in &self.playlists {
                    out.push_str(&format!(
                        "        <NODE Name=\"{}\" Type=\"1\" KeyType=\"0\" Entries=\"{}\">\n",
                        xml_escape(&playlist.name),
                        playlist.track_ids.len()
                    ));
                    for &id in &playlist.track_ids {
                        out.push_str(&format!("          <TRACK Key=\"{}\"/>\n", id));
                    }
                    out.push_str("        </NODE>\n");
                }

                out.push_str("      </NODE>\n");
                out.push_str("    </NODE>\n");
            }

            out.push_str("  </PLAYLISTS>\n");
            out.push_str("</DJ_PLAYLISTS>\n");

            out
        }
    }

    fn write_track(out: &mut String, track: &RekordboxTrack) {
        out.push_str("    <TRACK");
        out.push_str(&format!(" TrackID=\"{}\"", track.track_id));
        out.push_str(&format!(" Name=\"{}\"", xml_escape(&track.name)));
        out.push_str(&format!(" Artist=\"{}\"", xml_escape(&track.artist)));
        out.push_str(&format!(" Album=\"{}\"", xml_escape(&track.album)));
        out.push_str(&format!(" Genre=\"{}\"", xml_escape(&track.genre)));
        out.push_str(&format!(" Kind=\"{}\"", xml_escape(&track.kind)));
        out.push_str(&format!(" Size=\"{}\"", track.size));
        out.push_str(&format!(" TotalTime=\"{}\"", track.total_time));

        if let Some(bpm) = track.average_bpm {
            out.push_str(&format!(" AverageBpm=\"{:.2}\"", bpm));
        }

        out.push_str(&format!(
            " TrackNumber=\"{}\"",
            track.track_number.unwrap_or(0)
        ));
        out.push_str(&format!(
            " DiscNumber=\"{}\"",
            track.disc_number.unwrap_or(0)
        ));
        out.push_str(&format!(
            " Year=\"{}\"",
            xml_escape(track.year.as_deref().unwrap_or(""))
        ));
        out.push_str(&format!(
            " Label=\"{}\"",
            xml_escape(track.label.as_deref().unwrap_or(""))
        ));
        out.push_str(&format!(
            " Comments=\"{}\"",
            xml_escape(track.comment.as_deref().unwrap_or(""))
        ));
        out.push_str(" Rating=\"0\"");
        out.push_str(" Colour=\"0\"");
        out.push_str(&format!(
            " DateAdded=\"{}\"",
            xml_escape(track.date_added.as_deref().unwrap_or(""))
        ));
        out.push_str(&format!(
            " BitRate=\"{}\"",
            track.bitrate.unwrap_or(0)
        ));
        out.push_str(&format!(
            " SampleRate=\"{}\"",
            track.sample_rate.unwrap_or(0)
        ));

        if let Some(ref tonality) = track.tonality {
            out.push_str(&format!(" Tonality=\"{}\"", xml_escape(tonality)));
        }

        out.push_str(&format!(" Location=\"{}\"", xml_escape(&track.location)));

        if let Some(bpm) = track.average_bpm {
            // Has BPM: write TEMPO child element
            out.push_str(">\n");
            out.push_str(&format!(
                "      <TEMPO Inizio=\"0.000\" Bpm=\"{:.2}\" Metro=\"4/4\" Battito=\"1\"/>\n",
                bpm
            ));
            out.push_str("    </TRACK>\n");
        } else {
            out.push_str("/>\n");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pretty_assertions::assert_eq;

        fn make_test_library() -> RekordboxLibrary {
            RekordboxLibrary {
                tracks: vec![RekordboxTrack {
                    track_id: 1,
                    name: "Test Track".to_string(),
                    artist: "Test Artist".to_string(),
                    album: "Test Album".to_string(),
                    genre: "Techno".to_string(),
                    kind: "AIFF File".to_string(),
                    size: 12345678,
                    total_time: 423,
                    average_bpm: Some(128.0),
                    tonality: Some("8B".to_string()),
                    track_number: Some(1),
                    disc_number: Some(1),
                    year: Some("2023".to_string()),
                    label: Some("Fabric".to_string()),
                    comment: None,
                    date_added: Some("2024-06-15".to_string()),
                    bitrate: Some(1411),
                    sample_rate: Some(44100),
                    location: "file://localhost/music/Test%20Track.aiff".to_string(),
                }],
                playlists: vec![RekordboxPlaylist {
                    name: "My Set".to_string(),
                    track_ids: vec![1],
                }],
            }
        }

        #[test]
        fn xml_has_dj_playlists_root() {
            let xml = make_test_library().to_xml();
            assert!(xml.contains("<DJ_PLAYLISTS Version=\"1.0.0\">"), "missing root");
            assert!(xml.contains("</DJ_PLAYLISTS>"), "missing closing root");
        }

        #[test]
        fn xml_collection_entry_count_matches_tracks() {
            let xml = make_test_library().to_xml();
            assert!(
                xml.contains("COLLECTION Entries=\"1\""),
                "COLLECTION Entries should be 1"
            );
        }

        #[test]
        fn xml_track_attributes_present() {
            let xml = make_test_library().to_xml();
            assert!(xml.contains("TrackID=\"1\""), "TrackID missing");
            assert!(xml.contains("Name=\"Test Track\""), "Name missing");
            assert!(xml.contains("Artist=\"Test Artist\""), "Artist missing");
            assert!(xml.contains("AverageBpm=\"128.00\""), "BPM missing");
            assert!(xml.contains("Tonality=\"8B\""), "Tonality missing");
            assert!(xml.contains("Kind=\"AIFF File\""), "Kind missing");
            assert!(xml.contains("Size=\"12345678\""), "Size missing");
            assert!(xml.contains("TotalTime=\"423\""), "TotalTime missing");
            assert!(xml.contains("BitRate=\"1411\""), "BitRate missing");
            assert!(xml.contains("SampleRate=\"44100\""), "SampleRate missing");
            assert!(xml.contains("DateAdded=\"2024-06-15\""), "DateAdded missing");
        }

        #[test]
        fn xml_tempo_element_present_when_bpm_set() {
            let xml = make_test_library().to_xml();
            assert!(
                xml.contains("<TEMPO Inizio=\"0.000\" Bpm=\"128.00\""),
                "TEMPO element missing"
            );
        }

        #[test]
        fn xml_tempo_absent_when_no_bpm() {
            let lib = RekordboxLibrary {
                tracks: vec![RekordboxTrack {
                    track_id: 1,
                    name: "No BPM Track".to_string(),
                    artist: "Artist".to_string(),
                    album: "Album".to_string(),
                    genre: "".to_string(),
                    kind: "MP3 File".to_string(),
                    size: 1000,
                    total_time: 180,
                    average_bpm: None,
                    tonality: None,
                    track_number: None,
                    disc_number: None,
                    year: None,
                    label: None,
                    comment: None,
                    date_added: None,
                    bitrate: None,
                    sample_rate: None,
                    location: "file://localhost/music/nobpm.mp3".to_string(),
                }],
                playlists: vec![],
            };
            let xml = lib.to_xml();
            assert!(!xml.contains("<TEMPO"), "TEMPO should not appear when BPM is None");
            // Self-closing track tag
            assert!(xml.contains("/>"), "track should be self-closing when no BPM");
        }

        #[test]
        fn xml_playlist_structure() {
            let xml = make_test_library().to_xml();
            assert!(xml.contains("NODE Name=\"My Set\""), "playlist name missing");
            assert!(xml.contains("TRACK Key=\"1\""), "playlist track ref missing");
        }

        #[test]
        fn xml_empty_playlists_section() {
            let lib = RekordboxLibrary {
                tracks: vec![],
                playlists: vec![],
            };
            let xml = lib.to_xml();
            assert!(
                xml.contains("<NODE Type=\"0\" Name=\"ROOT\" Count=\"0\"/>"),
                "empty playlists should have Count=0 self-closing NODE"
            );
        }

        #[test]
        fn xml_escape_special_chars() {
            let lib = RekordboxLibrary {
                tracks: vec![RekordboxTrack {
                    track_id: 1,
                    name: "Track & \"More\"".to_string(),
                    artist: "Artist <Test>".to_string(),
                    album: "Album".to_string(),
                    genre: "".to_string(),
                    kind: "MP3 File".to_string(),
                    size: 1000,
                    total_time: 180,
                    average_bpm: None,
                    tonality: None,
                    track_number: None,
                    disc_number: None,
                    year: None,
                    label: None,
                    comment: None,
                    date_added: None,
                    bitrate: None,
                    sample_rate: None,
                    location: "file://localhost/music/track.mp3".to_string(),
                }],
                playlists: vec![],
            };
            let xml = lib.to_xml();
            assert!(
                xml.contains("Name=\"Track &amp; &quot;More&quot;\""),
                "& and \" not escaped in name"
            );
            assert!(
                xml.contains("Artist=\"Artist &lt;Test&gt;\""),
                "< and > not escaped in artist"
            );
        }

        #[test]
        fn xml_zero_entry_collection_is_valid() {
            let lib = RekordboxLibrary {
                tracks: vec![],
                playlists: vec![],
            };
            let xml = lib.to_xml();
            assert!(xml.contains("COLLECTION Entries=\"0\""), "zero entry count");
        }
    }
}

// =============================================================================
// Re-exports for convenient top-level use
// =============================================================================

pub use kind::ext_to_kind;
pub use path_uri::path_to_file_uri;
pub use tonality::key_to_tonality;
pub use xml::{RekordboxLibrary, RekordboxPlaylist, RekordboxTrack};
