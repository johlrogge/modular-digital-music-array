// Rekordbox XML export utilities.
// Generates Pioneer Rekordbox-compatible XML library files for import
// via Rekordbox File → Import Library.

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
            assert_eq!(path_to_file_uri(path), "file://localhost/music/100%25.flac");
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
    #[derive(Clone)]
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
    #[derive(Clone)]
    pub struct RekordboxPlaylist {
        pub name: String,
        pub track_ids: Vec<u32>,
    }

    /// A full Rekordbox library with tracks and playlists.
    #[derive(Clone)]
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
            out.push_str(&format!("  <COLLECTION Entries=\"{}\">\n", entry_count));

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
        out.push_str(&format!(" BitRate=\"{}\"", track.bitrate.unwrap_or(0)));
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
            assert!(
                xml.contains("<DJ_PLAYLISTS Version=\"1.0.0\">"),
                "missing root"
            );
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
            assert!(
                xml.contains("DateAdded=\"2024-06-15\""),
                "DateAdded missing"
            );
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
            assert!(
                !xml.contains("<TEMPO"),
                "TEMPO should not appear when BPM is None"
            );
            // Self-closing track tag
            assert!(
                xml.contains("/>"),
                "track should be self-closing when no BPM"
            );
        }

        #[test]
        fn xml_playlist_structure() {
            let xml = make_test_library().to_xml();
            assert!(
                xml.contains("NODE Name=\"My Set\""),
                "playlist name missing"
            );
            assert!(
                xml.contains("TRACK Key=\"1\""),
                "playlist track ref missing"
            );
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
// parse module — parse Rekordbox XML into RekordboxLibrary
// =============================================================================

pub mod parse {
    use super::xml::{RekordboxLibrary, RekordboxPlaylist, RekordboxTrack};
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    use thiserror::Error;

    #[derive(Debug, Error)]
    pub enum ParseError {
        #[error("XML parse error: {0}")]
        Xml(#[from] quick_xml::Error),

        #[error("UTF-8 error: {0}")]
        Utf8(#[from] std::str::Utf8Error),
    }

    /// Strip `file://localhost` prefix and percent-decode a Rekordbox location URI.
    ///
    /// Reverse of `path_uri::path_to_file_uri`.
    pub fn parse_location(location: &str) -> Option<String> {
        let path = location
            .strip_prefix("file://localhost")
            .or_else(|| location.strip_prefix("file://"))?;
        Some(percent_decode(path))
    }

    fn percent_decode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (char_to_hex(bytes[i + 1]), char_to_hex(bytes[i + 2])) {
                    out.push((h * 16 + l) as char);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    fn char_to_hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    fn attr_str(attrs: &quick_xml::events::attributes::Attributes, name: &[u8]) -> Option<String> {
        let mut result = None;
        for a in attrs.clone().flatten() {
            if a.key.as_ref() == name {
                if let Ok(val) = a.unescape_value() {
                    result = Some(val.into_owned());
                }
                break;
            }
        }
        result
    }

    fn attr_u32(attrs: &quick_xml::events::attributes::Attributes, name: &[u8]) -> Option<u32> {
        attr_str(attrs, name).and_then(|s| s.parse().ok())
    }

    fn attr_f32(attrs: &quick_xml::events::attributes::Attributes, name: &[u8]) -> Option<f32> {
        attr_str(attrs, name).and_then(|s| s.parse().ok())
    }

    /// Parse a Rekordbox XML export into a `RekordboxLibrary`.
    ///
    /// Parse tolerantly — skips unknown elements, uses defaults for missing optional attributes.
    pub fn parse_xml(xml: &str) -> Result<RekordboxLibrary, ParseError> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);

        let mut tracks: Vec<RekordboxTrack> = Vec::new();
        let mut playlists: Vec<RekordboxPlaylist> = Vec::new();

        #[derive(PartialEq, Clone, Copy)]
        enum Section {
            Other,
            Collection,
            Playlists,
        }

        let mut section = Section::Other;

        // Stack of (playlist_name, track_ids) for nested NODE parsing.
        // Empty name = folder/root placeholder (not emitted as a playlist).
        let mut playlist_stack: Vec<(String, Vec<u32>)> = Vec::new();
        let mut node_depth: u32 = 0;

        loop {
            match reader.read_event()? {
                Event::Eof => break,

                // Empty elements are self-closing: <TAG ... />
                // They never get an End event, so handle them separately
                Event::Empty(e) => {
                    let name_bytes = e.name().into_inner().to_vec();
                    let tag_name = std::str::from_utf8(&name_bytes)?;
                    match tag_name {
                        "TRACK" if section == Section::Collection => {
                            process_collection_track(&e, &mut tracks)?;
                        }
                        "TRACK" if section == Section::Playlists => {
                            let attrs = e.attributes();
                            let key = attr_u32(&attrs, b"Key").unwrap_or(0);
                            if let Some(top) = playlist_stack.last_mut() {
                                top.1.push(key);
                            }
                        }
                        "NODE" if section == Section::Playlists => {
                            // Self-closing NODE (empty folder or empty playlist)
                            let attrs = e.attributes();
                            let node_type = attr_str(&attrs, b"Type").unwrap_or_default();
                            let node_name = attr_str(&attrs, b"Name").unwrap_or_default();
                            if node_type == "1" && !node_name.is_empty() {
                                playlists.push(RekordboxPlaylist {
                                    name: node_name,
                                    track_ids: Vec::new(),
                                });
                            }
                        }
                        _ => {}
                    }
                }

                Event::Start(e) => {
                    let name_bytes = e.name().into_inner().to_vec();
                    let tag_name = std::str::from_utf8(&name_bytes)?;
                    match tag_name {
                        "COLLECTION" => {
                            section = Section::Collection;
                        }
                        "PLAYLISTS" => {
                            section = Section::Playlists;
                        }
                        "TRACK" if section == Section::Collection => {
                            // TRACK with TEMPO child — process it as a collection track
                            process_collection_track(&e, &mut tracks)?;
                        }
                        "NODE" if section == Section::Playlists => {
                            let attrs = e.attributes();
                            let node_type = attr_str(&attrs, b"Type").unwrap_or_default();
                            let node_name = attr_str(&attrs, b"Name").unwrap_or_default();
                            node_depth += 1;
                            if node_type == "1" {
                                playlist_stack.push((node_name, Vec::new()));
                            } else {
                                // Folder or ROOT — placeholder
                                playlist_stack.push((String::new(), Vec::new()));
                            }
                        }
                        _ => {}
                    }
                }

                Event::End(e) => {
                    let name_bytes = e.name().into_inner().to_vec();
                    let tag_name = std::str::from_utf8(&name_bytes)?;
                    match tag_name {
                        "COLLECTION" => {
                            section = Section::Other;
                        }
                        "PLAYLISTS" => {
                            section = Section::Other;
                        }
                        "NODE" if section == Section::Playlists && node_depth > 0 => {
                            node_depth -= 1;
                            if let Some((name, track_ids)) = playlist_stack.pop() {
                                if !name.is_empty() {
                                    playlists.push(RekordboxPlaylist { name, track_ids });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        Ok(RekordboxLibrary { tracks, playlists })
    }

    fn process_collection_track(
        e: &quick_xml::events::BytesStart,
        tracks: &mut Vec<RekordboxTrack>,
    ) -> Result<(), ParseError> {
        let attrs = e.attributes();
        let track_id = attr_u32(&attrs, b"TrackID").unwrap_or(0);
        let name = attr_str(&attrs, b"Name").unwrap_or_default();
        let artist = attr_str(&attrs, b"Artist").unwrap_or_default();
        let album = attr_str(&attrs, b"Album").unwrap_or_default();
        let genre = attr_str(&attrs, b"Genre").unwrap_or_default();
        let kind = attr_str(&attrs, b"Kind").unwrap_or_default();
        let size = attr_str(&attrs, b"Size")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let total_time = attr_u32(&attrs, b"TotalTime").unwrap_or(0);
        let average_bpm = attr_f32(&attrs, b"AverageBpm");
        let tonality = attr_str(&attrs, b"Tonality");
        let track_number = attr_u32(&attrs, b"TrackNumber");
        let disc_number = attr_u32(&attrs, b"DiscNumber");
        let year = attr_str(&attrs, b"Year");
        let label = attr_str(&attrs, b"Label");
        let comment = attr_str(&attrs, b"Comments");
        let date_added = attr_str(&attrs, b"DateAdded");
        let bitrate = attr_u32(&attrs, b"BitRate");
        let sample_rate = attr_u32(&attrs, b"SampleRate");
        let location = attr_str(&attrs, b"Location").unwrap_or_default();

        tracks.push(RekordboxTrack {
            track_id,
            name,
            artist,
            album,
            genre,
            kind,
            size,
            total_time,
            average_bpm,
            tonality,
            track_number,
            disc_number,
            year: year.filter(|s| !s.is_empty()),
            label: label.filter(|s| !s.is_empty()),
            comment: comment.filter(|s| !s.is_empty()),
            date_added: date_added.filter(|s| !s.is_empty()),
            bitrate,
            sample_rate,
            location,
        });
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pretty_assertions::assert_eq;

        const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS Version="1.0.0">
  <PRODUCT Name="rekordbox" Version="6.0.0" Company="AlphaTheta"/>
  <COLLECTION Entries="2">
    <TRACK TrackID="1" Name="Test Track" Artist="Test Artist" Album="Test Album"
           Genre="Techno" Kind="AIFF File" Size="12345678" TotalTime="423"
           AverageBpm="128.00" Tonality="8A" TrackNumber="1" DiscNumber="1"
           Year="2023" Label="Fabric" Comments="great track" Rating="0"
           DateAdded="2024-06-15" BitRate="1411" SampleRate="44100"
           Location="file://localhost/music/Test%20Track.aiff">
      <TEMPO Inizio="0.000" Bpm="128.00" Metro="4/4" Battito="1"/>
    </TRACK>
    <TRACK TrackID="2" Name="Another Track" Artist="Another Artist" Album=""
           Genre="" Kind="WAV File" Size="9000000" TotalTime="360"
           Location="file://localhost/music/another.wav"/>
  </COLLECTION>
  <PLAYLISTS>
    <NODE Type="0" Name="ROOT" Count="1">
      <NODE Name="My Set" Type="1" KeyType="0" Entries="2">
        <TRACK Key="1"/>
        <TRACK Key="2"/>
      </NODE>
    </NODE>
  </PLAYLISTS>
</DJ_PLAYLISTS>"#;

        #[test]
        fn parse_xml_collection_tracks() {
            let lib = parse_xml(SAMPLE_XML).unwrap();
            assert_eq!(lib.tracks.len(), 2);
            let t = &lib.tracks[0];
            assert_eq!(t.track_id, 1);
            assert_eq!(t.name, "Test Track");
            assert_eq!(t.artist, "Test Artist");
            assert_eq!(t.album, "Test Album");
            assert_eq!(t.genre, "Techno");
            assert_eq!(t.kind, "AIFF File");
            assert_eq!(t.size, 12345678);
            assert_eq!(t.total_time, 423);
            assert!((t.average_bpm.unwrap() - 128.0).abs() < 0.01);
            assert_eq!(t.tonality.as_deref(), Some("8A"));
            assert_eq!(t.track_number, Some(1));
            assert_eq!(t.disc_number, Some(1));
            assert_eq!(t.year.as_deref(), Some("2023"));
            assert_eq!(t.label.as_deref(), Some("Fabric"));
            assert_eq!(t.comment.as_deref(), Some("great track"));
            assert_eq!(t.date_added.as_deref(), Some("2024-06-15"));
            assert_eq!(t.bitrate, Some(1411));
            assert_eq!(t.sample_rate, Some(44100));
            assert_eq!(t.location, "file://localhost/music/Test%20Track.aiff");
        }

        #[test]
        fn parse_xml_track_with_minimal_attributes() {
            let lib = parse_xml(SAMPLE_XML).unwrap();
            let t = &lib.tracks[1];
            assert_eq!(t.track_id, 2);
            assert_eq!(t.name, "Another Track");
            assert!(t.average_bpm.is_none());
            assert!(t.tonality.is_none());
            assert!(t.year.is_none());
            assert!(t.label.is_none());
        }

        #[test]
        fn parse_xml_playlist() {
            let lib = parse_xml(SAMPLE_XML).unwrap();
            assert_eq!(lib.playlists.len(), 1);
            let p = &lib.playlists[0];
            assert_eq!(p.name, "My Set");
            assert_eq!(p.track_ids, vec![1, 2]);
        }

        #[test]
        fn parse_location_strips_prefix_and_decodes() {
            assert_eq!(
                parse_location("file://localhost/music/Test%20Track.aiff"),
                Some("/music/Test Track.aiff".to_string())
            );
        }

        #[test]
        fn parse_location_handles_percent_encoded_chars() {
            assert_eq!(
                parse_location("file://localhost/music/Simon%20%26%20Garfunkel/track.mp3"),
                Some("/music/Simon & Garfunkel/track.mp3".to_string())
            );
        }

        #[test]
        fn parse_location_rejects_non_file_uri() {
            assert_eq!(parse_location("http://example.com/track.mp3"), None);
        }

        #[test]
        fn roundtrip_parse_and_generate() {
            use super::super::xml::{RekordboxLibrary, RekordboxPlaylist, RekordboxTrack};

            let original = RekordboxLibrary {
                tracks: vec![RekordboxTrack {
                    track_id: 42,
                    name: "Round Trip".to_string(),
                    artist: "Some Artist".to_string(),
                    album: "An Album".to_string(),
                    genre: "Techno".to_string(),
                    kind: "FLAC File".to_string(),
                    size: 50000000,
                    total_time: 300,
                    average_bpm: Some(130.5),
                    tonality: Some("5B".to_string()),
                    track_number: Some(3),
                    disc_number: None,
                    year: Some("2022".to_string()),
                    label: Some("Some Label".to_string()),
                    comment: None,
                    date_added: Some("2023-01-01".to_string()),
                    bitrate: Some(1411),
                    sample_rate: Some(44100),
                    location: "file://localhost/music/Round%20Trip.flac".to_string(),
                }],
                playlists: vec![RekordboxPlaylist {
                    name: "My Playlist".to_string(),
                    track_ids: vec![42],
                }],
            };

            let xml = original.to_xml();
            let parsed = parse_xml(&xml).unwrap();

            assert_eq!(parsed.tracks.len(), 1);
            let t = &parsed.tracks[0];
            assert_eq!(t.track_id, 42);
            assert_eq!(t.name, "Round Trip");
            assert_eq!(t.artist, "Some Artist");
            assert!((t.average_bpm.unwrap() - 130.5).abs() < 0.01);
            assert_eq!(t.tonality.as_deref(), Some("5B"));
            assert_eq!(t.total_time, 300);
            assert_eq!(t.year.as_deref(), Some("2022"));
            assert_eq!(t.label.as_deref(), Some("Some Label"));

            assert_eq!(parsed.playlists.len(), 1);
            assert_eq!(parsed.playlists[0].name, "My Playlist");
            assert_eq!(parsed.playlists[0].track_ids, vec![42]);
        }

        #[test]
        fn attr_str_unescapes_xml_entities() {
            // Titles containing XML entities must be unescaped when parsed so that
            // track matching works correctly (e.g. &apos; → ' and &amp; → &).
            let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS Version="1.0.0">
  <PRODUCT Name="rekordbox" Version="6.0.0" Company="AlphaTheta"/>
  <COLLECTION Entries="1">
    <TRACK TrackID="1" Name="Love Dove (Back to 90&apos;s)" Artist="DJ &amp; Friends"
           Album="" Genre="" Kind="MP3 File" Size="1000000" TotalTime="300"
           Location="file://localhost/music/track.mp3"/>
  </COLLECTION>
  <PLAYLISTS>
    <NODE Type="0" Name="ROOT" Count="0"/>
  </PLAYLISTS>
</DJ_PLAYLISTS>"#;

            let lib = parse_xml(xml).unwrap();
            assert_eq!(lib.tracks.len(), 1);
            let t = &lib.tracks[0];
            assert_eq!(t.name, "Love Dove (Back to 90's)");
            assert_eq!(t.artist, "DJ & Friends");
        }
    }
}

// =============================================================================
// merge module — incremental export planning and library merging
// =============================================================================

pub mod merge {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    use super::path_uri::path_to_file_uri;
    use super::xml::{RekordboxLibrary, RekordboxPlaylist, RekordboxTrack};

    #[derive(Debug, Clone)]
    pub struct DesiredTrack {
        pub dest_path: PathBuf,
        pub source_id: String,
    }

    #[derive(Debug, Clone)]
    pub struct PlannedTrack {
        pub dest_path: PathBuf,
        pub source_id: String,
    }

    #[derive(Debug, Clone)]
    pub struct DestPathCollision {
        pub dest_path: PathBuf,
        pub source_ids: Vec<String>,
    }

    #[derive(Debug, Clone)]
    pub struct FormatChange {
        pub existing_location: String,
        pub new_dest_path: PathBuf,
        pub source_id: String,
    }

    #[derive(Debug, Default, Clone)]
    pub struct ExportPlan {
        pub to_download: Vec<PlannedTrack>,
        pub to_skip: Vec<String>,
        pub collisions: Vec<DestPathCollision>,
        pub format_changes: Vec<FormatChange>,
    }

    /// A track ready to be merged — every field except `track_id`, which
    /// `merge_export` assigns based on final ordering. Callers cannot
    /// supply a track_id, making the unassigned-id state unrepresentable.
    #[derive(Debug, Clone)]
    pub struct RefreshedTrack {
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
        pub date_added: Option<String>,
        pub bitrate: Option<u32>,
        pub sample_rate: Option<u32>,
        pub location: String,
    }

    pub fn plan_export(
        existing: Option<&RekordboxLibrary>,
        desired: &[DesiredTrack],
        disk_check: impl Fn(&Path) -> bool,
    ) -> ExportPlan {
        let existing_locations: HashSet<String> = existing
            .map(|lib| lib.tracks.iter().map(|t| t.location.clone()).collect())
            .unwrap_or_default();

        // Build (stem_without_ext, parent) → (location, ext) map for format-change detection.
        // We use the existing location's decoded path for comparison.
        let existing_by_stem: Vec<(PathBuf, String, String)> = existing
            .map(|lib| {
                lib.tracks
                    .iter()
                    .filter_map(|t| {
                        let decoded = super::parse::parse_location(&t.location)?;
                        let p = PathBuf::from(&decoded);
                        let stem = p.with_extension("");
                        let ext = p
                            .extension()
                            .map(|e| e.to_string_lossy().to_lowercase())
                            .unwrap_or_default();
                        Some((stem, t.location.clone(), ext))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Collision detection: group by dest_path.
        let mut by_dest: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for dt in desired {
            by_dest
                .entry(dt.dest_path.clone())
                .or_default()
                .push(dt.source_id.clone());
        }
        let collisions: Vec<DestPathCollision> = by_dest
            .iter()
            .filter(|(_, ids)| ids.len() >= 2)
            .map(|(path, ids)| DestPathCollision {
                dest_path: path.clone(),
                source_ids: ids.clone(),
            })
            .collect();

        let mut to_skip = Vec::new();
        let mut to_download = Vec::new();
        let mut format_changes = Vec::new();

        for dt in desired {
            let location = path_to_file_uri(&dt.dest_path);

            if existing_locations.contains(&location) && disk_check(&dt.dest_path) {
                to_skip.push(location);
            } else {
                // Check for format change: same parent+stem, different extension.
                let desired_stem = dt.dest_path.with_extension("");
                let desired_ext = dt
                    .dest_path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                for (existing_stem, existing_loc, existing_ext) in &existing_by_stem {
                    if existing_stem == &desired_stem && existing_ext != &desired_ext {
                        format_changes.push(FormatChange {
                            existing_location: existing_loc.clone(),
                            new_dest_path: dt.dest_path.clone(),
                            source_id: dt.source_id.clone(),
                        });
                    }
                }

                to_download.push(PlannedTrack {
                    dest_path: dt.dest_path.clone(),
                    source_id: dt.source_id.clone(),
                });
            }
        }

        ExportPlan {
            to_download,
            to_skip,
            collisions,
            format_changes,
        }
    }

    pub fn merge_export(
        existing: Option<RekordboxLibrary>,
        refreshed: Vec<RefreshedTrack>,
        playlist_name: &str,
        playlist_locations: &[String],
    ) -> RekordboxLibrary {
        let mut refreshed_by_location: HashMap<String, RefreshedTrack> = refreshed
            .into_iter()
            .map(|t| (t.location.clone(), t))
            .collect();

        // Destructure existing upfront so we can use both tracks and playlists.
        let (existing_tracks, existing_playlists) = existing
            .map(|lib| (lib.tracks, lib.playlists))
            .unwrap_or_default();

        let existing_locations_ordered: Vec<String> =
            existing_tracks.iter().map(|t| t.location.clone()).collect();
        let existing_locations_set: HashSet<String> =
            existing_locations_ordered.iter().cloned().collect();

        // Old-id → location for playlist remapping.
        let old_id_to_location: HashMap<u32, String> = existing_tracks
            .iter()
            .map(|t| (t.track_id, t.location.clone()))
            .collect();

        let mut existing_tracks_by_location: HashMap<String, RekordboxTrack> = existing_tracks
            .into_iter()
            .map(|t| (t.location.clone(), t))
            .collect();

        // Ordered locations: existing first, then new from refreshed.
        let mut ordered_locations = existing_locations_ordered;
        for loc in refreshed_by_location.keys() {
            if !existing_locations_set.contains(loc) {
                ordered_locations.push(loc.clone());
            }
        }

        // Assign sequential ids (1-based). Each location appears exactly once,
        // so .remove is safe and avoids a clone.
        let mut merged: Vec<RekordboxTrack> = Vec::with_capacity(ordered_locations.len());
        for (idx, loc) in ordered_locations.iter().enumerate() {
            let track_id = (idx + 1) as u32;
            let track: RekordboxTrack = if let Some(r) = refreshed_by_location.remove(loc) {
                RekordboxTrack {
                    track_id,
                    name: r.name,
                    artist: r.artist,
                    album: r.album,
                    genre: r.genre,
                    kind: r.kind,
                    size: r.size,
                    total_time: r.total_time,
                    average_bpm: r.average_bpm,
                    tonality: r.tonality,
                    track_number: r.track_number,
                    disc_number: r.disc_number,
                    year: r.year,
                    label: r.label,
                    comment: r.comment,
                    date_added: r.date_added,
                    bitrate: r.bitrate,
                    sample_rate: r.sample_rate,
                    location: r.location,
                }
            } else if let Some(mut existing_track) = existing_tracks_by_location.remove(loc) {
                existing_track.track_id = track_id;
                existing_track
            } else {
                continue;
            };
            merged.push(track);
        }

        let location_to_new_id: HashMap<String, u32> = merged
            .iter()
            .map(|t| (t.location.clone(), t.track_id))
            .collect();

        // Carry over existing playlists (except the named one), remapping track ids.
        let mut rebuilt_playlists: Vec<RekordboxPlaylist> = existing_playlists
            .into_iter()
            .filter(|p| p.name != playlist_name)
            .map(|p| {
                let remapped_ids: Vec<u32> = p
                    .track_ids
                    .iter()
                    .filter_map(|old_id| {
                        let loc = old_id_to_location.get(old_id)?;
                        location_to_new_id.get(loc).copied()
                    })
                    .collect();
                RekordboxPlaylist {
                    name: p.name,
                    track_ids: remapped_ids,
                }
            })
            .collect();

        // Build (or replace) the named playlist.
        // Skip appending when name is empty and locations is empty — stdin mode produces no playlist.
        if !playlist_name.is_empty() || !playlist_locations.is_empty() {
            let named_track_ids: Vec<u32> = playlist_locations
                .iter()
                .filter_map(|loc| {
                    let id = location_to_new_id.get(loc).copied();
                    debug_assert!(
                        id.is_some(),
                        "playlist_locations entry not found in merged: {}",
                        loc
                    );
                    id
                })
                .collect();

            rebuilt_playlists.push(RekordboxPlaylist {
                name: playlist_name.to_string(),
                track_ids: named_track_ids,
            });
        }

        RekordboxLibrary {
            tracks: merged,
            playlists: rebuilt_playlists,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::xml::{RekordboxLibrary, RekordboxPlaylist, RekordboxTrack};
        use super::*;

        fn make_track(id: u32, location: &str) -> RekordboxTrack {
            RekordboxTrack {
                track_id: id,
                name: format!("Track {}", id),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                genre: "Techno".to_string(),
                kind: "AIFF File".to_string(),
                size: 1000,
                total_time: 300,
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
                location: location.to_string(),
            }
        }

        fn make_refreshed(name: &str, location: &str) -> RefreshedTrack {
            RefreshedTrack {
                name: name.to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                genre: "Techno".to_string(),
                kind: "AIFF File".to_string(),
                size: 1000,
                total_time: 300,
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
                location: location.to_string(),
            }
        }

        fn make_library(
            tracks: Vec<RekordboxTrack>,
            playlists: Vec<RekordboxPlaylist>,
        ) -> RekordboxLibrary {
            RekordboxLibrary { tracks, playlists }
        }

        const LOC_A: &str = "file://localhost/music/a.aiff";
        const LOC_B: &str = "file://localhost/music/b.aiff";
        const LOC_C: &str = "file://localhost/music/c.aiff";

        #[test]
        fn merge_empty_existing_matches_fresh_export() {
            let refreshed = vec![
                make_refreshed("Track A", LOC_A),
                make_refreshed("Track B", LOC_B),
            ];
            let playlist_locs = vec![LOC_A.to_string(), LOC_B.to_string()];
            let lib = merge_export(None, refreshed, "My Set", &playlist_locs);
            assert_eq!(lib.tracks.len(), 2);
            assert_eq!(lib.playlists.len(), 1);
            assert_eq!(lib.playlists[0].track_ids.len(), 2);
        }

        #[test]
        fn merge_preserves_untouched_tracks() {
            let existing = make_library(vec![make_track(1, LOC_A), make_track(2, LOC_B)], vec![]);
            // Only refresh LOC_A; LOC_B stays untouched.
            let refreshed_a = make_refreshed("Updated A", LOC_A);
            let lib = merge_export(Some(existing), vec![refreshed_a], "My Set", &[]);
            assert_eq!(lib.tracks.len(), 2);
            let b = lib.tracks.iter().find(|t| t.location == LOC_B).unwrap();
            assert_eq!(b.name, "Track 2");
        }

        #[test]
        fn merge_refreshes_metadata_on_reexport() {
            let existing = make_library(vec![make_track(1, LOC_A)], vec![]);
            let mut refreshed = make_refreshed("Fresh Name", LOC_A);
            refreshed.artist = "Fresh Artist".to_string();
            let lib = merge_export(Some(existing), vec![refreshed], "My Set", &[]);
            let t = &lib.tracks[0];
            assert_eq!(t.name, "Fresh Name");
            assert_eq!(t.artist, "Fresh Artist");
        }

        #[test]
        fn merge_adds_new_tracks_to_collection() {
            let existing = make_library(vec![make_track(1, LOC_A)], vec![]);
            let new_track = make_refreshed("Track B", LOC_B);
            let lib = merge_export(Some(existing), vec![new_track], "My Set", &[]);
            assert_eq!(lib.tracks.len(), 2);
            assert!(lib.tracks.iter().any(|t| t.location == LOC_B));
        }

        #[test]
        fn merge_assigns_sequential_ids() {
            let existing = make_library(vec![make_track(5, LOC_A), make_track(10, LOC_B)], vec![]);
            let refreshed = vec![make_refreshed("Track C", LOC_C)];
            let lib = merge_export(Some(existing), refreshed, "My Set", &[]);
            assert_eq!(lib.tracks.len(), 3);
            let mut ids: Vec<u32> = lib.tracks.iter().map(|t| t.track_id).collect();
            ids.sort();
            assert_eq!(ids, vec![1, 2, 3]);
        }

        #[test]
        fn merge_replaces_named_playlist() {
            let existing = make_library(
                vec![make_track(1, LOC_A), make_track(2, LOC_B)],
                vec![RekordboxPlaylist {
                    name: "My Set".to_string(),
                    track_ids: vec![1, 2],
                }],
            );
            // Refresh LOC_C and make it the only entry in the named playlist.
            let refreshed = vec![make_refreshed("Track C", LOC_C)];
            let playlist_locs = vec![LOC_C.to_string()];
            let lib = merge_export(Some(existing), refreshed, "My Set", &playlist_locs);
            let named = lib.playlists.iter().find(|p| p.name == "My Set").unwrap();
            assert_eq!(named.track_ids.len(), 1);
            let c_id = lib
                .tracks
                .iter()
                .find(|t| t.location == LOC_C)
                .unwrap()
                .track_id;
            assert_eq!(named.track_ids[0], c_id);
        }

        #[test]
        fn merge_preserves_other_playlists_with_remapped_ids() {
            let existing = make_library(
                vec![make_track(1, LOC_A), make_track(2, LOC_B)],
                vec![
                    RekordboxPlaylist {
                        name: "Other".to_string(),
                        track_ids: vec![1, 2],
                    },
                    RekordboxPlaylist {
                        name: "My Set".to_string(),
                        track_ids: vec![1],
                    },
                ],
            );
            // Add a new track so IDs shift.
            let refreshed = vec![make_refreshed("Track C", LOC_C)];
            let lib = merge_export(Some(existing), refreshed, "My Set", &[]);
            // "Other" playlist should survive.
            let other = lib.playlists.iter().find(|p| p.name == "Other").unwrap();
            // Both LOC_A and LOC_B should be resolvable.
            let a_id = lib
                .tracks
                .iter()
                .find(|t| t.location == LOC_A)
                .unwrap()
                .track_id;
            let b_id = lib
                .tracks
                .iter()
                .find(|t| t.location == LOC_B)
                .unwrap()
                .track_id;
            assert!(other.track_ids.contains(&a_id));
            assert!(other.track_ids.contains(&b_id));
        }

        #[test]
        fn merge_named_playlist_creates_if_absent() {
            let existing = make_library(vec![make_track(1, LOC_A)], vec![]);
            let playlist_locs = vec![LOC_A.to_string()];
            let lib = merge_export(Some(existing), vec![], "New Playlist", &playlist_locs);
            assert!(lib.playlists.iter().any(|p| p.name == "New Playlist"));
        }

        #[test]
        fn merge_skips_empty_playlist_name() {
            // stdin mode passes ("", &[]) — no playlist should be appended.
            let existing = make_library(vec![make_track(1, LOC_A)], vec![]);
            let lib = merge_export(Some(existing), vec![], "", &[]);
            assert!(
                lib.playlists.is_empty(),
                "empty playlist_name + empty locations must not append a playlist"
            );
        }

        #[test]
        fn merge_export_assigns_ids_to_refreshed_tracks() {
            // Caller cannot supply a track_id (RefreshedTrack has none).
            // Verify merge_export assigns sequential 1-based ids regardless of insertion order.
            let refreshed = vec![
                make_refreshed("Track B", LOC_B),
                make_refreshed("Track A", LOC_A),
            ];
            let lib = merge_export(None, refreshed, "My Set", &[]);
            let ids: HashSet<u32> = lib.tracks.iter().map(|t| t.track_id).collect();
            assert_eq!(ids, HashSet::from([1, 2]));
            // Every id must be positive — the sentinel zero is unreachable.
            assert!(lib.tracks.iter().all(|t| t.track_id > 0));
        }

        // --- plan_export tests ---

        #[test]
        fn plan_export_skips_when_location_and_file_exist() {
            let path = PathBuf::from("/music/a.aiff");
            let loc = path_to_file_uri(&path);
            let existing = make_library(
                vec![{
                    let mut t = make_track(1, &loc);
                    t.location = loc.clone();
                    t
                }],
                vec![],
            );
            let desired = vec![DesiredTrack {
                dest_path: path.clone(),
                source_id: "id1".to_string(),
            }];
            let plan = plan_export(Some(&existing), &desired, |_| true);
            assert_eq!(plan.to_skip.len(), 1);
            assert!(plan.to_download.is_empty());
        }

        #[test]
        fn plan_export_redownloads_when_file_missing_but_xml_has_location() {
            let path = PathBuf::from("/music/a.aiff");
            let loc = path_to_file_uri(&path);
            let existing = make_library(
                vec![{
                    let mut t = make_track(1, &loc);
                    t.location = loc.clone();
                    t
                }],
                vec![],
            );
            let desired = vec![DesiredTrack {
                dest_path: path.clone(),
                source_id: "id1".to_string(),
            }];
            // disk_check returns false — file not on disk.
            let plan = plan_export(Some(&existing), &desired, |_| false);
            assert!(plan.to_skip.is_empty());
            assert_eq!(plan.to_download.len(), 1);
        }

        #[test]
        fn plan_export_downloads_new_track() {
            let path = PathBuf::from("/music/new.aiff");
            let desired = vec![DesiredTrack {
                dest_path: path.clone(),
                source_id: "id1".to_string(),
            }];
            let plan = plan_export(None, &desired, |_| false);
            assert!(plan.to_skip.is_empty());
            assert_eq!(plan.to_download.len(), 1);
            assert_eq!(plan.to_download[0].source_id, "id1");
        }

        #[test]
        fn plan_export_flags_dest_path_collision() {
            let path = PathBuf::from("/music/a.aiff");
            let desired = vec![
                DesiredTrack {
                    dest_path: path.clone(),
                    source_id: "id1".to_string(),
                },
                DesiredTrack {
                    dest_path: path.clone(),
                    source_id: "id2".to_string(),
                },
            ];
            let plan = plan_export(None, &desired, |_| false);
            assert_eq!(plan.collisions.len(), 1);
            assert_eq!(plan.collisions[0].source_ids.len(), 2);
        }

        #[test]
        fn plan_export_flags_format_extension_change() {
            // Existing XML has /x/y.aiff; desired has /x/y.wav.
            let existing_path = PathBuf::from("/music/y.aiff");
            let existing_loc = path_to_file_uri(&existing_path);
            let existing = make_library(
                vec![{
                    let mut t = make_track(1, &existing_loc);
                    t.location = existing_loc.clone();
                    t
                }],
                vec![],
            );

            let new_path = PathBuf::from("/music/y.wav");
            let desired = vec![DesiredTrack {
                dest_path: new_path.clone(),
                source_id: "id1".to_string(),
            }];
            let plan = plan_export(Some(&existing), &desired, |_| false);
            assert_eq!(plan.format_changes.len(), 1);
            assert_eq!(plan.format_changes[0].existing_location, existing_loc);
            assert_eq!(plan.format_changes[0].new_dest_path, new_path);
        }
    }
}

// =============================================================================
// Re-exports for convenient top-level use
// =============================================================================

pub use kind::ext_to_kind;
pub use merge::{
    merge_export, plan_export, DesiredTrack, DestPathCollision, ExportPlan, FormatChange,
    PlannedTrack, RefreshedTrack,
};
pub use parse::{parse_location, parse_xml, ParseError};
pub use path_uri::path_to_file_uri;
pub use tonality::key_to_tonality;
pub use xml::{RekordboxLibrary, RekordboxPlaylist, RekordboxTrack};
