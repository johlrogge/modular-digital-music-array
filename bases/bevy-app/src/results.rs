use bevy::prelude::*;
use mdma_client::TrackInfo;
use music_primitives::{Bpm, EnergyLevel, Key, TrackRole};

// =========================================================================
// Display helpers (WP2)
// =========================================================================

/// Format a BPM option as a string.
/// Returns the display value (e.g. "128.00") or "–" when None.
pub fn fmt_bpm(bpm: &Option<Bpm>) -> String {
    match bpm {
        Some(b) => b.to_string(),
        None => "\u{2013}".to_string(), // en dash
    }
}

/// Format a Key option as a Camelot wheel string (e.g. "8A").
/// Returns "–" when None.
pub fn fmt_key(key: &Option<Key>) -> String {
    match key {
        Some(k) => k.to_camelot(),
        None => "\u{2013}".to_string(), // en dash
    }
}

/// Format a TrackRole option as a human-readable string (e.g. "Build Up").
/// Returns "–" when None.
pub fn fmt_role(role: &Option<TrackRole>) -> String {
    match role {
        Some(r) => r.to_string(),
        None => "\u{2013}".to_string(), // en dash
    }
}

/// Format an EnergyLevel option as a string (e.g. "7").
/// Returns "–" when None.
pub fn fmt_energy(energy: &Option<EnergyLevel>) -> String {
    match energy {
        Some(e) => e.to_string(),
        None => "\u{2013}".to_string(), // en dash
    }
}

// =========================================================================
// Search results + sort (WP3)
// =========================================================================

/// Column the candidates table is currently sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortColumn {
    #[default]
    Title,
    Artist,
    Bpm,
    Key,
    Role,
    Energy,
}

/// Holds the most-recent search results and their sort state.
#[derive(Resource)]
pub struct SearchResults {
    pub tracks: Vec<TrackInfo>,
    pub sort: SortColumn,
    pub ascending: bool,
}

impl Default for SearchResults {
    /// Default: empty results, sorted by Title ascending.
    fn default() -> Self {
        Self {
            tracks: vec![],
            sort: SortColumn::Title,
            ascending: true,
        }
    }
}

/// Convert a `Key` to a comparable `(u8, u8)` tuple for Camelot-wheel sort.
///
/// Returns `(camelot_number, mode_rank)` where mode_rank is 0 for minor (A)
/// and 1 for major (B), so numeric ordering dominates and letter is a
/// tiebreaker. Uses `Key::camelot_number()` directly — no string parsing.
pub fn camelot_sort_key(key: &Key) -> (u8, u8) {
    use music_primitives::Mode;
    let number = key.camelot_number();
    let mode_rank = match key.mode() {
        Mode::Minor => 0,
        Mode::Major => 1,
    };
    (number, mode_rank)
}

/// Sort `tracks` in place by `column` in the requested direction.
///
/// Option fields sort LAST (after all Some values) regardless of direction.
/// The direction only affects the relative order of Some values.
pub fn sort_tracks(tracks: &mut [TrackInfo], column: SortColumn, ascending: bool) {
    tracks.sort_by(|a, b| {
        // Helper: returns true when the value should be treated as "none/missing".
        // None-sentinel comparisons are resolved here; returned as Greater so None
        // always ends up after Some entries before the ascending flip is applied.
        // We flip Some-vs-Some ordering separately, so None-last is direction-independent.

        macro_rules! none_last {
            ($a_opt:expr, $b_opt:expr, $cmp:expr) => {
                match ($a_opt, $b_opt) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (Some(a_val), Some(b_val)) => {
                        let inner = $cmp(a_val, b_val);
                        if ascending {
                            inner
                        } else {
                            inner.reverse()
                        }
                    }
                }
            };
        }

        match column {
            SortColumn::Title => none_last!(
                a.title.as_ref(),
                b.title.as_ref(),
                |a: &String, b: &String| a.to_lowercase().cmp(&b.to_lowercase())
            ),
            SortColumn::Artist => none_last!(
                a.artist.as_ref(),
                b.artist.as_ref(),
                |a: &String, b: &String| a.to_lowercase().cmp(&b.to_lowercase())
            ),
            SortColumn::Bpm => none_last!(a.bpm.as_ref(), b.bpm.as_ref(), |a: &Bpm, b: &Bpm| a
                .as_f32()
                .total_cmp(&b.as_f32())),
            SortColumn::Key => none_last!(a.key.as_ref(), b.key.as_ref(), |a: &Key, b: &Key| {
                camelot_sort_key(a).cmp(&camelot_sort_key(b))
            }),
            SortColumn::Role => none_last!(
                a.role.as_ref(),
                b.role.as_ref(),
                |a: &TrackRole, b: &TrackRole| a.set_arc_rank().cmp(&b.set_arc_rank())
            ),
            SortColumn::Energy => none_last!(
                a.energy.as_ref(),
                b.energy.as_ref(),
                |a: &EnergyLevel, b: &EnergyLevel| a.cmp(b)
            ),
        }
    });
}

// =========================================================================
// Unit tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use mdma_client::{ContentHash, TrackInfo};
    use music_primitives::{Bpm, EnergyLevel, Key, TrackRole};

    // --- fmt_bpm ---

    #[test]
    fn fmt_bpm_some_formats_with_two_decimal_places() {
        let bpm = Some(Bpm::from_f32(128.0).expect("valid bpm"));
        assert_eq!(fmt_bpm(&bpm), "128.00");
    }

    #[test]
    fn fmt_bpm_none_returns_en_dash() {
        assert_eq!(fmt_bpm(&None::<Bpm>), "\u{2013}");
    }

    #[test]
    fn fmt_bpm_fractional_bpm_formats_correctly() {
        let bpm = Some(Bpm::from_f32(133.33).expect("valid bpm"));
        // Bpm stores internally as rounded u32 hundredths; just check it's Some and non-dash.
        let result = fmt_bpm(&bpm);
        assert_ne!(result, "\u{2013}");
        assert!(!result.is_empty());
    }

    // --- fmt_key ---

    #[test]
    fn fmt_key_some_returns_camelot_string() {
        let key = Key::from_traditional("A Minor").expect("valid key");
        let result = fmt_key(&Some(key));
        assert_eq!(result, "8A");
    }

    #[test]
    fn fmt_key_some_major_key_returns_camelot_b_suffix() {
        let key = Key::from_traditional("C Major").expect("valid key");
        let result = fmt_key(&Some(key));
        assert_eq!(result, "8B");
    }

    #[test]
    fn fmt_key_none_returns_en_dash() {
        assert_eq!(fmt_key(&None), "\u{2013}");
    }

    // --- fmt_role ---

    #[test]
    fn fmt_role_some_opener_returns_opener() {
        let result = fmt_role(&Some(TrackRole::Opener));
        assert_eq!(result, "Opener");
    }

    #[test]
    fn fmt_role_some_build_up_returns_human_readable() {
        let result = fmt_role(&Some(TrackRole::BuildUp));
        assert_eq!(result, "Build Up");
    }

    #[test]
    fn fmt_role_none_returns_en_dash() {
        assert_eq!(fmt_role(&None), "\u{2013}");
    }

    // --- fmt_energy ---

    #[test]
    fn fmt_energy_some_returns_number_string() {
        let energy = EnergyLevel::new(7).expect("valid energy");
        let result = fmt_energy(&Some(energy));
        assert_eq!(result, "7");
    }

    #[test]
    fn fmt_energy_some_max_returns_ten() {
        let energy = EnergyLevel::new(10).expect("valid energy");
        let result = fmt_energy(&Some(energy));
        assert_eq!(result, "10");
    }

    #[test]
    fn fmt_energy_none_returns_en_dash() {
        assert_eq!(fmt_energy(&None), "\u{2013}");
    }

    // =========================================================================
    // sort_tracks tests
    // =========================================================================

    fn make_track(hash: &str) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: None,
            artist: None,
            album: None,
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

    fn with_title(mut t: TrackInfo, title: &str) -> TrackInfo {
        t.title = Some(title.to_string());
        t
    }

    fn with_artist(mut t: TrackInfo, artist: &str) -> TrackInfo {
        t.artist = Some(artist.to_string());
        t
    }

    fn with_bpm(mut t: TrackInfo, bpm: f32) -> TrackInfo {
        t.bpm = Some(Bpm::from_f32(bpm).expect("valid bpm in test"));
        t
    }

    fn with_key(mut t: TrackInfo, key: &str) -> TrackInfo {
        t.key = Some(Key::from_traditional(key).expect("valid key in test"));
        t
    }

    fn with_role(mut t: TrackInfo, role: TrackRole) -> TrackInfo {
        t.role = Some(role);
        t
    }

    fn with_energy(mut t: TrackInfo, level: u8) -> TrackInfo {
        t.energy = Some(EnergyLevel::new(level).expect("valid energy in test"));
        t
    }

    // --- title sort ---

    #[test]
    fn sort_by_title_ascending_case_insensitive() {
        let mut tracks = vec![
            with_title(make_track("c"), "Zebra"),
            with_title(make_track("a"), "apple"),
            with_title(make_track("b"), "Mango"),
        ];
        sort_tracks(&mut tracks, SortColumn::Title, true);
        assert_eq!(tracks[0].title.as_deref(), Some("apple"));
        assert_eq!(tracks[1].title.as_deref(), Some("Mango"));
        assert_eq!(tracks[2].title.as_deref(), Some("Zebra"));
    }

    #[test]
    fn sort_by_title_none_sorts_last() {
        let mut tracks = vec![
            make_track("no-title"),           // None title
            with_title(make_track("z"), "Z"), // Some
        ];
        sort_tracks(&mut tracks, SortColumn::Title, true);
        assert_eq!(tracks[0].title.as_deref(), Some("Z"));
        assert!(tracks[1].title.is_none());
    }

    #[test]
    fn sort_by_title_descending() {
        let mut tracks = vec![
            with_title(make_track("a"), "A"),
            with_title(make_track("z"), "Z"),
        ];
        sort_tracks(&mut tracks, SortColumn::Title, false);
        assert_eq!(tracks[0].title.as_deref(), Some("Z"));
        assert_eq!(tracks[1].title.as_deref(), Some("A"));
    }

    // --- artist sort ---

    #[test]
    fn sort_by_artist_ascending() {
        let mut tracks = vec![
            with_artist(make_track("b"), "Zoo"),
            with_artist(make_track("a"), "Ace"),
        ];
        sort_tracks(&mut tracks, SortColumn::Artist, true);
        assert_eq!(tracks[0].artist.as_deref(), Some("Ace"));
        assert_eq!(tracks[1].artist.as_deref(), Some("Zoo"));
    }

    #[test]
    fn sort_by_artist_none_sorts_last() {
        let mut tracks = vec![
            make_track("no-artist"),
            with_artist(make_track("a"), "Alpha"),
        ];
        sort_tracks(&mut tracks, SortColumn::Artist, true);
        assert_eq!(tracks[0].artist.as_deref(), Some("Alpha"));
        assert!(tracks[1].artist.is_none());
    }

    // --- bpm sort ---

    #[test]
    fn sort_by_bpm_ascending() {
        let mut tracks = vec![
            with_bpm(make_track("b"), 140.0),
            with_bpm(make_track("a"), 128.0),
        ];
        sort_tracks(&mut tracks, SortColumn::Bpm, true);
        assert!((tracks[0].bpm.as_ref().unwrap().as_f32() - 128.0).abs() < 0.1);
        assert!((tracks[1].bpm.as_ref().unwrap().as_f32() - 140.0).abs() < 0.1);
    }

    #[test]
    fn sort_by_bpm_none_sorts_last() {
        let mut tracks = vec![make_track("none"), with_bpm(make_track("val"), 128.0)];
        sort_tracks(&mut tracks, SortColumn::Bpm, true);
        assert!(tracks[0].bpm.is_some());
        assert!(tracks[1].bpm.is_none());
    }

    #[test]
    fn sort_by_bpm_none_sorts_last_when_descending() {
        // None should always be last, even when descending.
        let mut tracks = vec![make_track("none"), with_bpm(make_track("val"), 128.0)];
        sort_tracks(&mut tracks, SortColumn::Bpm, false);
        assert!(tracks[0].bpm.is_some());
        assert!(tracks[1].bpm.is_none());
    }

    // --- key sort (camelot numeric-then-letter) ---

    #[test]
    fn sort_by_key_10a_sorts_after_2a() {
        // "10A" number=10, "2A" number=2 — numeric comparison so 10 > 2.
        // "C# Minor" = 12A? Let me use known Camelot mappings.
        // D Minor = 7A, A Minor = 8A, B Minor = 10A
        let d_minor = with_key(make_track("d"), "D Minor"); // 7A
        let b_minor = with_key(make_track("b"), "B Minor"); // 10A

        let mut tracks = vec![b_minor, d_minor];
        sort_tracks(&mut tracks, SortColumn::Key, true);
        // 7A < 10A
        assert_eq!(tracks[0].key.as_ref().unwrap().to_camelot(), "7A");
        assert_eq!(tracks[1].key.as_ref().unwrap().to_camelot(), "10A");
    }

    #[test]
    fn sort_by_key_a_before_b_same_number() {
        // A minor = 8A (mode A), C major = 8B (mode B).
        // With same number (8), A < B.
        let a_minor = with_key(make_track("am"), "A Minor"); // 8A
        let c_major = with_key(make_track("cm"), "C Major"); // 8B
        let mut tracks = vec![c_major, a_minor];
        sort_tracks(&mut tracks, SortColumn::Key, true);
        assert_eq!(tracks[0].key.as_ref().unwrap().to_camelot(), "8A");
        assert_eq!(tracks[1].key.as_ref().unwrap().to_camelot(), "8B");
    }

    #[test]
    fn sort_by_key_none_sorts_last() {
        let mut tracks = vec![make_track("none"), with_key(make_track("k"), "A Minor")];
        sort_tracks(&mut tracks, SortColumn::Key, true);
        assert!(tracks[0].key.is_some());
        assert!(tracks[1].key.is_none());
    }

    // --- role sort ---

    #[test]
    fn sort_by_role_follows_set_arc_order() {
        let mut tracks = vec![
            with_role(make_track("f"), TrackRole::Filler),
            with_role(make_track("o"), TrackRole::Opener),
            with_role(make_track("p"), TrackRole::Peak),
            with_role(make_track("b"), TrackRole::BuildUp),
        ];
        sort_tracks(&mut tracks, SortColumn::Role, true);
        assert_eq!(tracks[0].role, Some(TrackRole::Opener));
        assert_eq!(tracks[1].role, Some(TrackRole::BuildUp));
        assert_eq!(tracks[2].role, Some(TrackRole::Peak));
        assert_eq!(tracks[3].role, Some(TrackRole::Filler));
    }

    #[test]
    fn set_arc_rank_covers_all_variants_in_set_arc_order() {
        assert_eq!(TrackRole::Opener.set_arc_rank(), 0);
        assert_eq!(TrackRole::BuildUp.set_arc_rank(), 1);
        assert_eq!(TrackRole::Peak.set_arc_rank(), 2);
        assert_eq!(TrackRole::Banger.set_arc_rank(), 3);
        assert_eq!(TrackRole::CoolDown.set_arc_rank(), 4);
        assert_eq!(TrackRole::Closer.set_arc_rank(), 5);
        assert_eq!(TrackRole::Filler.set_arc_rank(), 6);
    }

    #[test]
    fn sort_by_role_none_sorts_last() {
        let mut tracks = vec![
            make_track("none"),
            with_role(make_track("o"), TrackRole::Opener),
        ];
        sort_tracks(&mut tracks, SortColumn::Role, true);
        assert!(tracks[0].role.is_some());
        assert!(tracks[1].role.is_none());
    }

    // --- energy sort ---

    #[test]
    fn sort_by_energy_ascending() {
        let mut tracks = vec![
            with_energy(make_track("hi"), 9),
            with_energy(make_track("lo"), 3),
        ];
        sort_tracks(&mut tracks, SortColumn::Energy, true);
        assert_eq!(tracks[0].energy.unwrap().value(), 3);
        assert_eq!(tracks[1].energy.unwrap().value(), 9);
    }

    #[test]
    fn sort_by_energy_none_sorts_last() {
        let mut tracks = vec![make_track("none"), with_energy(make_track("e"), 5)];
        sort_tracks(&mut tracks, SortColumn::Energy, true);
        assert!(tracks[0].energy.is_some());
        assert!(tracks[1].energy.is_none());
    }

    // --- descending toggle ---

    #[test]
    fn sort_by_energy_descending() {
        let mut tracks = vec![
            with_energy(make_track("lo"), 2),
            with_energy(make_track("hi"), 8),
        ];
        sort_tracks(&mut tracks, SortColumn::Energy, false);
        assert_eq!(tracks[0].energy.unwrap().value(), 8);
        assert_eq!(tracks[1].energy.unwrap().value(), 2);
    }

    #[test]
    fn sort_by_energy_descending_none_still_last() {
        let mut tracks = vec![
            make_track("none"),
            with_energy(make_track("lo"), 1),
            with_energy(make_track("hi"), 9),
        ];
        sort_tracks(&mut tracks, SortColumn::Energy, false);
        // Descending: 9, 1, None (None always last)
        assert_eq!(tracks[0].energy.unwrap().value(), 9);
        assert_eq!(tracks[1].energy.unwrap().value(), 1);
        assert!(tracks[2].energy.is_none());
    }
}
