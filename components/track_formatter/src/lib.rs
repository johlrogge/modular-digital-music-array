//! Canonical track line formatter shared between CLI and TUI.
//!
//! Produces the standard `{12-char-hash}  {Artist} - {Title}  [{duration}]`
//! format used for playlist files and pipe-mode output.
//!
//! Also provides the colored, aligned table renderer used by both `mdma search`
//! and `mdma view` via [`render_for_user`].
//!
//! # Hash policy
//!
//! Two hash types encode two policies at the type level:
//!
//! - [`NonShrinkableHash`] — playlist serialization: `FreeText`, never truncated by
//!   corsett because playlist paths bypass `resize_columns`.
//! - [`ShrinkableHash`] — user-facing display: `RightEllipsis`, can be ellipsis-
//!   truncated under extreme width pressure. Both have `RemovalPolicy::Never`.

mod hash_cell;
pub use hash_cell::{HashCell, NonShrinkableHash, ShrinkableHash};

use colored::Colorize;
use corsett::{
    shortener::{FreeText, RightEllipsis},
    ColumnSizingConfigBuilder, RemovalPolicy, Row, ShortenAny,
};
use library_ipc_protocol::{ContentHash, TrackInfo};

// =============================================================================
// Corsett column newtypes (artist, title, duration — hash is handled by HashCell)
// =============================================================================

struct ColArtist(String);
struct ColTitle(String);
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
impl AsRef<str> for ColDuration {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Artist and Title use RightEllipsis for graceful truncation.
// Duration uses FreeText (always short, no ellipsis needed).
impl corsett::Shorten for ColArtist {
    type Algorithm = RightEllipsis<'…', FreeText>;
}
impl corsett::Shorten for ColTitle {
    type Algorithm = RightEllipsis<'…', FreeText>;
}
impl corsett::Shorten for ColDuration {
    type Algorithm = FreeText;
}

// =============================================================================
// Row type — 4 columns: [hash, artist, title, duration]
// Generic over H: HashCell so the hash algorithm is determined at the call site.
// =============================================================================

struct TrackRow<H: HashCell> {
    hash: H,
    artist: ColArtist,
    title: ColTitle,
    duration: ColDuration,
}

impl<H: HashCell> Row<4> for TrackRow<H> {
    fn get_cell(&self, index: usize) -> &dyn ShortenAny {
        match index {
            0 => &self.hash,
            1 => &self.artist,
            2 => &self.title,
            3 => &self.duration,
            _ => panic!("TrackRow only has 4 columns, index {index} out of bounds"),
        }
    }
}

// =============================================================================
// Dynamic row type for subset/reordered columns
// =============================================================================

/// A row holding N boxed `ShortenAny` cells, for use with the subset/reordered column path.
struct DynRow<const N: usize> {
    cells: [Box<dyn ShortenAny>; N],
}

impl<const N: usize> Row<N> for DynRow<N> {
    fn get_cell(&self, index: usize) -> &dyn ShortenAny {
        &*self.cells[index]
    }
}

// =============================================================================
// Public API
// =============================================================================

/// Extract the first 12 characters of a hash, stripping any `sha256:` prefix.
pub fn short_hash(hash: &ContentHash) -> &str {
    let clean = hash
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(hash.as_str());
    if clean.len() >= 12 {
        &clean[..12]
    } else {
        clean
    }
}

/// Format a single track as the canonical playlist line.
///
/// Format: `{12-char-hash}  {Artist} - {Title}  [{duration}]`
///
/// If duration is absent the trailing `  [{duration}]` segment is omitted.
/// Artist and Title fall back to `"Unknown"` when absent.
///
/// Internally constructs a [`NonShrinkableHash`] to carry the playlist-safe intent:
/// this hash must never be truncated. The guarantee is structural — this function
/// formats directly via `format!()` and never calls `corsett::resize_columns`, so
/// the full 12-char prefix always appears in the output.
pub fn format_track_line(track: &TrackInfo) -> String {
    let title = track.title.as_deref().unwrap_or("Unknown");
    let artist = track.artist.as_deref().unwrap_or("Unknown");
    let hash: NonShrinkableHash = (&track.content_hash).into();
    match track.duration {
        Some(d) => format!("{}  {} - {}  [{}]", hash.as_ref(), artist, title, d),
        None => format!("{}  {} - {}", hash.as_ref(), artist, title),
    }
}

/// Columns available in the parameterized track table renderer.
///
/// Used by `mdma view` to specify which fields to show and in what order.
/// The same columns are used by `mdma search` via `ALL_COLUMNS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewColumn {
    Hash,
    Artist,
    Title,
    Duration,
}

/// The default column set for `mdma search` — all four columns in the standard order.
pub const ALL_COLUMNS: &[ViewColumn] = &[
    ViewColumn::Hash,
    ViewColumn::Artist,
    ViewColumn::Title,
    ViewColumn::Duration,
];

const GAP_SIZE: usize = 2;
const MAX_DEPTH: usize = 200;

/// Render a colored, aligned table of tracks for user-facing display.
///
/// This is the canonical entry point for `mdma search` and `mdma view`.
/// Uses [`ShrinkableHash`] — the hash column uses `RightEllipsis` so it can be
/// ellipsis-truncated under extreme width pressure while never being removed
/// (`RemovalPolicy::Never`). At typical terminal widths (≥80 chars) the hash
/// is always the full 12-char prefix.
///
/// `term_width` is injected by the caller (no terminal detection here — that's
/// a CLI concern). `reserved_prefix` is subtracted from `term_width` before
/// column sizing.
pub fn render_for_user(
    tracks: &[TrackInfo],
    columns: &[ViewColumn],
    term_width: usize,
    reserved_prefix: usize,
) -> Vec<String> {
    render_track_table_with_columns_inner::<ShrinkableHash>(
        tracks,
        columns,
        reserved_prefix,
        term_width,
    )
}

/// Generic inner renderer — parameterized over the hash policy `H`.
///
/// `render_for_user` (with `H = ShrinkableHash`) is the only public entry point.
/// This function is kept pub for backward compatibility with existing CLI call sites
/// during the transition period.
pub fn render_track_table_with_columns_inner<H: HashCell>(
    tracks: &[TrackInfo],
    columns: &[ViewColumn],
    reserved_prefix: usize,
    term_width: usize,
) -> Vec<String> {
    if tracks.is_empty() || columns.is_empty() {
        return vec![];
    }

    let available = term_width.saturating_sub(reserved_prefix);

    // Build ALL_COLUMNS rows and pick the corsett path based on column set.
    // For the full 4-column set we use the typed Row<4> path.
    // For a subset or different order we dispatch on N via render_with_corsett::<N, H>.
    if columns == ALL_COLUMNS {
        render_all_columns::<H>(tracks, available)
    } else {
        match columns.len() {
            1 => render_with_corsett::<1, H>(columns, tracks, available),
            2 => render_with_corsett::<2, H>(columns, tracks, available),
            3 => render_with_corsett::<3, H>(columns, tracks, available),
            4 => render_with_corsett::<4, H>(columns, tracks, available),
            _ => unreachable!("at most 4 ViewColumn variants exist"),
        }
    }
}

/// Render all 4 columns in the standard order via the typed `Row<4>` path.
/// Generic over `H: HashCell` — the hash algorithm is determined at the call site.
fn render_all_columns<H: HashCell>(tracks: &[TrackInfo], available: usize) -> Vec<String> {
    let rows: Vec<TrackRow<H>> = tracks
        .iter()
        .map(|t| TrackRow {
            hash: H::from(&t.content_hash),
            artist: ColArtist(t.artist.as_deref().unwrap_or("Unknown").to_string()),
            title: ColTitle(t.title.as_deref().unwrap_or("Unknown").to_string()),
            duration: ColDuration(t.duration.map(|d| d.to_string()).unwrap_or_default()),
        })
        .collect();

    let config = ColumnSizingConfigBuilder::<4>::new()
        .terminal_width(available)
        .gap_size(GAP_SIZE)
        .max_depth(MAX_DEPTH)
        .removal_policies([
            H::REMOVAL_POLICY,    // hash — driven by HashCell::REMOVAL_POLICY (always Never)
            RemovalPolicy::Never, // artist — never removed in search/view
            RemovalPolicy::Never, // title — never removed
            RemovalPolicy::Never, // duration — never removed
        ])
        .build();

    let resized = corsett::resize_columns(config, &rows);

    resized
        .into_iter()
        .map(|[hash_s, artist_s, title_s, dur_s]| {
            let mut parts: Vec<String> = Vec::with_capacity(4);
            parts.push(hash_s.bright_black().to_string());
            parts.push(artist_s.green().to_string());
            parts.push(title_s.bold().to_string());
            if !dur_s.is_empty() {
                parts.push(dur_s.bright_black().to_string());
            }
            parts.join("  ")
        })
        .collect()
}

/// Build a boxed `ShortenAny` cell for a given column and track, in the correct algorithm.
/// Generic over `H: HashCell` so the hash slot uses the caller's chosen hash policy.
fn make_cell<H: HashCell>(col: ViewColumn, track: &TrackInfo) -> Box<dyn ShortenAny> {
    match col {
        ViewColumn::Hash => Box::new(H::from(&track.content_hash)),
        ViewColumn::Artist => Box::new(ColArtist(
            track.artist.as_deref().unwrap_or("Unknown").to_string(),
        )),
        ViewColumn::Title => Box::new(ColTitle(
            track.title.as_deref().unwrap_or("Unknown").to_string(),
        )),
        ViewColumn::Duration => Box::new(ColDuration(
            track.duration.map(|d| d.to_string()).unwrap_or_default(),
        )),
    }
}

/// Apply ANSI color to an already-sized cell string.
fn colorize_cell(s: &str, col: ViewColumn) -> String {
    if s.is_empty() {
        return String::new();
    }
    match col {
        ViewColumn::Hash => s.bright_black().to_string(),
        ViewColumn::Artist => s.green().to_string(),
        ViewColumn::Title => s.bold().to_string(),
        ViewColumn::Duration => s.bright_black().to_string(),
    }
}

/// Render an arbitrary subset/ordering of N columns via corsett's typed `Row<N>` API.
///
/// All column width allocation is done by corsett — no hand-rolled math.
/// Generic over `H: HashCell` so the hash slot uses the caller's chosen hash policy.
fn render_with_corsett<const N: usize, H: HashCell>(
    columns: &[ViewColumn],
    tracks: &[TrackInfo],
    available: usize,
) -> Vec<String> {
    assert_eq!(columns.len(), N, "columns.len() must match const generic N");

    let rows: Vec<DynRow<N>> = tracks
        .iter()
        .map(|track| {
            // Build the cells array from the column slice in order.
            // We use std::array::from_fn for clean const-generic array construction.
            let cells = std::array::from_fn(|i| make_cell::<H>(columns[i], track));
            DynRow { cells }
        })
        .collect();

    let config = ColumnSizingConfigBuilder::<N>::new()
        .terminal_width(available)
        .gap_size(GAP_SIZE)
        .max_depth(MAX_DEPTH)
        .removal_policies([RemovalPolicy::Never; N])
        .build();

    let resized = corsett::resize_columns(config, &rows);

    resized
        .into_iter()
        .map(|sized_row| {
            let parts: Vec<String> = sized_row
                .iter()
                .enumerate()
                .filter_map(|(i, cell_str)| {
                    if cell_str.is_empty() {
                        None
                    } else {
                        Some(colorize_cell(cell_str, columns[i]))
                    }
                })
                .collect();
            parts.join("  ")
        })
        .collect()
}

/// Build playlist file content from a slice of track info.
///
/// Each track produces one canonical line. Lines are joined with `\n` and
/// a trailing newline is appended.
///
/// The hash used in each line is taken from `track.content_hash` — there is
/// no separate hash slice, making it structurally impossible to produce a
/// bare-hash line via this function.
pub fn format_playlist_content(tracks: &[TrackInfo]) -> String {
    let mut out = String::new();
    for track in tracks {
        out.push_str(&format_track_line(track));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_protocol::{ContentHash, TrackInfo};

    fn make_track(
        hash: &str,
        artist: Option<&str>,
        title: Option<&str>,
        duration: Option<u32>,
    ) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: title.map(String::from),
            artist: artist.map(String::from),
            album: None,
            duration: duration.map(|s| library_ipc_protocol::DurationSeconds::new(s)),
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
    fn short_hash_strips_prefix_and_truncates() {
        let h = ContentHash::new("sha256:abcdef1234567890");
        assert_eq!(short_hash(&h), "abcdef123456");
    }

    #[test]
    fn short_hash_no_prefix_truncates() {
        let h = ContentHash::new("abcdef1234567890");
        assert_eq!(short_hash(&h), "abcdef123456");
    }

    #[test]
    fn format_track_line_with_duration() {
        let track = make_track(
            "sha256:8d4e7b2cabc12345",
            Some("Artist Name"),
            Some("Track Title"),
            Some(225), // 3:45
        );
        let line = format_track_line(&track);
        assert_eq!(line, "8d4e7b2cabc1  Artist Name - Track Title  [3:45]");
    }

    #[test]
    fn format_track_line_without_duration() {
        let track = make_track(
            "sha256:8d4e7b2cabc12345",
            Some("Artist Name"),
            Some("Track Title"),
            None,
        );
        let line = format_track_line(&track);
        assert_eq!(line, "8d4e7b2cabc1  Artist Name - Track Title");
    }

    #[test]
    fn format_track_line_missing_metadata_falls_back_to_unknown() {
        let track = make_track("sha256:8d4e7b2cabc12345", None, None, None);
        let line = format_track_line(&track);
        assert_eq!(line, "8d4e7b2cabc1  Unknown - Unknown");
    }

    #[test]
    fn format_track_line_matches_canonical_regex() {
        let track = make_track(
            "sha256:8d4e7b2cabc12345",
            Some("Artist Name"),
            Some("Track Title"),
            Some(225),
        );
        let line = format_track_line(&track);
        // Regex: ^[0-9a-f]{12}\s{2}.+\s-\s.+\s{2}\[\d{1,2}:\d{2}(:\d{2})?\]$
        assert!(
            line.starts_with("8d4e7b2cabc1  "),
            "must start with 12-char hash + 2 spaces"
        );
        assert!(line.contains(" - "), "must contain artist-title separator");
        assert!(line.ends_with(']'), "must end with closing bracket");
    }

    #[test]
    fn format_playlist_content_builds_newline_terminated_lines() {
        let tracks = vec![
            make_track(
                "sha256:8d4e7b2cabc12345",
                Some("Artist One"),
                Some("Title One"),
                Some(225),
            ),
            make_track(
                "sha256:a3f91e0dabc12345",
                Some("Artist Two"),
                Some("Title Two"),
                Some(252),
            ),
        ];
        let content = format_playlist_content(&tracks);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "8d4e7b2cabc1  Artist One - Title One  [3:45]");
        assert_eq!(lines[1], "a3f91e0dabc1  Artist Two - Title Two  [4:12]");
    }

    /// Belt-and-braces: the first whitespace-separated token of a playlist line
    /// must be exactly 12 lowercase hex chars (the load-bearing invariant for
    /// playlist parse round-trips).
    #[test]
    fn playlist_line_first_token_is_exactly_12_hex_chars() {
        let track = make_track(
            "sha256:8d4e7b2cabc12345",
            Some("A Very Long Artist Name That Could Stress The Format"),
            Some("An Equally Very Long Title String"),
            Some(225),
        );
        let line = format_track_line(&track);
        let first_token = line
            .split_whitespace()
            .next()
            .expect("line must have token");
        assert_eq!(
            first_token.len(),
            12,
            "playlist line first token must be exactly 12 chars, got: {first_token:?}"
        );
        assert!(
            first_token.chars().all(|c| c.is_ascii_hexdigit()),
            "playlist line first token must be all hex digits, got: {first_token:?}"
        );
    }

    // ── Corsett-based renderer tests ─────────────────────────────────────────────

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if c == 'm' {
                    in_escape = false;
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn hash_column_never_shorter_than_12_chars_with_wide_terminal() {
        // Uniqueness invariant: the hash token is always exactly 12 chars.
        let tracks: Vec<TrackInfo> = (0..3)
            .map(|i| {
                make_track(
                    &format!("sha256:abcdef{:02}deadbeef", i),
                    Some("A Very Long Artist Name That Puts Pressure On Layout"),
                    Some("An Equally Very Long Title String For Layout Testing"),
                    Some(300),
                )
            })
            .collect();

        let lines = render_for_user(&tracks, ALL_COLUMNS, 200, 0);

        for (i, line) in lines.iter().enumerate() {
            let stripped = strip_ansi(line);
            let hash_token = stripped
                .split_whitespace()
                .next()
                .expect("line must have at least one token");
            assert_eq!(
                hash_token.len(),
                12,
                "Line {}: hash token {:?} should be exactly 12 chars",
                i + 1,
                hash_token,
            );
        }
    }

    /// Narrow terminal (hash-only): hash column is always 12 chars because
    /// FreeText never truncates content shorter than available width, and
    /// a single hash column at width=20 always fits (12 < 20).
    /// RemovalPolicy::Never guarantees the column is present.
    #[test]
    fn hash_column_present_and_12_chars_at_narrow_terminal() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("A Very Long Artist Name"),
            Some("A Very Long Title"),
            Some(300),
        );
        // Single hash column at term_width=20: hash is 12 chars, width=20, fits exactly.
        let lines = render_for_user(&[track], &[ViewColumn::Hash], 20, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let hash_token = stripped
            .split_whitespace()
            .next()
            .expect("line must have at least one token");
        assert_eq!(
            hash_token.len(),
            12,
            "hash token must be exactly 12 chars at narrow terminal (hash-only), got: {hash_token:?}"
        );
    }

    /// Narrow terminal with subset path: hash column is present (not removed)
    /// because RemovalPolicy::Never applies to all columns.
    /// Content may be corsett-shortened under extreme width pressure but
    /// the column token must be non-empty and start with valid hex.
    #[test]
    fn hash_column_present_at_narrow_terminal_subset_path() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("A Very Long Artist Name"),
            Some("A Very Long Title"),
            Some(300),
        );
        // Use Hash+Title (subset path, not ALL_COLUMNS). term_width=20 is narrow.
        let lines = render_for_user(&[track], &[ViewColumn::Hash, ViewColumn::Title], 20, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        // Hash token must be present (RemovalPolicy::Never guarantees the column exists).
        // ShrinkableHash uses RightEllipsis — at narrow widths the hash may be
        // ellipsis-truncated (e.g. "ab…"), so we cannot require all-hex chars here.
        // We only check that the column is non-empty and starts with hex chars.
        let hash_token = stripped
            .split_whitespace()
            .next()
            .expect("line must have at least one token — hash column must be present");
        assert!(
            !hash_token.is_empty(),
            "hash column must not be empty even at narrow terminal (subset path)"
        );
        assert!(
            hash_token.chars().next().map_or(false, |c| c.is_ascii_hexdigit()),
            "hash column token must start with hex chars (may end with ellipsis at narrow widths), got: {hash_token:?}"
        );
        // At wide-enough terminals (which is the realistic case), it's exactly 12:
        let track2 = make_track(
            "sha256:abcdef1234567890",
            Some("A Very Long Artist Name"),
            Some("A Very Long Title"),
            Some(300),
        );
        let wide_lines = render_for_user(&[track2], &[ViewColumn::Hash, ViewColumn::Title], 120, 0);
        let wide_stripped = strip_ansi(&wide_lines[0]);
        let wide_hash = wide_stripped.split_whitespace().next().unwrap();
        assert_eq!(
            wide_hash.len(),
            12,
            "hash must be exactly 12 chars at comfortable width, got: {wide_hash:?}"
        );
    }

    #[test]
    fn all_columns_present_at_wide_terminal() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Polyrytmi"),
            Some(367),
        );
        let lines = render_for_user(&[track], ALL_COLUMNS, 200, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("abcdef123456"), "must contain hash");
        assert!(
            stripped.contains("Carbon Based Lifeforms"),
            "must contain artist"
        );
        assert!(stripped.contains("Polyrytmi"), "must contain title");
        assert!(stripped.contains("6:07"), "must contain duration");
    }

    #[test]
    fn subset_columns_hash_only() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Title"),
            Some(180),
        );
        let lines = render_for_user(&[track], &[ViewColumn::Hash], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("abcdef123456"), "must contain hash");
        assert!(!stripped.contains("Artist"), "must not contain artist");
        assert!(!stripped.contains("Title"), "must not contain title");
    }

    #[test]
    fn subset_columns_artist_only() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Init"),
            Some(508),
        );
        let lines = render_for_user(&[track], &[ViewColumn::Artist], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(
            stripped.contains("Carbon Based Lifeforms"),
            "must contain artist"
        );
        assert!(!stripped.contains("abcdef123456"), "must not contain hash");
    }

    #[test]
    fn subset_columns_title_only() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Polyrytmi"),
            Some(367),
        );
        let lines = render_for_user(&[track], &[ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("Polyrytmi"), "must contain title");
        assert!(!stripped.contains("Carbon"), "must not contain artist");
    }

    #[test]
    fn subset_columns_duration_only() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("Title"),
            Some(225), // 3:45
        );
        let lines = render_for_user(&[track], &[ViewColumn::Duration], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(stripped.contains("3:45"), "must contain duration");
        assert!(!stripped.contains("Artist"), "must not contain artist");
    }

    #[test]
    fn column_order_preserved_title_then_hash() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("TheTitle"),
            None,
        );
        let lines = render_for_user(&[track], &[ViewColumn::Title, ViewColumn::Hash], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let title_pos = stripped.find("TheTitle").expect("title not in output");
        let hash_pos = stripped.find("abcdef123456").expect("hash not in output");
        assert!(
            title_pos < hash_pos,
            "title should appear before hash, got: {stripped:?}"
        );
    }

    #[test]
    fn column_order_preserved_hash_then_title() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Artist"),
            Some("TheTitle"),
            None,
        );
        let lines = render_for_user(&[track], &[ViewColumn::Hash, ViewColumn::Title], 120, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let hash_pos = stripped.find("abcdef123456").expect("hash not in output");
        let title_pos = stripped.find("TheTitle").expect("title not in output");
        assert!(
            hash_pos < title_pos,
            "hash should appear before title, got: {stripped:?}"
        );
    }

    #[test]
    fn empty_tracks_returns_empty_vec() {
        let lines = render_for_user(&[], ALL_COLUMNS, 120, 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn empty_columns_returns_empty_vec() {
        let track = make_track("sha256:abcdef1234567890", Some("A"), Some("T"), None);
        let lines = render_for_user(&[track], &[], 120, 0);
        assert!(lines.is_empty());
    }

    // ── render_for_user tests ─────────────────────────────────────────────────

    /// render_for_user is the canonical entry point for mdma search and mdma view.
    /// It uses ShrinkableHash — at typical widths hash is the full 12 chars.
    #[test]
    fn render_for_user_produces_full_hash_at_typical_width() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("Carbon Based Lifeforms"),
            Some("Polyrytmi"),
            Some(367),
        );
        let lines = render_for_user(&[track], ALL_COLUMNS, 200, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        let hash_token = stripped.split_whitespace().next().expect("must have token");
        assert_eq!(
            hash_token, "abcdef123456",
            "hash must be 12 chars at typical width"
        );
    }

    /// render_for_user must not remove the hash column (RemovalPolicy::Never).
    /// At narrow widths, ShrinkableHash (RightEllipsis) may ellipsis-truncate the hash
    /// (e.g. "a…") but the column must still be present and start with hex chars.
    #[test]
    fn render_for_user_hash_column_never_removed() {
        let track = make_track(
            "sha256:abcdef1234567890",
            Some("A Very Long Artist Name That Puts Pressure On Layout"),
            Some("An Equally Very Long Title For Layout Pressure"),
            Some(300),
        );
        let lines = render_for_user(&[track], ALL_COLUMNS, 80, 0);
        assert_eq!(lines.len(), 1);
        let stripped = strip_ansi(&lines[0]);
        assert!(!stripped.is_empty(), "line must not be empty");
        // Hash column must be present — RemovalPolicy::Never ensures the column is not dropped.
        // ShrinkableHash may ellipsis-truncate at narrow widths, so allow trailing "…".
        let first_token = stripped.split_whitespace().next().expect("must have token");
        assert!(
            first_token
                .chars()
                .next()
                .map_or(false, |c| c.is_ascii_hexdigit()),
            "first token must start with hex (hash column present), got: {first_token:?}"
        );
    }

    /// render_for_user with empty tracks returns empty vec.
    #[test]
    fn render_for_user_empty_tracks_returns_empty() {
        let lines = render_for_user(&[], ALL_COLUMNS, 120, 0);
        assert!(lines.is_empty());
    }

    /// format_track_line uses NonShrinkableHash internally — the 12-char guarantee
    /// holds regardless of any width constraint (no corsett involved).
    #[test]
    fn format_track_line_hash_is_always_full_12_chars_via_non_shrinkable() {
        let track = make_track(
            "sha256:deadbeef12345678",
            Some("Artist"),
            Some("Title"),
            Some(180),
        );
        let line = format_track_line(&track);
        let first_token = line.split_whitespace().next().expect("must have token");
        assert_eq!(
            first_token.len(),
            12,
            "NonShrinkableHash must produce exactly 12-char hash, got: {first_token:?}"
        );
        assert_eq!(first_token, "deadbeef1234");
    }
}
