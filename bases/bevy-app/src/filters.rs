use bevy::prelude::*;
use library_search::{parse_key_query, parse_numeric_query, TrackQuery};
use music_primitives::TrackRole;

/// Filter input state for the DJ Workspace side panel.
///
/// Fields are `pub` so `ui.rs` can bind them to `TextEdit`/`ComboBox` widgets.
#[derive(Resource, Default)]
pub struct FilterState {
    pub bpm_text: String,
    pub key_text: String,
    /// None means "(none)" in the combo box.
    pub role: Option<TrackRole>,
    pub energy_text: String,
    /// Set when `build_track_query` returns an error.
    pub error: Option<String>,
    /// True while a search is in flight.
    pub searching: bool,
}

/// Build a `TrackQuery` from the current filter text and role.
///
/// - Blank/whitespace text fields produce `None` for that field.
/// - Parse errors produce `Err("FieldName: <parser message>")`.
/// - All four fields empty → `Err("no filters set")`.
pub fn build_track_query(state: &FilterState) -> Result<TrackQuery, String> {
    let bpm = if state.bpm_text.trim().is_empty() {
        None
    } else {
        Some(parse_numeric_query(state.bpm_text.trim()).map_err(|e| format!("BPM: {e}"))?)
    };

    let key = if state.key_text.trim().is_empty() {
        None
    } else {
        Some(parse_key_query(state.key_text.trim()).map_err(|e| format!("Key: {e}"))?)
    };

    let energy = if state.energy_text.trim().is_empty() {
        None
    } else {
        Some(parse_numeric_query(state.energy_text.trim()).map_err(|e| format!("Energy: {e}"))?)
    };

    let role = state.role;

    if bpm.is_none() && key.is_none() && energy.is_none() && role.is_none() {
        return Err("no filters set".to_string());
    }

    Ok(TrackQuery {
        bpm,
        key,
        role,
        energy,
        ..Default::default()
    })
}

// =========================================================================
// Unit tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use library_search::{KeyQuery, NumericQuery};

    fn state_with(bpm: &str, key: &str, role: Option<TrackRole>, energy: &str) -> FilterState {
        FilterState {
            bpm_text: bpm.to_string(),
            key_text: key.to_string(),
            role,
            energy_text: energy.to_string(),
            error: None,
            searching: false,
        }
    }

    // --- BPM parsing ---

    #[test]
    fn bpm_tolerance_128_pm_3_produces_tolerance_variant() {
        let s = state_with("128+-3", "", None, "");
        let q = build_track_query(&s).unwrap();
        match q.bpm {
            Some(NumericQuery::Tolerance { value, up, down }) => {
                assert!((value - 128.0).abs() < f32::EPSILON);
                assert!((up - 3.0).abs() < f32::EPSILON);
                assert!((down - 3.0).abs() < f32::EPSILON);
            }
            other => panic!("Expected Tolerance, got {:?}", other),
        }
    }

    #[test]
    fn bpm_range_126_to_130_produces_range_variant() {
        let s = state_with("126..130", "", None, "");
        let q = build_track_query(&s).unwrap();
        match q.bpm {
            Some(NumericQuery::Range(lo, hi)) => {
                assert!((lo - 126.0).abs() < f32::EPSILON);
                assert!((hi - 130.0).abs() < f32::EPSILON);
            }
            other => panic!("Expected Range, got {:?}", other),
        }
    }

    #[test]
    fn bpm_exact_128_produces_exact_variant() {
        let s = state_with("128", "", None, "");
        let q = build_track_query(&s).unwrap();
        match q.bpm {
            Some(NumericQuery::Exact(v)) => {
                assert!((v - 128.0).abs() < f32::EPSILON);
            }
            other => panic!("Expected Exact, got {:?}", other),
        }
    }

    // --- Key parsing ---

    #[test]
    fn key_8a_tilde_parses_without_error() {
        // "8A~" is a valid key query (exact 8A, ~ hint is accepted by parser).
        let s = state_with("", "8A~", None, "");
        let q = build_track_query(&s).unwrap();
        // The parser returns Exact for a bare key with ~ (no tolerance digits).
        match q.key {
            Some(KeyQuery::Exact { number, .. }) => {
                assert_eq!(number, 8);
            }
            other => panic!("Expected Exact key, got {:?}", other),
        }
    }

    #[test]
    fn key_8a_pm1_tilde_produces_tolerance_with_include_relative() {
        let s = state_with("", "8A+-1~", None, "");
        let q = build_track_query(&s).unwrap();
        match q.key {
            Some(KeyQuery::Tolerance {
                number,
                include_relative,
                tolerance_up,
                tolerance_down,
                ..
            }) => {
                assert_eq!(number, 8);
                assert!(include_relative);
                assert_eq!(tolerance_up, 1);
                assert_eq!(tolerance_down, 1);
            }
            other => panic!("Expected Tolerance key, got {:?}", other),
        }
    }

    // --- Energy parsing ---

    #[test]
    fn energy_range_5_to_8_produces_range_variant() {
        let s = state_with("", "", None, "5..8");
        let q = build_track_query(&s).unwrap();
        match q.energy {
            Some(NumericQuery::Range(lo, hi)) => {
                assert!((lo - 5.0).abs() < f32::EPSILON);
                assert!((hi - 8.0).abs() < f32::EPSILON);
            }
            other => panic!("Expected Range, got {:?}", other),
        }
    }

    // --- Role only ---

    #[test]
    fn role_only_query_succeeds_with_no_text_fields() {
        let s = state_with("", "", Some(TrackRole::Peak), "");
        let q = build_track_query(&s).unwrap();
        assert_eq!(q.role, Some(TrackRole::Peak));
        assert!(q.bpm.is_none());
        assert!(q.key.is_none());
        assert!(q.energy.is_none());
    }

    // --- Error cases ---

    #[test]
    fn invalid_bpm_text_returns_err_naming_bpm_field() {
        let s = state_with("not_a_number", "", None, "");
        let err = build_track_query(&s).unwrap_err();
        assert!(
            err.starts_with("BPM:"),
            "Expected error to start with 'BPM:', got: {err}"
        );
    }

    #[test]
    fn all_empty_fields_returns_no_filters_set_error() {
        let s = state_with("", "", None, "");
        let err = build_track_query(&s).unwrap_err();
        assert_eq!(err, "no filters set");
    }

    // --- Other fields are left None ---

    #[test]
    fn query_leaves_unrelated_fields_as_none() {
        let s = state_with("128", "", None, "");
        let q = build_track_query(&s).unwrap();
        assert!(q.any_text.is_none());
        assert!(q.artist.is_none());
        assert!(q.title.is_none());
        assert!(q.key.is_none());
        assert!(q.energy.is_none());
        assert!(q.role.is_none());
    }
}
