use crate::selection::SelectionState;
use crate::theme::{ACCENT2, BG_ELEVATED, TEXT_PRIMARY, TEXT_TERTIARY, WARNING};
use corsett::{
    shortener::{FreeText, RightEllipsis},
    ColumnSizingConfigBuilder, RemovalPolicy, Row, Score, Shorten, ShortenAny,
};
use mdma_client::TrackInfo;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem},
    Frame,
};

// =============================================================================
// Corsett column types
// =============================================================================

struct ColArtist(String);
struct ColTitle(String);
struct ColBpm(String);
struct ColKey(String);
struct ColDuration(String);

impl AsRef<str> for ColArtist {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for ColTitle {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for ColBpm {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for ColKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for ColDuration {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Shorten for ColArtist {
    type Algorithm = RightEllipsis<'…', FreeText>;
}
impl Shorten for ColTitle {
    type Algorithm = RightEllipsis<'…', FreeText>;
}
impl Shorten for ColBpm {
    type Algorithm = FreeText;
}
impl Shorten for ColKey {
    type Algorithm = FreeText;
}
impl Shorten for ColDuration {
    type Algorithm = FreeText;
}

// =============================================================================
// Row type — 5 columns: [artist, title, bpm, key, duration]
// =============================================================================

struct TrackRow {
    artist: ColArtist,
    title: ColTitle,
    bpm: ColBpm,
    key: ColKey,
    duration: ColDuration,
}

impl Row<5> for TrackRow {
    fn get_cell(&self, index: usize) -> &dyn ShortenAny {
        match index {
            0 => &self.artist,
            1 => &self.title,
            2 => &self.bpm,
            3 => &self.key,
            4 => &self.duration,
            _ => panic!("TrackRow only has 5 columns, index {index} out of bounds"),
        }
    }
}

impl TrackRow {
    fn from_track(track: &TrackInfo) -> Self {
        Self {
            artist: ColArtist(track.artist.clone().unwrap_or_default()),
            title: ColTitle(
                track
                    .title
                    .clone()
                    .unwrap_or_else(|| "(unknown)".to_string()),
            ),
            bpm: ColBpm(
                track
                    .bpm
                    .map(|b| format!("{:>3}bpm", b.as_u32()))
                    .unwrap_or_default(),
            ),
            key: ColKey(track.key.map(|k| k.to_camelot()).unwrap_or_default()),
            duration: ColDuration(
                track
                    .duration
                    .map(|d| format!("[{}]", d))
                    .unwrap_or_default(),
            ),
        }
    }
}

// =============================================================================
// Public API
// =============================================================================

/// Per-column sizing result.
///
/// `col_widths[i] == 0` means that column was removed by corsett under space pressure.
/// Columns: 0 = artist, 1 = title, 2 = bpm, 3 = key, 4 = duration.
pub struct SizedColumns {
    /// Padded/truncated cell strings per row: `[artist, title, bpm, key, duration]`.
    pub cells: Vec<[String; 5]>,
    /// Max visual width per column (0 = column removed).
    pub col_widths: [usize; 5],
}

const GAP_SIZE: usize = 2;
const SEP: &str = " \u{2014} "; // " — " (3 chars: space, em-dash, space)

/// Size track columns for `available_width` terminal columns using corsett.
///
/// Removal order (first removed → last):
///   key/duration (cols 3,4) — removed first (key is narrower so removed before duration)
///   bpm/artist   (cols 2,0) — removed next (bpm is narrower so goes before artist)
///   title        (col 1)    — never removed
///
/// Column removal is decided upfront based on natural content widths and available budget.
/// Corsett handles fine-grained shrinking of the remaining visible columns.
pub fn size_track_columns(tracks: &[&TrackInfo], available_width: u16) -> SizedColumns {
    if tracks.is_empty() {
        return SizedColumns {
            cells: vec![],
            col_widths: [0; 5],
        };
    }

    let rows: Vec<TrackRow> = tracks.iter().map(|t| TrackRow::from_track(t)).collect();

    // Compute natural max content width per column across all rows.
    let mut natural_widths = [0usize; 5];
    for row in &rows {
        natural_widths[0] = natural_widths[0].max(row.artist.0.chars().count());
        natural_widths[1] = natural_widths[1].max(row.title.0.chars().count());
        natural_widths[2] = natural_widths[2].max(row.bpm.0.chars().count());
        natural_widths[3] = natural_widths[3].max(row.key.0.chars().count());
        natural_widths[4] = natural_widths[4].max(row.duration.0.chars().count());
    }

    // Subtract gap overhead (4 gaps between 5 columns).
    // The separator (" — ") is rendered conditionally at render time (only when
    // artist col_width > 0) so corsett must NOT reserve space for it.
    let gap_overhead = (5 - 1) * GAP_SIZE;
    let available_content = (available_width as usize).saturating_sub(gap_overhead);

    // Remove columns in priority order until natural total fits in available_content.
    // Removal order: key(3), duration(4), bpm(2), artist(0); title(1) is never removed.
    const REMOVAL_ORDER: [usize; 4] = [3, 4, 2, 0];
    let mut removed = [false; 5];
    let mut total_natural: usize = natural_widths.iter().sum();
    for &col in &REMOVAL_ORDER {
        if total_natural <= available_content {
            break;
        }
        total_natural = total_natural.saturating_sub(natural_widths[col]);
        removed[col] = true;
    }

    let config = ColumnSizingConfigBuilder::<5>::new()
        .terminal_width(available_content)
        .gap_size(GAP_SIZE)
        .max_depth(200)
        .removal_policies([
            RemovalPolicy::BelowScore(Score::MINIMAL), // artist   — removed near-last
            RemovalPolicy::Never,                      // title    — never removed
            RemovalPolicy::BelowScore(Score::MINIMAL), // bpm      — removed before artist (narrower)
            RemovalPolicy::BelowScore(Score::BASIC),   // key      — removed first
            RemovalPolicy::BelowScore(Score::BASIC),   // duration — removed first
        ])
        .build();

    let resized = corsett::resize_columns(config, &rows);

    // Max visual width per column across all rows.
    let mut col_widths = [0usize; 5];
    for row in &resized {
        for (i, cell) in row.iter().enumerate() {
            let w = cell.chars().count();
            if w > col_widths[i] {
                col_widths[i] = w;
            }
        }
    }

    // Force col_widths to 0 for any column we decided to remove above.
    // This ensures that even if corsett shrunk (rather than removed) a column,
    // it is treated as absent by the render layer.
    for col in 0..5 {
        if removed[col] {
            col_widths[col] = 0;
        }
    }

    let cells = resized
        .into_iter()
        .map(|[a, t, b, k, d]| {
            [
                fit_cell(&a, col_widths[0]),
                fit_cell(&t, col_widths[1]),
                fit_cell(&b, col_widths[2]),
                fit_cell(&k, col_widths[3]),
                fit_cell(&d, col_widths[4]),
            ]
        })
        .collect();

    SizedColumns { cells, col_widths }
}

/// Fit a string into exactly `width` visible chars: pad right with spaces if shorter,
/// truncate with `…` if longer. Returns empty string when `width == 0`.
fn fit_cell(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let s = s.replace('\n', " ");
    let len = s.chars().count();
    if len > width {
        if width > 1 {
            let truncated: String = s.chars().take(width - 1).collect();
            format!("{}…", truncated)
        } else {
            s.chars().take(width).collect()
        }
    } else if len < width {
        format!("{}{}", s, " ".repeat(width - len))
    } else {
        s.to_string()
    }
}

// =============================================================================
// Render
// =============================================================================

/// Render a list of tracks respecting the SelectionState's visibility and selection.
///
/// The block title and borders are provided by the caller.
pub fn render_track_list(
    f: &mut Frame,
    area: Rect,
    tracks: &[TrackInfo],
    selection: &SelectionState,
    block: Block,
) {
    let visible_tracks: Vec<&TrackInfo> = selection
        .visible_to_data
        .iter()
        .map(|&data_idx| &tracks[data_idx])
        .collect();

    let sized = size_track_columns(&visible_tracks, area.width);

    let items: Vec<ListItem> = selection
        .visible_to_data
        .iter()
        .enumerate()
        .zip(sized.cells.into_iter())
        .map(
            |((vis_idx, &_data_idx), [artist_str, title_str, bpm_str, key_str, dur_str])| {
                let is_cursor = selection.cursor_position() == Some(vis_idx);
                let is_selected = selection.selected.contains(&vis_idx);

                let (artist_color, title_color, meta_color, bg) = if is_cursor && is_selected {
                    (Color::Black, Color::Black, Color::Black, WARNING)
                } else if is_cursor {
                    (Color::Black, Color::Black, Color::Black, ACCENT2)
                } else if is_selected {
                    (WARNING, WARNING, TEXT_TERTIARY, Color::Reset)
                } else {
                    (ACCENT2, TEXT_PRIMARY, TEXT_TERTIARY, Color::Reset)
                };

                let mut spans: Vec<Span> = Vec::with_capacity(9);

                // Artist + separator — only when artist column is non-zero width
                if sized.col_widths[0] > 0 {
                    spans.push(Span::styled(
                        artist_str,
                        Style::default().fg(artist_color).bg(bg),
                    ));
                    spans.push(Span::styled(SEP, Style::default().fg(meta_color).bg(bg)));
                }

                // Title — always present
                spans.push(Span::styled(
                    title_str,
                    Style::default()
                        .fg(title_color)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ));

                // BPM — conditional
                if sized.col_widths[2] > 0 {
                    spans.push(Span::styled(
                        format!("  {}", bpm_str),
                        Style::default().fg(meta_color).bg(bg),
                    ));
                }

                // Key — conditional
                if sized.col_widths[3] > 0 {
                    spans.push(Span::styled(
                        format!("  {}", key_str),
                        Style::default().fg(meta_color).bg(bg),
                    ));
                }

                // Duration — conditional
                if sized.col_widths[4] > 0 {
                    spans.push(Span::styled(
                        format!("  {}", dur_str),
                        Style::default().fg(meta_color).bg(bg),
                    ));
                }

                ListItem::new(Line::from(spans)).style(Style::default().bg(if is_cursor {
                    BG_ELEVATED
                } else {
                    Color::Reset
                }))
            },
        )
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default());

    let mut ls = selection.list_state.clone();
    f.render_stateful_widget(list, area, &mut ls);
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_protocol::DurationSeconds;
    use mdma_client::ContentHash;
    use music_primitives::{Bpm, Key, Mode, PitchClass};

    fn make_track(
        artist: Option<&str>,
        title: Option<&str>,
        bpm: Option<u32>,
        key: Option<Key>,
        duration_secs: Option<u32>,
    ) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new("test-hash"),
            title: title.map(str::to_string),
            artist: artist.map(str::to_string),
            album: None,
            duration: duration_secs.map(DurationSeconds::new),
            bpm: bpm.map(|b| Bpm::from_u32(b).unwrap()),
            key,
            blob_path: None,
            cover_art_path: None,
            track_number: None,
            disc_number: None,
            added: None,
            started: None,
            stopped: None,
        }
    }

    fn make_full_track() -> TrackInfo {
        make_track(
            Some("Aphex Twin"),
            Some("Windowlicker"),
            Some(133),
            Some(Key::new(PitchClass::A, Mode::Minor)),
            Some(6 * 60 + 17),
        )
    }

    // -------------------------------------------------------------------------
    // fit_cell tests (unchanged behaviour)
    // -------------------------------------------------------------------------

    #[test]
    fn fit_cell_pads_short_string_to_width() {
        let result = fit_cell("hello", 10);
        assert_eq!(result.chars().count(), 10);
        assert_eq!(result, "hello     ");
    }

    #[test]
    fn fit_cell_returns_exact_string_at_width() {
        let result = fit_cell("hello", 5);
        assert_eq!(result.chars().count(), 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn fit_cell_truncates_long_string_with_ellipsis() {
        let result = fit_cell("hello world", 8);
        assert_eq!(result.chars().count(), 8);
        assert!(result.ends_with('…'));
        assert_eq!(result, "hello w…");
    }

    #[test]
    fn fit_cell_width_zero_returns_empty() {
        assert_eq!(fit_cell("hello", 0), "");
    }

    #[test]
    fn fit_cell_width_one_returns_single_char_for_long_string() {
        let result = fit_cell("hello", 1);
        assert_eq!(result.chars().count(), 1);
        assert_eq!(result, "h");
    }

    #[test]
    fn fit_cell_handles_multibyte_chars() {
        let result = fit_cell("héllo", 5);
        assert_eq!(result.chars().count(), 5);
        assert_eq!(result, "héllo");

        let result = fit_cell("héllo world", 6);
        assert_eq!(result.chars().count(), 6);
        assert!(result.ends_with('…'));
    }

    // -------------------------------------------------------------------------
    // size_track_columns: wide terminal — all columns visible
    // -------------------------------------------------------------------------

    #[test]
    fn wide_terminal_all_columns_visible() {
        let track = make_full_track();
        let tracks = vec![&track];
        let sized = size_track_columns(&tracks, 200);

        assert!(
            sized.col_widths[0] > 0,
            "artist should be visible at 200 cols"
        );
        assert!(
            sized.col_widths[1] > 0,
            "title should be visible at 200 cols"
        );
        assert!(sized.col_widths[2] > 0, "bpm should be visible at 200 cols");
        assert!(sized.col_widths[3] > 0, "key should be visible at 200 cols");
        assert!(
            sized.col_widths[4] > 0,
            "duration should be visible at 200 cols"
        );
    }

    // -------------------------------------------------------------------------
    // size_track_columns: narrow terminal — duration and key removed first
    // -------------------------------------------------------------------------

    #[test]
    fn narrow_terminal_duration_removed() {
        let track = make_full_track();
        let tracks = vec![&track];
        // 40 cols should be enough for artist+title+bpm but squeeze out duration
        let sized = size_track_columns(&tracks, 40);

        assert_eq!(
            sized.col_widths[4], 0,
            "duration should be removed at 40 cols"
        );
        assert!(sized.col_widths[1] > 0, "title must never be removed");
    }

    #[test]
    fn very_narrow_terminal_bpm_and_key_removed() {
        let track = make_full_track();
        let tracks = vec![&track];
        // 25 cols — only artist+title should remain (bpm/key/dur gone)
        let sized = size_track_columns(&tracks, 25);

        assert_eq!(sized.col_widths[2], 0, "bpm should be removed at 25 cols");
        assert!(sized.col_widths[1] > 0, "title must never be removed");
    }

    // -------------------------------------------------------------------------
    // size_track_columns: extremely narrow — artist removed, title stays
    // -------------------------------------------------------------------------

    #[test]
    fn extremely_narrow_artist_removed_title_stays() {
        let track = make_full_track();
        let tracks = vec![&track];
        // 10 cols — artist must go, title must survive
        let sized = size_track_columns(&tracks, 10);

        assert_eq!(
            sized.col_widths[0], 0,
            "artist should be removed at 10 cols"
        );
        assert!(sized.col_widths[1] > 0, "title must never be removed");
    }

    // -------------------------------------------------------------------------
    // size_track_columns: separator implicit from col_widths[0]
    // -------------------------------------------------------------------------

    #[test]
    fn no_artist_means_no_separator_needed() {
        let track = make_full_track();
        let tracks = vec![&track];
        let sized = size_track_columns(&tracks, 10);

        // When artist col_width is 0 the render loop skips artist+separator.
        // This test checks the contract: col_widths[0] == 0 is the signal.
        assert_eq!(sized.col_widths[0], 0);
    }

    // -------------------------------------------------------------------------
    // size_track_columns: empty input
    // -------------------------------------------------------------------------

    #[test]
    fn empty_tracks_returns_empty_cells() {
        let tracks: Vec<&TrackInfo> = vec![];
        let sized = size_track_columns(&tracks, 80);
        assert!(sized.cells.is_empty());
    }

    // -------------------------------------------------------------------------
    // size_track_columns: cells count matches input
    // -------------------------------------------------------------------------

    #[test]
    fn cells_count_matches_track_count() {
        let t1 = make_full_track();
        let t2 = make_track(None, Some("Other Track"), None, None, None);
        let tracks = vec![&t1, &t2];
        let sized = size_track_columns(&tracks, 120);
        assert_eq!(sized.cells.len(), 2);
    }

    // -------------------------------------------------------------------------
    // TrackRow formatting
    // -------------------------------------------------------------------------

    #[test]
    fn bpm_formatted_as_right_aligned_with_suffix() {
        let track = make_track(None, Some("T"), Some(128), None, None);
        let row = TrackRow::from_track(&track);
        assert_eq!(row.bpm.as_ref(), "128bpm");
    }

    #[test]
    fn bpm_low_value_right_padded() {
        let track = make_track(None, Some("T"), Some(90), None, None);
        let row = TrackRow::from_track(&track);
        assert_eq!(row.bpm.as_ref(), " 90bpm");
    }

    #[test]
    fn key_formatted_as_camelot() {
        let key = Key::new(PitchClass::A, Mode::Minor);
        let track = make_track(None, Some("T"), None, Some(key), None);
        let row = TrackRow::from_track(&track);
        assert_eq!(row.key.as_ref(), "8A");
    }

    #[test]
    fn duration_formatted_as_bracketed_mm_ss() {
        let track = make_track(None, Some("T"), None, None, Some(6 * 60 + 17));
        let row = TrackRow::from_track(&track);
        assert_eq!(row.duration.as_ref(), "[6:17]");
    }

    #[test]
    fn missing_artist_becomes_empty_string() {
        let track = make_track(None, Some("T"), None, None, None);
        let row = TrackRow::from_track(&track);
        assert_eq!(row.artist.as_ref(), "");
    }

    #[test]
    fn missing_title_becomes_unknown() {
        let track = make_track(Some("A"), None, None, None, None);
        let row = TrackRow::from_track(&track);
        assert_eq!(row.title.as_ref(), "(unknown)");
    }
}
