use crate::args::NewPlaylistArg;
use crate::ctx::Ctx;
use crate::library::Yes;
use crate::output::{library_error, unexpected_response};
use chrono::{DateTime, Local, TimeZone};
use color_eyre::eyre::eyre;
use color_eyre::Result;
use hicli::{Arg, Failure};
use library_ipc_client::{
    BinEntry, BinId, ClientError, LibraryRequest, LibraryResponse, PlaylistName, ProtocolError,
};
use mdma_client::LibraryBackend;

/// Id of a binned playlist, as `playlist bin ls` prints it: NAME.UNIX-MS
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(positional = "ID")]
pub(crate) struct BinIdArg(pub BinId);

/// Restore under this name instead of the original one
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "as", metavar = "NAME")]
pub(crate) struct RestoreAs(pub Option<NewPlaylistArg>);

/// The library's answer, `accept`ed, with the bin's refusals in their own words.
/// A reply `accept` declines is unexpected.
fn bin_answer<T>(
    reply: Result<LibraryResponse, ClientError>,
    accept: impl FnOnce(LibraryResponse) -> Option<T>,
) -> Result<T> {
    let response = match reply {
        Ok(LibraryResponse::Error(e)) => return Err(bin_error(ClientError::Protocol(e))),
        Ok(response) => response,
        Err(e) => return Err(bin_error(e)),
    };
    accept(response).ok_or_else(|| unexpected_response("library"))
}

fn bin_error(err: ClientError) -> color_eyre::Report {
    match err {
        ClientError::Protocol(ProtocolError::PlaylistAlreadyExists { name }) => eyre!(
            "A playlist named '{name}' already exists; restore under another name with --as <name>"
        ),
        ClientError::Protocol(ProtocolError::BinEntryNotFound { id }) => {
            eyre!("'{id}' is not in the bin; list the bin with: mdma playlist bin ls")
        }
        other => library_error(other),
    }
}

fn accept_entries(response: LibraryResponse) -> Option<Vec<BinEntry>> {
    match response {
        LibraryResponse::BinEntries(entries) => Some(entries),
        _ => None,
    }
}

/// How a `PlaylistBin` ended, when the library answered it.
enum Binned {
    Id(BinId),
    Conflict,
}

fn accept_binned(response: LibraryResponse) -> Option<Binned> {
    match response {
        LibraryResponse::PlaylistBinned { id } => Some(Binned::Id(id)),
        LibraryResponse::PlaylistDeleteConflict { .. } => Some(Binned::Conflict),
        _ => None,
    }
}

/// Move `name` to the bin as it is now (read its version, then bin at that version),
/// and give the id it is under. A playlist that changed in between is not binned.
/// A node that does not know `PlaylistBin` refuses, and deletes nothing.
pub(crate) fn bin_playlist(library: &LibraryBackend, name: &PlaylistName) -> Result<BinId> {
    let version = bin_answer(
        library.request(&LibraryRequest::PlaylistSnapshot { name: name.clone() }),
        |response| match response {
            LibraryResponse::PlaylistSnapshot(snapshot) => Some(snapshot.version),
            _ => None,
        },
    )?;
    let binned = bin_answer(
        library.request(&LibraryRequest::PlaylistBin {
            name: name.clone(),
            version,
        }),
        accept_binned,
    )?;
    match binned {
        Binned::Id(id) => Ok(id),
        Binned::Conflict => Err(eyre!(
            "'{name}' changed while it was being removed; nothing was binned. Run the command again"
        )),
    }
}

/// Move a binned playlist back, under `as_name` when given; the name it has now.
pub(crate) fn restore_playlist(
    library: &LibraryBackend,
    id: BinId,
    as_name: Option<PlaylistName>,
) -> Result<PlaylistName> {
    bin_answer(
        library.request(&LibraryRequest::PlaylistRestore { id, as_name }),
        |response| match response {
            LibraryResponse::PlaylistRestored { name } => Some(name),
            _ => None,
        },
    )
}

/// What `playlist rm` says: where the playlist went and how to get it back.
pub(crate) fn binned_message(name: &PlaylistName, id: &BinId) -> String {
    format!("Moved '{name}' to the bin (id {id}). Restore with: mdma playlist bin restore {id}")
}

/// One line of `playlist bin ls`: id, name, binned time, track count. Plain text,
/// two spaces apart, the id first so that `cut -d' ' -f1` gives it.
fn bin_line<Tz: TimeZone>(entry: &BinEntry, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let at: DateTime<Tz> = DateTime::<chrono::Utc>::from(entry.id.binned_at()).with_timezone(tz);
    format!(
        "{}  {}  {}  {}",
        entry.id,
        entry.id.name(),
        at.format("%Y-%m-%d %H:%M"),
        super::tracks_label(entry.entry_count, 0)
    )
}

/// List the bin, newest first: id, name, when it was binned, track count
///
/// The id comes first, so `mdma playlist bin ls | cut -d' ' -f1` gives the ids.
///
/// # Examples
///
/// ```sh
/// mdma playlist bin ls
/// ```
#[hicli::command]
pub(crate) fn ls(ctx: &Ctx) -> Result<()> {
    let library = ctx.node().library()?;
    let entries = bin_answer(library.request(&LibraryRequest::BinList), accept_entries)?;
    if entries.is_empty() {
        eprintln!("The bin is empty");
    }
    for entry in &entries {
        println!("{}", bin_line(entry, &Local));
    }
    Ok(())
}

/// Move a binned playlist back, under its old name or another
///
/// When a playlist of that name exists already nothing is restored: give another
/// name with `--as`.
///
/// # Examples
///
/// ```sh
/// mdma playlist bin restore friday.1763470000123
/// mdma playlist bin restore friday.1763470000123 --as friday-old
/// ```
#[hicli::command]
pub(crate) fn restore(as_name: RestoreAs, id: BinIdArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.node().library()?;
    let as_name = as_name.0.map(|NewPlaylistArg(name)| name);
    let name = restore_playlist(&library, id.0, as_name)?;
    eprintln!("Restored playlist '{name}'");
    Ok(())
}

/// Delete one binned playlist for good
///
/// This cannot be undone.
#[hicli::command]
pub(crate) fn purge(id: BinIdArg, ctx: &Ctx) -> Result<()> {
    let library = ctx.node().library()?;
    let id = id.0;
    bin_answer(
        library.request(&LibraryRequest::BinPurge { id: id.clone() }),
        super::accept_pong,
    )?;
    eprintln!("Purged '{id}'");
    Ok(())
}

/// The refusal for `empty` without `--yes`.
fn require_yes(yes: Yes) -> Result<(), Failure> {
    if yes.0 {
        Ok(())
    } else {
        Err(Failure::usage(
            "emptying the bin cannot be undone: pass --yes to confirm",
        ))
    }
}

/// Delete every binned playlist for good
///
/// Needs `--yes`: this cannot be undone.
///
/// # Arguments
///
/// * `yes` - Confirm: the binned playlists cannot be recovered
#[hicli::command]
pub(crate) fn empty(yes: Yes, ctx: &Ctx) -> hicli::Outcome {
    require_yes(yes)?;
    let library = ctx.node().library()?;
    bin_answer(
        library.request(&LibraryRequest::BinEmpty),
        super::accept_pong,
    )?;
    eprintln!("Emptied the bin");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        failure_text, help, parses, run_arg, usage_error, FakeServices, Setup,
    };
    use chrono::Utc;
    use library_service_stub::LibraryService;
    use pretty_assertions::assert_eq;
    use std::sync::{Arc, Mutex};

    fn id(s: &str) -> BinId {
        s.parse().unwrap()
    }

    fn name(s: &str) -> PlaylistName {
        PlaylistName::new(s).unwrap()
    }

    fn bin(args: &[&str]) -> Vec<&'static str> {
        let mut argv = vec!["playlist", "bin"];
        argv.extend_from_slice(args);
        parses(&argv)
    }

    fn bin_usage_error(args: &[&str]) -> String {
        let mut argv = vec!["playlist", "bin"];
        argv.extend_from_slice(args);
        usage_error(&argv)
    }

    #[test]
    fn every_bin_subcommand_parses() {
        assert_eq!(bin(&["ls"]), ["playlist", "bin", "ls"]);
        assert_eq!(
            bin(&["restore", "friday.1763470000123"]),
            ["playlist", "bin", "restore"]
        );
        assert_eq!(
            bin(&["purge", "friday.1763470000123"]),
            ["playlist", "bin", "purge"]
        );
        assert_eq!(bin(&["empty", "--yes"]), ["playlist", "bin", "empty"]);
        assert_eq!(bin(&["empty", "-y"]), ["playlist", "bin", "empty"]);
    }

    #[test]
    fn restore_takes_an_optional_new_name_before_or_after_the_id() {
        for args in [
            &["restore", "--as", "other", "friday.42"][..],
            &["restore", "friday.42", "--as", "other"],
            &["restore", "friday.42", "--as=other"],
        ] {
            assert_eq!(bin(args), ["playlist", "bin", "restore"]);
        }
        assert_eq!(
            run_arg::<RestoreAs>(&["--as", "other"]),
            Ok(RestoreAs(Some(NewPlaylistArg(name("other")))))
        );
        assert_eq!(run_arg::<RestoreAs>(&[]), Ok(RestoreAs(None)));
        bin_usage_error(&["restore", "friday.42", "--as", "bad name"]);
        bin_usage_error(&["restore", "friday.42", "--as"]);
    }

    #[test]
    fn a_bad_id_is_a_usage_error_with_the_parse_error() {
        for cmd in ["restore", "purge"] {
            let err = bin_usage_error(&[cmd, "friday"]);
            assert!(err.contains("bin id must be <name>.<unix-ms>"), "{err}");
            bin_usage_error(&[cmd, "a/b.42"]);
            bin_usage_error(&[cmd, "friday.x"]);
            bin_usage_error(&[cmd]);
        }
        assert_eq!(
            run_arg::<BinIdArg>(&["friday.42"]),
            Ok(BinIdArg(id("friday.42")))
        );
    }

    #[test]
    fn empty_without_yes_parses_and_is_refused_with_a_usage_failure() {
        assert_eq!(bin(&["empty"]), ["playlist", "bin", "empty"]);
        let refused = require_yes(Yes(false)).unwrap_err();
        assert!(
            refused.message().contains("cannot be undone") && refused.message().contains("--yes"),
            "{}",
            refused.message()
        );
        assert_eq!(require_yes(Yes(true)), Ok(()));
    }

    #[test]
    fn the_bin_group_and_its_commands_are_in_help() {
        let help = help(&["playlist", "bin", "--help"]);
        for word in ["ls", "restore", "purge", "empty"] {
            assert!(help.contains(word), "{word}: {help}");
        }
        bin_usage_error(&["bogus"]);
    }

    #[test]
    fn rm_is_the_remove_command() {
        assert_eq!(parses(&["playlist", "rm", "set"]), ["playlist", "remove"]);
    }

    #[test]
    fn rm_says_where_the_playlist_went_and_how_to_restore_it() {
        assert_eq!(
            binned_message(&name("x"), &id("x.1763470000123")),
            "Moved 'x' to the bin (id x.1763470000123). Restore with: mdma playlist bin restore x.1763470000123"
        );
    }

    fn entry(s: &str, entry_count: usize) -> BinEntry {
        BinEntry {
            id: id(s),
            entry_count,
        }
    }

    #[test]
    fn a_bin_line_is_id_name_time_and_tracks_in_plain_text() {
        // 1763470000123 ms is 2025-11-18 12:46:40 UTC.
        let line = bin_line(&entry("friday.1763470000123", 3), &Utc);
        assert_eq!(
            line,
            "friday.1763470000123  friday  2025-11-18 12:46  3 tracks"
        );
        assert!(!line.contains('\x1b'));
        assert_eq!(line.split(' ').next(), Some("friday.1763470000123"));
    }

    fn protocol(e: ProtocolError) -> Result<LibraryResponse, ClientError> {
        Err(ClientError::Protocol(e))
    }

    fn answer(reply: Result<LibraryResponse, ClientError>) -> Result<()> {
        bin_answer(reply, super::super::accept_pong)
    }

    #[test]
    fn a_name_clash_says_to_use_as() {
        let text = failure_text(answer(protocol(ProtocolError::PlaylistAlreadyExists {
            name: "friday".into(),
        })));
        assert_eq!(
            text,
            "A playlist named 'friday' already exists; restore under another name with --as <name>"
        );
    }

    #[test]
    fn a_missing_entry_says_it_is_not_in_the_bin() {
        let text = failure_text(answer(Ok(LibraryResponse::Error(
            ProtocolError::BinEntryNotFound {
                id: "friday.42".into(),
            },
        ))));
        assert_eq!(
            text,
            "'friday.42' is not in the bin; list the bin with: mdma playlist bin ls"
        );
    }

    #[test]
    fn other_failures_read_as_everywhere_else() {
        let text = failure_text(answer(protocol(ProtocolError::Internal {
            message: "disk on fire".into(),
        })));
        assert_eq!(text, "internal error: disk on fire");
    }

    #[test]
    fn a_reply_that_is_not_accepted_is_unexpected() {
        assert_eq!(
            failure_text(bin_answer(Ok(LibraryResponse::Pong), accept_entries)),
            "Unexpected response from library"
        );
    }

    #[test]
    fn bin_entries_keep_the_librarys_order() {
        let sent = vec![entry("b.20", 1), entry("a.30", 2), entry("a.10", 0)];
        let got = bin_answer(
            Ok(LibraryResponse::BinEntries(sent.clone())),
            accept_entries,
        );
        assert_eq!(got.unwrap(), sent);
    }

    // ── against a fake library ───────────────────────────────────────────────

    /// A fake whose library is the stub service, with a playlist `x` in it.
    fn stub_fake() -> (FakeServices, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let service = LibraryService::from_metadata_dir(dir.path().to_path_buf());
        create(&service, "x");
        let fake = FakeServices::start(Setup {
            library: Some(Box::new(move |request| service.handle_request(request))),
            ..Setup::default()
        });
        (fake, dir)
    }

    fn create(service: &LibraryService, playlist: &str) {
        let response = service.handle_request(LibraryRequest::PlaylistNew {
            name: name(playlist),
            content: String::new(),
        });
        assert!(
            !matches!(response, LibraryResponse::Error(_)),
            "{response:?}"
        );
    }

    fn scripted(
        script: impl Fn(LibraryRequest) -> LibraryResponse + Send + 'static,
    ) -> FakeServices {
        FakeServices::start(Setup {
            library: Some(Box::new(script)),
            ..Setup::default()
        })
    }

    #[test]
    fn rm_bins_the_playlist_and_gives_the_exact_id() {
        let (fake, _dir) = stub_fake();
        let binned = bin_playlist(&fake.library, &name("x")).unwrap();
        assert_eq!(binned.name(), &name("x"));
        let listed = bin_answer(
            fake.library.request(&LibraryRequest::BinList),
            accept_entries,
        );
        assert_eq!(
            listed
                .unwrap()
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            [binned]
        );
    }

    #[test]
    fn rm_of_a_missing_playlist_fails_as_usual() {
        let (fake, _dir) = stub_fake();
        assert_eq!(
            failure_text(bin_playlist(&fake.library, &name("nope"))),
            "playlist not found: nope"
        );
    }

    fn snapshot() -> library_ipc_client::PlaylistSnapshot {
        library_ipc_client::PlaylistSnapshot {
            version: library_ipc_client::PlaylistVersion::of(b"x"),
            entries: vec![],
        }
    }

    #[test]
    fn rm_bins_at_the_version_it_read_and_does_not_retry_a_conflict() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        let fake = scripted(move |request| {
            log.lock().unwrap().push(request.clone());
            match request {
                LibraryRequest::PlaylistSnapshot { .. } => {
                    LibraryResponse::PlaylistSnapshot(snapshot())
                }
                _ => LibraryResponse::PlaylistDeleteConflict {
                    current: snapshot(),
                },
            }
        });
        assert_eq!(
            failure_text(bin_playlist(&fake.library, &name("x"))),
            "'x' changed while it was being removed; nothing was binned. Run the command again"
        );
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 2, "no retry: {asked:?}");
        match &asked[1] {
            LibraryRequest::PlaylistBin { name: n, version } => {
                assert_eq!((n, version), (&name("x"), &snapshot().version));
            }
            other => panic!("expected PlaylistBin, got {other:?}"),
        }
    }

    #[test]
    fn rm_on_an_old_node_says_to_upgrade_and_asks_nothing_that_deletes() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        let fake = scripted(move |request| {
            log.lock().unwrap().push(request.clone());
            match request {
                LibraryRequest::PlaylistSnapshot { .. } => {
                    LibraryResponse::PlaylistSnapshot(snapshot())
                }
                _ => LibraryResponse::Error(ProtocolError::InvalidRequest {
                    message: "unknown variant `PlaylistBin`".into(),
                }),
            }
        });
        assert_eq!(
            failure_text(bin_playlist(&fake.library, &name("x"))),
            mdma_client::upgrade_node_text("CLI")
        );
        assert!(asked
            .lock()
            .unwrap()
            .iter()
            .all(|r| !matches!(r, LibraryRequest::PlaylistRemove { .. })));
    }

    #[test]
    fn restore_puts_the_playlist_back_under_its_name_or_another() {
        let (fake, _dir) = stub_fake();
        let first = bin_playlist(&fake.library, &name("x")).unwrap();
        assert_eq!(
            restore_playlist(&fake.library, first, None).unwrap(),
            name("x")
        );
        let second = bin_playlist(&fake.library, &name("x")).unwrap();
        assert_eq!(
            restore_playlist(&fake.library, second, Some(name("y"))).unwrap(),
            name("y")
        );
    }

    #[test]
    fn restore_onto_a_taken_name_says_to_use_as() {
        let (fake, _dir) = stub_fake();
        let binned = bin_playlist(&fake.library, &name("x")).unwrap();
        let again = fake.library.request(&LibraryRequest::PlaylistNew {
            name: name("x"),
            content: String::new(),
        });
        assert!(again.is_ok(), "{again:?}");
        assert_eq!(
            failure_text(restore_playlist(&fake.library, binned, None)),
            "A playlist named 'x' already exists; restore under another name with --as <name>"
        );
    }

    #[test]
    fn restore_of_an_id_not_in_the_bin_says_so() {
        let (fake, _dir) = stub_fake();
        assert_eq!(
            failure_text(restore_playlist(&fake.library, id("x.42"), None)),
            "'x.42' is not in the bin; list the bin with: mdma playlist bin ls"
        );
    }
}
