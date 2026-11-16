use crate::args::{parse_count, NewPlaylistArg, OrStdin, PlaylistArg, PlaylistInput, StdinDash};
use crate::ctx::Ctx;
use crate::local_complete::complete_playlists;
use crate::output::{interpret_library, library_error};
use crate::queue::resolve_for_playback;
use crate::stdin::{read_stdin_to_string, require_stdin_hashes_or_usage};
use bpaf::{construct, long, Parser};
use color_eyre::eyre::{bail, eyre};
use color_eyre::Result;
use colored::Colorize;
use hicli::Arg;
use library_ipc_client::{
    entry_tracks, track_hashes, unresolved_entries, CanonicalHash, ClientError, ContentHash,
    EntryTrackError, LibraryResponse, PlaylistEntry, PlaylistName, ProtocolError, TrackInfo,
};
use std::collections::HashSet;
use std::num::NonZeroUsize;

/// Current playlist name
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "FROM", complete = complete_playlists)]
pub(crate) struct FromPlaylist(pub PlaylistArg);

/// New playlist name
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "TO")]
pub(crate) struct ToPlaylist(pub NewPlaylistArg);

// Spelled apart they read `FROM` and `TO`, not `NAME NAME`. TO is a name that does not
// exist yet, so only FROM completes the existing playlists.
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
pub(crate) struct RenameArgs {
    pub(crate) from: FromPlaylist,
    pub(crate) to: ToPlaylist,
}

/// Print playlist only if it contains at least N input tracks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "at-least", metavar = "AT_LEAST")]
pub(crate) struct AtLeastArg(pub NonZeroUsize);

impl std::str::FromStr for AtLeastArg {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_count(s).map(Self)
    }
}

impl Arg for ContainsMode {
    /// At most one of `--all`, `--at-least N`, `--no`; none means any.
    fn parser() -> impl Parser<Self> {
        let all = long("all")
            .help("Print playlist only if it contains ALL input tracks")
            .req_flag(Self::All);
        let at_least = <AtLeastArg as Arg>::parser().map(|AtLeastArg(n)| Self::AtLeast(n));
        let no = long("no")
            .help("Print playlist only if it contains NONE of the input tracks")
            .req_flag(Self::None);
        construct!([all, at_least, no]).fallback(Self::Any)
    }
}

/// Which playlists `playlist contains` prints, relative to the piped-in tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainsMode {
    Any,
    All,
    AtLeast(NonZeroUsize),
    None,
}

impl ContainsMode {
    /// Returns `(threshold, invert)` for matching against `input_len` tracks.
    fn threshold(self, input_len: usize) -> (usize, bool) {
        match self {
            Self::None => (1, true),
            Self::All => (input_len, false),
            Self::AtLeast(n) => (n.get(), false),
            Self::Any => (1, false),
        }
    }
}

fn playlist_content(response: LibraryResponse) -> Option<String> {
    match response {
        LibraryResponse::PlaylistContent(c) => Some(c),
        _ => None,
    }
}

fn playlist_names(response: LibraryResponse) -> Option<Vec<PlaylistName>> {
    match response {
        LibraryResponse::PlaylistNames(names) => Some(names),
        _ => None,
    }
}

/// The first whitespace-delimited token of each line of `reader`; an error when there are none.
fn require_names(reader: impl std::io::BufRead) -> Result<Vec<String>> {
    let names: Vec<String> = reader
        .lines()
        .map_while(std::result::Result::ok)
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect();
    if names.is_empty() {
        bail!("No playlist name provided and stdin was empty");
    }
    Ok(names)
}

fn parse_playlist_name(name: &str) -> Result<PlaylistName> {
    PlaylistName::new(name).map_err(|e| eyre!("Invalid playlist name: {}", e))
}

/// A reply of `Pong`.
fn accept_pong(response: LibraryResponse) -> Option<()> {
    match response {
        LibraryResponse::Pong => Some(()),
        _ => None,
    }
}

/// List all playlists
#[hicli::command]
pub(crate) fn list(ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{DurationSeconds, LibraryRequest};

    let library = ctx.library()?;
    let names = interpret_library(
        library.request(&LibraryRequest::PlaylistList),
        playlist_names,
    )?;

    for name in &names {
        let Some(entries) = entries_unless_removed(library.playlist_entries(name))? else {
            continue;
        };

        let total_secs =
            playlist_seconds(&entries, |h| library.get_track(h)).map_err(library_error)?;

        let duration = DurationSeconds::new(total_secs);
        println!(
            "{}  {}  [{}]",
            name.to_string().bold(),
            tracks_label(entries.len(), unresolved_entries(&entries).count()).green(),
            duration
        );
    }

    Ok(())
}

/// The entries of a playlist, or `None` when it was removed after the names were
/// listed. Any other failure is an error: a playlist is never skipped silently.
fn entries_unless_removed(
    reply: Result<Vec<PlaylistEntry>, ClientError>,
) -> Result<Option<Vec<PlaylistEntry>>> {
    match reply {
        Ok(entries) => Ok(Some(entries)),
        Err(ClientError::Protocol(ProtocolError::PlaylistNotFound { .. })) => Ok(None),
        Err(e) => Err(library_error(e)),
    }
}

/// The total duration, in seconds, of the available entries of a playlist.
///
/// Unavailable entries add nothing (they are counted apart); a failed track
/// lookup fails the sum.
fn playlist_seconds<'a, E>(
    entries: &'a [PlaylistEntry],
    get_track: impl FnMut(&ContentHash) -> Result<TrackInfo, E> + 'a,
) -> Result<u32, E> {
    entry_tracks(entries, get_track)
        .filter_map(|outcome| match outcome {
            Ok(track) => Some(Ok(track.duration.map_or(0, |d| d.value()))),
            Err(EntryTrackError::Unresolved(_)) => None,
            Err(EntryTrackError::Lookup { source, .. }) => Some(Err(source)),
        })
        .sum()
}

/// "N tracks", with "(k unavailable)" after it when some entries are not available.
fn tracks_label(total: usize, unavailable: usize) -> String {
    match unavailable {
        0 => format!("{total} tracks"),
        k => format!("{total} tracks ({k} unavailable)"),
    }
}

/// How many of the `input` tracks are among the available entries of a playlist.
fn matching_inputs(input: &HashSet<CanonicalHash>, entries: &[PlaylistEntry]) -> usize {
    let tracks: HashSet<CanonicalHash> = track_hashes(entries).into_iter().collect();
    input.intersection(&tracks).count()
}

/// One line of `playlist entries`: the full hash, or why the entry is unavailable.
fn entry_line(entry: &PlaylistEntry) -> String {
    match entry {
        PlaylistEntry::Track { hash } => hash.to_string(),
        PlaylistEntry::Unresolved { error } => format!("unavailable: {error}"),
    }
}

/// Print a playlist's entries: one line each, in order, the full hash or why not
///
/// A line that does not resolve is shown in its place, never dropped.
///
/// # Examples
///
/// ```sh
/// mdma playlist entries friday
/// ```
#[hicli::command]
pub(crate) fn entries(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    for entry in library.playlist_entries(&pname).map_err(library_error)? {
        println!("{}", entry_line(&entry));
    }
    Ok(())
}

/// Get playlist content (pipe to mdma queue replace to load)
///
/// Give a name, or `-` to read playlist names from stdin (first whitespace-delimited
/// token per line - handles both bare names and enriched `playlist list` output).
///
/// # Examples
///
/// ```sh
/// mdma playlist get friday | mdma queue replace
/// mdma playlist list | dmenu | mdma playlist get -
/// ```
#[hicli::command]
pub(crate) fn get(name: PlaylistInput, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    match name.0 {
        OrStdin::Value(pname) => {
            let content = interpret_library(
                library.request(&LibraryRequest::PlaylistGet { name: pname }),
                playlist_content,
            )?;
            print!("{}", content);
        }
        OrStdin::Stdin => {
            let names = require_names(std::io::stdin().lock())?;
            for n in names {
                let pname = parse_playlist_name(&n)?;
                let content = interpret_library(
                    library.request(&LibraryRequest::PlaylistGet { name: pname }),
                    playlist_content,
                )?;
                print!("{}", content);
            }
        }
    }
    Ok(())
}

/// Print the name of every playlist that contains any of the tracks piped in on stdin
///
/// Give `-` to read track lines from stdin (8-12 char hex hash as first token).
/// Output: one playlist name per match; pipe to `sort -u` to deduplicate.
///
/// # Examples
///
/// ```sh
/// mdma search --artist CBL | mdma playlist contains -
/// mdma queue list | mdma playlist contains --all -
/// ```
#[hicli::command]
pub(crate) fn contains(mode: ContainsMode, _stdin: StdinDash, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;

    let library = ctx.library()?;

    // 1. Read hashes from stdin, and resolve them to full hashes, as the entries are
    let input_hashes = require_stdin_hashes_or_usage(&[])?;
    let input_hashes = resolve_for_playback(&library, &input_hashes)?;

    // 2. Determine threshold and invert
    let (threshold, invert) = mode.threshold(input_hashes.len());

    let input_set: HashSet<CanonicalHash> = input_hashes.into_iter().collect();

    // 3. Get all playlist names
    let names = interpret_library(
        library.request(&LibraryRequest::PlaylistList),
        playlist_names,
    )?;

    // 4. For each playlist, count how many input hashes appear in it
    for pname in &names {
        let Some(entries) = entries_unless_removed(library.playlist_entries(pname))? else {
            continue;
        };

        let match_count = matching_inputs(&input_set, &entries);

        let should_print = if invert {
            match_count == 0
        } else {
            match_count >= threshold
        };

        if should_print {
            println!("{}", pname);
        }
    }

    Ok(())
}

/// Create a new playlist from stdin (fails if it already exists)
///
/// Give `-` to read the playlist content from stdin.
///
/// # Examples
///
/// ```sh
/// mdma search --genre=Techno | mdma playlist new techno -
/// ```
#[hicli::command]
pub(crate) fn new(name: NewPlaylistArg, _stdin: StdinDash, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let NewPlaylistArg(pname) = name;
    let content = read_stdin_to_string()?;
    interpret_library(
        library.request(&LibraryRequest::PlaylistNew {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    )?;
    eprintln!("Created playlist '{}'", pname);
    Ok(())
}

/// Append stdin to an existing playlist
///
/// Give `-` to read the content to append from stdin.
///
/// # Examples
///
/// ```sh
/// mdma search --artist CBL | mdma playlist append friday -
/// ```
#[hicli::command]
pub(crate) fn append(name: PlaylistArg, _stdin: StdinDash, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string()?;
    interpret_library(
        library.request(&LibraryRequest::PlaylistAppend {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    )?;
    eprintln!("Appended to playlist '{}'", pname);
    Ok(())
}

/// Replace playlist content from stdin
///
/// Give `-` to read the new content from stdin.
///
/// # Examples
///
/// ```sh
/// mdma queue list | mdma playlist replace friday -
/// ```
#[hicli::command]
pub(crate) fn replace(name: PlaylistArg, _stdin: StdinDash, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    let content = read_stdin_to_string()?;
    interpret_library(
        library.request(&LibraryRequest::PlaylistReplace {
            name: pname.clone(),
            content,
        }),
        playlist_content,
    )?;
    eprintln!("Replaced playlist '{}'", pname);
    Ok(())
}

/// Edit playlist in $EDITOR
#[hicli::command]
pub(crate) fn edit(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{LibraryRequest, LibraryResponse, ProtocolError};

    let library = ctx.library()?;
    let PlaylistArg(pname) = name;

    // 1. Get current content (or start empty if not found)
    let current = match library.request(&LibraryRequest::PlaylistGet {
        name: pname.clone(),
    }) {
        Ok(LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. })) => String::new(),
        reply => interpret_library(reply, playlist_content)?,
    };

    // 2. Edit a temp file in $EDITOR
    let new_content = crate::editor::edit_text("mdma-playlist-", &current)?;

    // 3. Replace
    interpret_library(
        library.request(&LibraryRequest::PlaylistReplace {
            name: pname.clone(),
            content: new_content,
        }),
        playlist_content,
    )?;
    eprintln!("Updated playlist '{}'", pname);
    Ok(())
}

/// Remove a playlist
#[hicli::command]
pub(crate) fn remove(name: PlaylistArg, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let PlaylistArg(pname) = name;
    interpret_library(
        library.request(&LibraryRequest::PlaylistRemove {
            name: pname.clone(),
        }),
        accept_pong,
    )?;
    eprintln!("Removed playlist '{}'", pname);
    Ok(())
}

/// Rename a playlist
#[hicli::command]
pub(crate) fn rename(args: RenameArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::LibraryRequest;
    let library = ctx.library()?;
    let FromPlaylist(PlaylistArg(from_name)) = args.from;
    let ToPlaylist(NewPlaylistArg(to_name)) = args.to;
    interpret_library(
        library.request(&LibraryRequest::PlaylistRename {
            from: from_name.clone(),
            to: to_name.clone(),
        }),
        accept_pong,
    )?;
    eprintln!("Renamed playlist '{}' to '{}'", from_name, to_name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use library_ipc_client::EntryError;
    use pretty_assertions::assert_eq;

    use crate::test_support::{parses, run_arg, track as track_info, usage_error};

    use crate::test_support::failure_text;

    #[test]
    fn get_reads_the_first_token_of_each_stdin_line() {
        assert_eq!(
            require_names("mix  3 tracks [1:00]\n\n  other\n".as_bytes()).unwrap(),
            ["mix", "other"]
        );
    }

    #[test]
    fn get_without_a_name_and_with_empty_stdin_is_an_error() {
        assert_eq!(
            failure_text(require_names("\n  \n".as_bytes())),
            "No playlist name provided and stdin was empty"
        );
    }

    #[test]
    fn get_reads_stdin_only_for_a_dash() {
        assert_eq!(parses(&["playlist", "get", "mix"]), ["playlist", "get"]);
        assert_eq!(parses(&["playlist", "get", "-"]), ["playlist", "get"]);
        let err = usage_error(&["playlist", "get"]);
        assert!(err.contains("<NAME|->"), "{err}");
        // A name and `-` do not mix.
        usage_error(&["playlist", "get", "-", "mix"]);
        usage_error(&["playlist", "get", "mix", "-"]);
    }

    #[test]
    fn get_with_an_invalid_name_is_an_error() {
        assert_eq!(parse_playlist_name("mix").unwrap().to_string(), "mix");
        assert_eq!(
            failure_text(parse_playlist_name("a/b")),
            format!(
                "Invalid playlist name: {}",
                PlaylistName::new("a/b").unwrap_err()
            )
        );
    }

    #[test]
    fn rename_args_parser_reads_from_and_to() {
        let name = |n: &str| PlaylistName::new(n).unwrap();
        assert_eq!(
            run_arg::<RenameArgs>(&["old", "new"]),
            Ok(RenameArgs {
                from: FromPlaylist(PlaylistArg(name("old"))),
                to: ToPlaylist(NewPlaylistArg(name("new"))),
            })
        );
        assert!(run_arg::<RenameArgs>(&["old"]).is_err());
        assert!(run_arg::<RenameArgs>(&["old", "has space"]).is_err());
    }

    #[test]
    fn at_least_arg_parser_reads_a_non_zero_count() {
        let two = AtLeastArg(NonZeroUsize::new(2).unwrap());
        assert_eq!(run_arg::<AtLeastArg>(&["--at-least", "2"]), Ok(two));
        let err = run_arg::<AtLeastArg>(&["--at-least", "0"]).unwrap_err();
        assert!(
            err.ends_with("`--at-least`: must be a whole number of at least 1"),
            "{err}"
        );
        assert!(run_arg::<AtLeastArg>(&[]).is_err());
    }

    #[test]
    fn contains_mode_parser_picks_at_most_one_mode() {
        let three = NonZeroUsize::new(3).unwrap();
        assert_eq!(run_arg::<ContainsMode>(&[]), Ok(ContainsMode::Any));
        assert_eq!(run_arg::<ContainsMode>(&["--all"]), Ok(ContainsMode::All));
        assert_eq!(run_arg::<ContainsMode>(&["--no"]), Ok(ContainsMode::None));
        assert_eq!(
            run_arg::<ContainsMode>(&["--at-least", "3"]),
            Ok(ContainsMode::AtLeast(three))
        );
        assert!(run_arg::<ContainsMode>(&["--all", "--no"]).is_err());
        assert!(run_arg::<ContainsMode>(&["--all", "--at-least", "3"]).is_err());
        let err = run_arg::<ContainsMode>(&["--at-least", "0"]).unwrap_err();
        assert!(
            err.contains("must be a whole number of at least 1"),
            "{err}"
        );
    }

    fn argv<'a>(args: &[&'a str]) -> Vec<&'a str> {
        std::iter::once("playlist")
            .chain(args.iter().copied())
            .collect()
    }

    /// The path `mdma playlist <args>` reaches.
    fn playlist(args: &[&str]) -> Vec<&'static str> {
        parses(&argv(args))
    }

    /// `mdma playlist <args>` must be rejected as a usage error.
    fn playlist_usage_error(args: &[&str]) -> String {
        usage_error(&argv(args))
    }

    #[test]
    fn invalid_playlist_name_is_a_usage_error() {
        for cmd in ["new", "append", "replace", "edit", "remove", "get"] {
            playlist_usage_error(&[cmd, "has space"]);
            playlist_usage_error(&[cmd, "a/b"]);
        }
        playlist_usage_error(&["rename", "bad name", "ok"]);
        playlist_usage_error(&["rename", "ok", "bad/name"]);
    }

    #[test]
    fn valid_playlist_names_are_accepted() {
        assert_eq!(playlist(&["new", "my-list_1", "-"]), ["playlist", "new"]);
        assert_eq!(playlist(&["get", "-"]), ["playlist", "get"]);
        assert_eq!(playlist(&["get", "mix"]), ["playlist", "get"]);
        assert_eq!(playlist(&["rename", "a", "b"]), ["playlist", "rename"]);
        assert_eq!(
            run_arg::<PlaylistArg>(&["my-list_1"])
                .unwrap()
                .0
                .to_string(),
            "my-list_1"
        );
    }

    #[test]
    fn contains_conflicting_flags_are_a_usage_error() {
        playlist_usage_error(&["contains", "--all", "--no"]);
        playlist_usage_error(&["contains", "--all", "--at-least", "2"]);
        playlist_usage_error(&["contains", "--no", "--at-least", "2"]);
    }

    #[test]
    fn contains_zero_at_least_is_a_usage_error() {
        for bad in ["0", "abc", "1.5", ""] {
            let err = playlist_usage_error(&["contains", "--at-least", bad]);
            assert!(
                err.contains("must be a whole number of at least 1"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn contains_flags_build_the_right_mode() {
        assert_eq!(playlist(&["contains", "-"]), ["playlist", "contains"]);
        assert_eq!(run_arg::<ContainsMode>(&[]), Ok(ContainsMode::Any));
        assert_eq!(
            run_arg::<ContainsMode>(&["--at-least", "2"]),
            Ok(ContainsMode::AtLeast(NonZeroUsize::new(2).unwrap()))
        );
    }

    #[test]
    fn contains_threshold_default_is_one_not_inverted() {
        assert_eq!(ContainsMode::Any.threshold(3), (1, false));
    }

    #[test]
    fn contains_threshold_all_uses_input_len() {
        assert_eq!(ContainsMode::All.threshold(5), (5, false));
    }

    #[test]
    fn contains_threshold_at_least_uses_given_value() {
        let mode = ContainsMode::AtLeast(NonZeroUsize::new(3).unwrap());
        assert_eq!(mode.threshold(7), (3, false));
    }

    #[test]
    fn contains_threshold_no_flag_inverts() {
        assert_eq!(ContainsMode::None.threshold(4), (1, true));
    }

    fn full(c: char) -> CanonicalHash {
        CanonicalHash::try_from(ContentHash::new(format!(
            "sha256:{}",
            c.to_string().repeat(64)
        )))
        .unwrap()
    }

    fn track(c: char) -> PlaylistEntry {
        PlaylistEntry::Track { hash: full(c) }
    }

    fn unknown(token: &str) -> PlaylistEntry {
        PlaylistEntry::Unresolved {
            error: EntryError::Unknown {
                hash: ContentHash::new(token),
            },
        }
    }

    /// Only the available entries are matched: an unresolved entry is not counted,
    /// even when the input holds a full hash that happens to be what it spelled.
    ///
    /// (Resolving the piped-in hashes to full ones is `LibraryBackend::resolve_for_playback`,
    /// which needs a live library; there is no fake backend to test that step against.)
    #[test]
    fn contains_matches_available_entries_and_ignores_unresolved_ones() {
        let entries = [track('a'), unknown("bbbbbbbb"), track('c')];
        let input: HashSet<CanonicalHash> = [full('a'), full('b')].into_iter().collect();
        assert_eq!(matching_inputs(&input, &entries), 1);
    }

    #[test]
    fn contains_counts_an_input_once_however_often_the_playlist_repeats_it() {
        let entries = [track('a'), track('a'), track('a')];
        let input: HashSet<CanonicalHash> = [full('a'), full('c')].into_iter().collect();
        assert_eq!(matching_inputs(&input, &entries), 1);
    }

    #[test]
    fn unavailable_entries_are_counted_in_the_track_count() {
        assert_eq!(tracks_label(5, 0), "5 tracks");
        assert_eq!(tracks_label(5, 2), "5 tracks (2 unavailable)");
    }

    #[test]
    fn unresolved_entries_are_reported_by_position() {
        let entries = [
            track('a'),
            unknown("bbbbbbbb"),
            track('c'),
            unknown("dddddddd"),
        ];
        let lines: Vec<String> = unresolved_entries(&entries)
            .map(|u| u.to_string())
            .collect();
        assert_eq!(
            lines,
            [
                "entry 2: unknown track bbbbbbbb",
                "entry 4: unknown track dddddddd"
            ]
        );
        assert_eq!(unresolved_entries(&entries).count(), 2);
    }

    fn timed(secs: u32) -> TrackInfo {
        TrackInfo {
            duration: Some(library_ipc_client::DurationSeconds::new(secs)),
            ..track_info("sha256:aa000001")
        }
    }

    #[test]
    fn playlist_seconds_sums_available_entries_and_skips_unresolved_ones() {
        let entries = [track('a'), unknown("bbbbbbbb"), track('c'), track('a')];
        let total = playlist_seconds(&entries, |h: &ContentHash| {
            Ok::<_, String>(if h == full('c').as_content_hash() {
                timed(100)
            } else {
                timed(10)
            })
        });
        assert_eq!(total, Ok(120));
    }

    #[test]
    fn playlist_seconds_fails_on_a_failed_lookup() {
        let entries = [track('a'), track('c')];
        let total = playlist_seconds(&entries, |h: &ContentHash| {
            if h == full('c').as_content_hash() {
                Err("socket closed".to_string())
            } else {
                Ok(timed(10))
            }
        });
        assert_eq!(total, Err("socket closed".to_string()));
    }

    #[test]
    fn a_removed_playlist_is_skipped_and_any_other_failure_is_not() {
        let gone = ClientError::Protocol(ProtocolError::PlaylistNotFound {
            name: "mix".to_string(),
        });
        assert_eq!(entries_unless_removed(Err(gone)).unwrap(), None);

        let entries = vec![track('a')];
        assert_eq!(
            entries_unless_removed(Ok(entries.clone())).unwrap(),
            Some(entries)
        );

        let broken = ClientError::Protocol(ProtocolError::Internal {
            message: "disk on fire".to_string(),
        });
        assert_eq!(
            failure_text(entries_unless_removed(Err(broken))),
            "internal error: disk on fire"
        );
    }

    #[test]
    fn entries_lines_show_the_hash_or_why_it_is_unavailable() {
        assert_eq!(
            entry_line(&track('a')),
            format!("sha256:{}", "a".repeat(64))
        );
        assert_eq!(
            entry_line(&unknown("bbbbbbbb")),
            "unavailable: unknown track bbbbbbbb"
        );
    }
}
