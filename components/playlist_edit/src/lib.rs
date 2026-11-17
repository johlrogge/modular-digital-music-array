//! Pure playlist editing (ADR-027): parse a playlist file into lines and apply
//! edit ops to it.
//!
//! No IO. The caller passes in the library's tracks, and hashes are resolved
//! with `library_projection`'s one matcher, so the library service and its stub
//! apply the same rules. Undo is not computed here: the caller keeps the bytes
//! the edit started from and reverts to them (ADR-027 §5).
//!
//! Only the lines an op names change; every other line, including comments,
//! short IDs and `# reason` text, stays byte for byte.

use library_ipc_protocol::{
    entry_token, is_entry_line, CanonicalHash, ContentHash, Duplicates, EditOp, EntryError,
    PlaylistEntry, ProtocolError, Skipped, TrackInfo,
};
use library_projection::{resolve_entry, resolve_entry_track, HashedTrack};
use thiserror::Error;
use track_formatter::format_track_line;

/// Why a string is not a [`StoredLine`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoredLineError {
    #[error("a stored line is exactly one line")]
    NotOneLine,
    #[error("a stored line must be an entry line, not blank or a comment")]
    NotAnEntry,
}

/// One playlist entry line, exactly as stored (trailing text included): a
/// single line that is an entry by the `is_entry_line` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLine(String);

impl StoredLine {
    pub fn new(raw: impl Into<String>) -> Result<Self, StoredLineError> {
        let raw = raw.into();
        if raw.contains('\n') {
            return Err(StoredLineError::NotOneLine);
        }
        if !is_entry_line(&raw) {
            return Err(StoredLineError::NotAnEntry);
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The line's hash token (its first whitespace token).
    pub fn token(&self) -> &str {
        // An entry line always has a token; validated on construction.
        entry_token(&self.0).unwrap_or_default()
    }
}

/// One line of a playlist file, without its line break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// An entry line (see `library_ipc_protocol::is_entry_line`).
    Entry(StoredLine),
    /// A comment or blank line.
    Other { raw: String },
}

impl Line {
    fn parse(raw: &str) -> Self {
        // The one classification rule lives in StoredLine's validation.
        match StoredLine::new(raw) {
            Ok(line) => Line::Entry(line),
            Err(_) => Line::Other {
                raw: raw.to_string(),
            },
        }
    }

    fn raw(&self) -> &str {
        match self {
            Line::Entry(line) => line.as_str(),
            Line::Other { raw } => raw,
        }
    }
}

/// A playlist file as lines, so that rendering gives back the same bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistDoc {
    lines: Vec<Line>,
    /// Whether the last line ends in a newline.
    trailing_newline: bool,
}

impl PlaylistDoc {
    /// Split `text` at `\n`. Any `\r` stays in its line, so CRLF files render
    /// unchanged. `render(parse(text)) == text` for every `text`.
    pub fn parse(text: &str) -> Self {
        let mut pieces: Vec<&str> = text.split('\n').collect();
        // "a\nb\n" splits to ["a", "b", ""]: the empty piece is no line.
        // An empty file has no lines, and gets a newline once it has one.
        let trailing_newline = pieces.last() == Some(&"");
        if trailing_newline {
            pieces.pop();
        }
        Self {
            lines: pieces.into_iter().map(Line::parse).collect(),
            trailing_newline,
        }
    }

    pub fn render(&self) -> String {
        let mut text = self
            .lines
            .iter()
            .map(Line::raw)
            .collect::<Vec<_>>()
            .join("\n");
        if self.trailing_newline && !self.lines.is_empty() {
            text.push('\n');
        }
        text
    }

    /// The entry lines, in order: what edit positions index.
    pub fn entries(&self) -> impl Iterator<Item = &StoredLine> {
        self.lines.iter().filter_map(|line| match line {
            Line::Entry(entry) => Some(entry),
            Line::Other { .. } => None,
        })
    }

    pub fn entry_count(&self) -> usize {
        self.entries().count()
    }

    /// For each entry, its index among all lines.
    fn entry_slots(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| matches!(line, Line::Entry(_)))
            .map(|(index, _)| index)
            .collect()
    }

    /// The line index that a new line at entry position `at` goes to: before
    /// the entry now at `at`; after the last entry when `at` is the count (so
    /// a trailing comment stays last); the end of the file when there are no
    /// entries.
    fn line_index_for(&self, at: usize) -> usize {
        let slots = self.entry_slots();
        match slots.get(at) {
            Some(&index) => index,
            None => slots.last().map_or(self.lines.len(), |last| last + 1),
        }
    }

    fn insert_entry(&mut self, at: usize, entry: StoredLine) {
        let index = self.line_index_for(at);
        self.lines.insert(index, Line::Entry(entry));
    }

    fn remove_entry(&mut self, at: usize) {
        let index = self.entry_slots()[at];
        self.lines.remove(index);
        // A file with no lines is what `parse("")` gives; keep the flag the
        // same, so editing in one step or in two ends in the same bytes.
        if self.lines.is_empty() {
            self.trailing_newline = true;
        }
    }

    /// Move entry `from` to position `to`. The entries trade places among the
    /// entry lines' slots, so no comment or blank line moves.
    fn move_entry(&mut self, from: usize, to: usize) {
        let slots = self.entry_slots();
        let mut entries: Vec<StoredLine> = self.entries().cloned().collect();
        let moved = entries.remove(from);
        entries.insert(to, moved);
        for (slot, entry) in slots.into_iter().zip(entries) {
            self.lines[slot] = Line::Entry(entry);
        }
    }
}

/// Why an edit was refused. Nothing is changed.
#[derive(Debug, Clone, Error)]
pub enum EditError {
    /// The op at index `op` cannot be applied.
    #[error("invalid edit (op {op}): {reason}")]
    Invalid { op: usize, reason: String },
    /// An inserted hash does not resolve to one track.
    #[error("{0}")]
    Unresolved(EntryError),
}

impl From<EditError> for ProtocolError {
    fn from(error: EditError) -> Self {
        match error {
            EditError::Invalid { op, reason } => ProtocolError::InvalidEdit { op, reason },
            EditError::Unresolved(EntryError::Unknown { hash }) => ProtocolError::TrackNotFound {
                hash: hash.to_string(),
            },
            EditError::Unresolved(EntryError::Ambiguous { hash, candidates }) => {
                ProtocolError::ambiguous_hash(
                    hash.as_str(),
                    candidates.iter().map(ContentHash::as_str),
                )
            }
            EditError::Unresolved(EntryError::NotCanonical { hash, found }) => {
                ProtocolError::HashNotCanonical {
                    hash: hash.to_string(),
                    found: found.to_string(),
                }
            }
        }
    }
}

/// The result of a successful `apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    pub doc: PlaylistDoc,
    /// Hashes `Duplicates::Skip` left out, with where they already are.
    pub skipped: Vec<Skipped>,
}

/// Apply `ops` to `doc`, in order; `doc` itself is not changed. Each op's
/// positions refer to the state the ops before it left. Any refused op
/// refuses the whole edit. No ops is the identity edit.
///
/// `tracks` are the library's tracks (hashes resolve with
/// `library_projection::resolve_visible`) and `to_info` gives the track a new
/// line is written from.
pub fn apply<T: HashedTrack>(
    doc: &PlaylistDoc,
    ops: &[EditOp],
    tracks: &[T],
    to_info: impl Fn(&T) -> TrackInfo,
) -> Result<Edited, EditError> {
    let mut edited = Edited {
        doc: doc.clone(),
        skipped: Vec::new(),
    };
    for (index, op) in ops.iter().enumerate() {
        apply_op(index, op, &mut edited, tracks, &to_info)?;
    }
    Ok(edited)
}

fn invalid(op: usize, reason: String) -> EditError {
    EditError::Invalid { op, reason }
}

fn check_position(op: usize, name: &str, position: usize, limit: usize) -> Result<(), EditError> {
    if position >= limit {
        return Err(invalid(
            op,
            format!("{name} {position} is out of range ({limit} positions)"),
        ));
    }
    Ok(())
}

fn apply_op<T: HashedTrack>(
    index: usize,
    op: &EditOp,
    edited: &mut Edited,
    tracks: &[T],
    to_info: &impl Fn(&T) -> TrackInfo,
) -> Result<(), EditError> {
    let count = edited.doc.entry_count();
    match op {
        EditOp::Insert {
            at,
            hashes,
            duplicates,
        } => {
            // An insert may go after the last entry, so `count + 1` positions.
            check_position(index, "position", *at, count + 1)?;
            insert(edited, *at, hashes, *duplicates, tracks, to_info)
        }
        EditOp::Remove { at } => {
            check_position(index, "position", *at, count)?;
            edited.doc.remove_entry(*at);
            Ok(())
        }
        EditOp::Move { from, to } => {
            check_position(index, "from", *from, count)?;
            check_position(index, "to", *to, count)?;
            edited.doc.move_entry(*from, *to);
            Ok(())
        }
    }
}

/// Where a hash that `Skip` left out is held already.
enum Holder {
    /// The entry at this position before the insert.
    Existing(usize),
    /// The nth line this insert adds.
    Added(usize),
}

fn insert<T: HashedTrack>(
    edited: &mut Edited,
    at: usize,
    hashes: &[ContentHash],
    duplicates: Duplicates,
    tracks: &[T],
    to_info: &impl Fn(&T) -> TrackInfo,
) -> Result<(), EditError> {
    // Resolve everything first: one refused hash refuses the whole edit.
    let resolved = hashes
        .iter()
        .map(|hash| resolve_entry_track(tracks, hash.as_str()).map_err(EditError::Unresolved))
        .collect::<Result<Vec<_>, _>>()?;

    // The canonical hash of each existing entry; an unresolved one is None,
    // and never counts as a duplicate.
    let existing: Vec<Option<CanonicalHash>> = match duplicates {
        Duplicates::Skip => edited
            .doc
            .entries()
            .map(|entry| match resolve_entry(tracks, entry.token()) {
                PlaylistEntry::Track { hash } => Some(hash),
                PlaylistEntry::Unresolved { .. } => None,
            })
            .collect(),
        Duplicates::Keep => Vec::new(),
    };

    let mut added: Vec<(CanonicalHash, StoredLine)> = Vec::new();
    let mut left_out: Vec<(CanonicalHash, Holder)> = Vec::new();
    for (hash, track) in resolved {
        let holder = match duplicates {
            Duplicates::Keep => None,
            Duplicates::Skip => existing
                .iter()
                .position(|held| held.as_ref() == Some(&hash))
                .map(Holder::Existing)
                .or_else(|| {
                    added
                        .iter()
                        .position(|(added_hash, _)| added_hash == &hash)
                        .map(Holder::Added)
                }),
        };
        match holder {
            Some(holder) => left_out.push((hash, holder)),
            None => {
                let line = StoredLine::new(format_track_line(&to_info(track)))
                    .expect("format_track_line writes one entry line");
                added.push((hash, line));
            }
        }
    }

    // Positions are those of the state after this op.
    let added_count = added.len();
    edited
        .skipped
        .extend(left_out.into_iter().map(|(hash, holder)| Skipped {
            hash: hash.into(),
            existing_at: match holder {
                Holder::Existing(position) if position >= at => position + added_count,
                Holder::Existing(position) => position,
                Holder::Added(nth) => at + nth,
            },
        }));

    for (nth, (_, line)) in added.into_iter().enumerate() {
        edited.doc.insert_entry(at + nth, line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_protocol::{playlist_hashes, PlaylistVersion};
    use pretty_assertions::assert_eq;

    fn full(tag: &str) -> String {
        format!("sha256:{}", tag.repeat(64))
    }

    fn info(hash: String, title: &str) -> TrackInfo {
        TrackInfo {
            content_hash: ContentHash::new(hash),
            title: Some(title.to_string()),
            artist: Some("Artist".to_string()),
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

    struct TestTrack {
        info: TrackInfo,
        hidden: bool,
    }

    impl HashedTrack for TestTrack {
        fn content_hash(&self) -> &str {
            self.info.content_hash.as_str()
        }

        fn title(&self) -> Option<&str> {
            self.info.title.as_deref()
        }

        fn is_hidden(&self) -> bool {
            self.hidden
        }
    }

    fn test_track(hash: String, title: &str) -> TestTrack {
        TestTrack {
            info: info(hash, title),
            hidden: false,
        }
    }

    /// Tracks a, b and c (hashes of that letter repeated), a hidden e, a
    /// legacy short-hash track, and two that share the prefix `dd`.
    fn library() -> Vec<TestTrack> {
        let mut hidden = test_track(full("e"), "E");
        hidden.hidden = true;
        vec![
            test_track(full("a"), "A"),
            test_track(full("b"), "B"),
            test_track(full("c"), "C"),
            hidden,
            test_track("ff00ff00".to_string(), "Legacy"),
            test_track(format!("sha256:dd1{}", "0".repeat(61)), "D1"),
            test_track(format!("sha256:dd2{}", "0".repeat(61)), "D2"),
        ]
    }

    fn try_apply(text: &str, ops: &[EditOp]) -> Result<Edited, EditError> {
        apply(&PlaylistDoc::parse(text), ops, &library(), |t| {
            t.info.clone()
        })
    }

    fn edit(text: &str, ops: &[EditOp]) -> Edited {
        try_apply(text, ops).unwrap()
    }

    fn rendered(text: &str, ops: &[EditOp]) -> String {
        edit(text, ops).doc.render()
    }

    fn insert_op(at: usize, tags: &[&str], duplicates: Duplicates) -> EditOp {
        EditOp::Insert {
            at,
            hashes: tags.iter().map(|t| ContentHash::new(full(t))).collect(),
            duplicates,
        }
    }

    fn line_for(tag: &str, title: &str) -> String {
        format!("{}  Artist - {title}", &full(tag)[7..19])
    }

    /// Every text up to length 7 over an alphabet of line-format characters.
    fn all_short_texts() -> Vec<String> {
        let alphabet = ["a", "#", " ", "\n", "\r", "\t"];
        let mut texts = vec![String::new()];
        let mut frontier = texts.clone();
        for _ in 0..7 {
            frontier = frontier
                .iter()
                .flat_map(|text| alphabet.iter().map(move |c| format!("{text}{c}")))
                .collect();
            texts.extend(frontier.iter().cloned());
        }
        texts
    }

    // ----- parse and render -----

    #[test]
    fn render_of_parse_is_identity_for_every_short_text() {
        for text in all_short_texts() {
            assert_eq!(PlaylistDoc::parse(&text).render(), text, "{text:?}");
        }
    }

    #[test]
    fn render_of_parse_keeps_realistic_files() {
        for text in [
            "",
            "\n",
            "\n\n",
            "aaa",
            "aaa\n",
            "aaa  Artist - Title\r\nbbb  # unknown track\r\n",
            "# a note\n\naaa\n   \n# tail",
            "aaa\n\n\n",
        ] {
            assert_eq!(PlaylistDoc::parse(text).render(), text, "{text:?}");
        }
    }

    #[test]
    fn parse_classifies_lines_like_playlist_hashes_for_every_short_text() {
        for text in all_short_texts() {
            let doc = PlaylistDoc::parse(&text);
            let tokens: Vec<&str> = doc.entries().map(StoredLine::token).collect();
            let hashes: Vec<&str> = playlist_hashes(&text).collect();
            assert_eq!(tokens, hashes, "{text:?}");
        }
    }

    // ----- ops -----

    const FILE: &str = "# set\naaa  Artist - A\n\n# the second\nbbb  # unknown track\nccc\n# end\n";

    #[test]
    fn insert_goes_before_the_entry_at_the_position() {
        assert_eq!(
            rendered(FILE, &[insert_op(1, &["c"], Duplicates::Keep)]),
            format!(
                "# set\naaa  Artist - A\n\n# the second\n{}\nbbb  # unknown track\nccc\n# end\n",
                line_for("c", "C")
            )
        );
    }

    #[test]
    fn insert_at_the_count_appends_after_the_last_entry() {
        assert_eq!(
            rendered(FILE, &[insert_op(3, &["a"], Duplicates::Keep)]),
            format!(
                "# set\naaa  Artist - A\n\n# the second\nbbb  # unknown track\nccc\n{}\n# end\n",
                line_for("a", "A")
            )
        );
    }

    #[test]
    fn insert_into_an_empty_playlist_ends_the_file_in_a_newline() {
        assert_eq!(
            rendered("", &[insert_op(0, &["a"], Duplicates::Keep)]),
            format!("{}\n", line_for("a", "A"))
        );
    }

    #[test]
    fn insert_appends_after_an_unterminated_last_line() {
        assert_eq!(
            rendered("aaa", &[insert_op(1, &["b"], Duplicates::Keep)]),
            format!("aaa\n{}", line_for("b", "B"))
        );
    }

    #[test]
    fn insert_resolves_a_short_prefix_to_the_track() {
        let ops = [EditOp::Insert {
            at: 0,
            hashes: vec![ContentHash::new("bbbb")],
            duplicates: Duplicates::Keep,
        }];
        assert_eq!(rendered("", &ops), format!("{}\n", line_for("b", "B")));
    }

    #[test]
    fn insert_of_a_line_break_title_makes_one_line() {
        let mut tracks = library();
        tracks[0].info.title = Some("two\nlines\r".to_string());
        let ops = [insert_op(0, &["a"], Duplicates::Keep)];
        let edited = apply(&PlaylistDoc::parse(""), &ops, &tracks, |t| t.info.clone()).unwrap();
        assert_eq!(edited.doc.entry_count(), 1);
        assert_eq!(edited.doc.render().lines().count(), 1);
    }

    #[test]
    fn remove_takes_only_the_entry_line() {
        assert_eq!(
            rendered(FILE, &[EditOp::Remove { at: 1 }]),
            "# set\naaa  Artist - A\n\n# the second\nccc\n# end\n"
        );
    }

    #[test]
    fn move_leaves_comments_and_blank_lines_in_place() {
        assert_eq!(
            rendered(FILE, &[EditOp::Move { from: 0, to: 2 }]),
            "# set\nbbb  # unknown track\n\n# the second\nccc\naaa  Artist - A\n# end\n"
        );
        assert_eq!(
            rendered(FILE, &[EditOp::Move { from: 2, to: 0 }]),
            "# set\nccc\n\n# the second\naaa  Artist - A\nbbb  # unknown track\n# end\n"
        );
    }

    #[test]
    fn ops_apply_in_order_each_seeing_the_one_before() {
        let text = rendered(
            "aaa\nbbb\nccc\n",
            &[
                EditOp::Remove { at: 0 },
                EditOp::Move { from: 1, to: 0 },
                EditOp::Remove { at: 1 },
            ],
        );
        assert_eq!(text, "ccc\n");
    }

    #[test]
    fn untouched_lines_stay_byte_identical() {
        let text = "  AAA   spaced \r\n#c\r\n\r\n9fb4105e  # unknown track\r\nccc\r\n";
        assert_eq!(
            rendered(text, &[EditOp::Move { from: 0, to: 2 }]),
            "9fb4105e  # unknown track\r\n#c\r\n\r\nccc\r\n  AAA   spaced \r\n"
        );
        assert_eq!(
            rendered(text, &[EditOp::Remove { at: 1 }]),
            "  AAA   spaced \r\n#c\r\n\r\nccc\r\n"
        );
    }

    #[test]
    fn no_ops_is_the_identity_edit() {
        for text in all_short_texts().iter().step_by(97) {
            let edited = edit(text, &[]);
            assert_eq!(edited.doc.render(), *text);
            assert!(edited.skipped.is_empty());
        }
    }

    #[test]
    fn apply_leaves_the_given_doc_alone() {
        let doc = PlaylistDoc::parse(FILE);
        apply(&doc, &[EditOp::Remove { at: 0 }], &library(), |t| {
            t.info.clone()
        })
        .unwrap();
        assert_eq!(doc.render(), FILE);
    }

    #[test]
    fn emptying_a_file_then_inserting_matches_two_separate_edits() {
        for text in ["aaa", "aaa\n"] {
            let together = rendered(
                text,
                &[
                    EditOp::Remove { at: 0 },
                    insert_op(0, &["a"], Duplicates::Keep),
                ],
            );
            let emptied = rendered(text, &[EditOp::Remove { at: 0 }]);
            let apart = rendered(&emptied, &[insert_op(0, &["a"], Duplicates::Keep)]);
            assert_eq!(together, apart, "{text:?}");
            assert_eq!(together, format!("{}\n", line_for("a", "A")));
        }
    }

    #[test]
    fn an_edit_has_a_new_version_unless_it_changes_nothing() {
        let version = |text: &str| PlaylistVersion::of(text.as_bytes());
        let changed = rendered("aaa\n", &[EditOp::Remove { at: 0 }]);
        assert_ne!(version(&changed), version("aaa\n"));
        let same = rendered("aaa\n", &[insert_op(0, &["a"], Duplicates::Skip)]);
        assert_eq!(version(&same), version("aaa\n"));
    }

    // ----- duplicates -----

    #[test]
    fn skip_leaves_out_hashes_already_present_and_says_where() {
        let edited = edit("aaa\nbbb\n", &[insert_op(0, &["b", "c"], Duplicates::Skip)]);
        assert_eq!(
            edited.doc.render(),
            format!("{}\naaa\nbbb\n", line_for("c", "C"))
        );
        // bbb moved from 1 to 2 by the inserted line.
        assert_eq!(
            edited.skipped,
            vec![Skipped {
                hash: ContentHash::new(full("b")),
                existing_at: 2
            }]
        );
    }

    #[test]
    fn skip_also_drops_repeats_within_one_insert() {
        let edited = edit("", &[insert_op(0, &["a", "b", "a"], Duplicates::Skip)]);
        assert_eq!(edited.doc.entry_count(), 2);
        assert_eq!(
            edited.skipped,
            vec![Skipped {
                hash: ContentHash::new(full("a")),
                existing_at: 0
            }]
        );
    }

    #[test]
    fn keep_inserts_every_hash_even_duplicates() {
        let edited = edit("aaa\n", &[insert_op(1, &["a", "a"], Duplicates::Keep)]);
        assert_eq!(edited.doc.entry_count(), 3);
        assert!(edited.skipped.is_empty());
    }

    #[test]
    fn a_short_existing_id_counts_as_a_duplicate_of_the_full_hash() {
        let edited = edit("aaaa\n", &[insert_op(1, &["a"], Duplicates::Skip)]);
        assert_eq!(edited.skipped.len(), 1);
        assert_eq!(edited.doc.render(), "aaaa\n");
    }

    #[test]
    fn an_unresolved_entry_never_counts_as_a_duplicate() {
        // "ee" names only a hidden track, so it holds nothing.
        let edited = edit(
            "ee  # unknown track\n",
            &[insert_op(1, &["a"], Duplicates::Skip)],
        );
        assert!(edited.skipped.is_empty());
        assert_eq!(edited.doc.entry_count(), 2);
    }

    #[test]
    fn an_edit_that_skips_everything_changes_nothing() {
        let edited = edit("aaa\n", &[insert_op(0, &["a"], Duplicates::Skip)]);
        assert_eq!(edited.doc.render(), "aaa\n");
    }

    // ----- refusals -----

    #[test]
    fn positions_out_of_range_refuse_the_whole_edit_naming_the_op() {
        for (bad, ops) in [
            (1, vec![EditOp::Remove { at: 0 }, EditOp::Remove { at: 1 }]),
            (0, vec![insert_op(3, &["a"], Duplicates::Keep)]),
            (0, vec![EditOp::Move { from: 2, to: 0 }]),
            (0, vec![EditOp::Move { from: 0, to: 2 }]),
        ] {
            match try_apply("aaa\nbbb\n", &ops) {
                Err(EditError::Invalid { op, .. }) => assert_eq!(op, bad, "{ops:?}"),
                other => panic!("expected a refusal for {ops:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_unknown_hash_refuses_the_whole_edit_with_the_typed_error() {
        let ops = [
            EditOp::Remove { at: 0 },
            insert_op(0, &["a", "9"], Duplicates::Keep),
        ];
        let err = try_apply("bbb\n", &ops).unwrap_err();
        assert!(
            matches!(err, EditError::Unresolved(EntryError::Unknown { .. })),
            "{err}"
        );
        assert!(matches!(
            ProtocolError::from(err),
            ProtocolError::TrackNotFound { .. }
        ));
    }

    #[test]
    fn a_hidden_track_is_as_unknown_as_a_missing_one() {
        let err = try_apply("", &[insert_op(0, &["e"], Duplicates::Keep)]).unwrap_err();
        assert!(matches!(
            err,
            EditError::Unresolved(EntryError::Unknown { .. })
        ));
    }

    #[test]
    fn an_ambiguous_hash_refuses_the_whole_edit_with_the_typed_error() {
        let ops = [EditOp::Insert {
            at: 0,
            hashes: vec![ContentHash::new("dd")],
            duplicates: Duplicates::Keep,
        }];
        let err = try_apply("", &ops).unwrap_err();
        assert!(matches!(
            err,
            EditError::Unresolved(EntryError::Ambiguous { .. })
        ));
        assert!(matches!(
            ProtocolError::from(err),
            ProtocolError::AmbiguousHash { .. }
        ));
    }

    #[test]
    fn a_track_known_only_by_a_short_hash_is_refused_as_not_canonical() {
        let ops = [EditOp::Insert {
            at: 0,
            hashes: vec![ContentHash::new("ff00")],
            duplicates: Duplicates::Keep,
        }];
        let err = try_apply("", &ops).unwrap_err();
        assert!(matches!(
            err,
            EditError::Unresolved(EntryError::NotCanonical { .. })
        ));
        match ProtocolError::from(err) {
            ProtocolError::HashNotCanonical { hash, found } => {
                assert_eq!(hash, "ff00");
                assert_eq!(found, "ff00ff00");
            }
            other => panic!("expected HashNotCanonical, got {other:?}"),
        }
    }

    #[test]
    fn stored_line_accepts_one_entry_line_verbatim() {
        let line = StoredLine::new("  9fb4  Artist - Title\r").unwrap();
        assert_eq!(line.as_str(), "  9fb4  Artist - Title\r");
        assert_eq!(line.token(), "9fb4");
    }

    #[test]
    fn stored_line_refuses_what_is_not_one_entry_line() {
        for (raw, expected) in [
            ("a\nb", StoredLineError::NotOneLine),
            ("a\n", StoredLineError::NotOneLine),
            ("", StoredLineError::NotAnEntry),
            ("   ", StoredLineError::NotAnEntry),
            ("# comment", StoredLineError::NotAnEntry),
        ] {
            assert_eq!(StoredLine::new(raw), Err(expected), "{raw:?}");
        }
    }

    #[test]
    fn invalid_edits_convert_to_protocol_errors() {
        let error: ProtocolError = invalid(3, "bad".to_string()).into();
        assert!(matches!(error, ProtocolError::InvalidEdit { op: 3, .. }));
    }
}
