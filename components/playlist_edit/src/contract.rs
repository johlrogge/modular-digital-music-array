//! The ADR-027 contract for playlist edits, stated once for every library
//! service (the real one and the stub), so that console and BDD tests see the
//! same rules.
//!
//! Each function is one scenario. A service's tests call it with a `Setup`
//! that builds a service holding tracks [`track_a`], [`track_b1`] and
//! [`track_b2`] (the last two share the prefix `ab`) and the playlist `set`
//! holding the given content, and answers requests for it. Bytes are read back
//! with `PlaylistGet`.

use library_ipc_protocol::{
    CanonicalHash, ContentHash, Duplicates, EditOp, EditOutcome, EntryError, LibraryRequest,
    LibraryResponse, PlaylistEntry, PlaylistName, PlaylistSnapshot, PlaylistVersion, ProtocolError,
    Skipped, StoredPlaylist,
};

/// Generates one `#[test]` per scenario of this module, each run against the
/// service that `$setup` (a `Setup`) builds. The one list of scenarios.
#[macro_export]
macro_rules! playlist_contract_tests {
    ($setup:expr) => {
        $crate::playlist_contract_tests!(
            @scenarios $setup;
            snapshot_has_the_version_of_the_bytes_and_the_entries,
            stale_edit_conflicts_with_the_current_snapshot_and_writes_nothing,
            applied_edit_reports_the_version_of_the_new_bytes,
            lines_an_edit_did_not_touch_stay_byte_identical,
            an_edit_that_skips_everything_changes_nothing,
            keep_adds_a_duplicate_again,
            revert_restores_the_exact_bytes_and_its_own_previous_redoes_the_edit,
            stale_revert_conflicts_and_writes_nothing,
            versioned_delete_deletes,
            stale_delete_conflicts_and_keeps_the_file,
            rename_is_refused_onto_an_existing_playlist,
            remove_deletes_unconditionally,
            a_refused_insert_refuses_the_whole_edit_and_writes_nothing,
            a_missing_playlist_is_not_found_for_every_operation,
            a_crlf_playlist_keeps_its_bytes_through_read_and_edit,
            append_joins_on_a_line_break,
        );
    };
    (@scenarios $setup:expr; $($scenario:ident),* $(,)?) => {
        $(
            #[test]
            fn $scenario() {
                $crate::contract::$scenario(&$setup);
            }
        )*
    };
}

/// A service under test: request in, response out.
pub type Handler = Box<dyn Fn(LibraryRequest) -> LibraryResponse>;

/// Builds a service holding the playlist `set` with the given content.
pub type Setup<'a> = &'a dyn Fn(&str) -> Handler;

pub fn full_hash(prefix: &str, fill: char) -> String {
    format!(
        "sha256:{prefix}{}",
        fill.to_string().repeat(64 - prefix.len())
    )
}

pub fn track_a() -> String {
    full_hash("aaaa", 'a')
}

pub fn track_b1() -> String {
    full_hash("ab", '1')
}

pub fn track_b2() -> String {
    full_hash("ab", '2')
}

/// Entries: A (short token, trailing text), B1 (full), an unknown track; with
/// comments and a blank line between.
pub fn content() -> String {
    format!(
        "# set\naaaaaaaa  Artist - A  [3:00]\n\n# note\n{}\nffffffff  # unknown\n",
        track_b1()
    )
}

pub fn set() -> PlaylistName {
    PlaylistName::new("set").expect("valid name")
}

fn get(handler: &Handler, name: &PlaylistName) -> String {
    match handler(LibraryRequest::PlaylistGet { name: name.clone() }) {
        LibraryResponse::PlaylistContent(content) => content,
        other => panic!("expected PlaylistContent, got {other:?}"),
    }
}

fn bytes(handler: &Handler) -> String {
    get(handler, &set())
}

fn snapshot(handler: &Handler) -> PlaylistSnapshot {
    match handler(LibraryRequest::PlaylistSnapshot { name: set() }) {
        LibraryResponse::PlaylistSnapshot(snapshot) => snapshot,
        other => panic!("expected PlaylistSnapshot, got {other:?}"),
    }
}

fn edit(handler: &Handler, version: &PlaylistVersion, ops: Vec<EditOp>) -> LibraryResponse {
    handler(LibraryRequest::PlaylistEdit {
        name: set(),
        version: version.clone(),
        ops,
    })
}

fn revert(handler: &Handler, version: &PlaylistVersion, to: &StoredPlaylist) -> LibraryResponse {
    handler(LibraryRequest::PlaylistRevert {
        name: set(),
        version: version.clone(),
        content: to.clone(),
    })
}

/// What `Applied` carries.
struct Done {
    snapshot: PlaylistSnapshot,
    skipped: Vec<Skipped>,
    previous: Option<StoredPlaylist>,
}

impl Done {
    /// The file as it was before an edit that wrote: what Undo sends back.
    fn undo_point(&self) -> &StoredPlaylist {
        self.previous
            .as_ref()
            .expect("an edit that wrote has a previous")
    }
}

fn applied(response: LibraryResponse) -> Done {
    match response {
        LibraryResponse::PlaylistEdited(EditOutcome::Applied {
            snapshot,
            skipped,
            previous,
        }) => Done {
            snapshot,
            skipped,
            previous,
        },
        other => panic!("expected Applied, got {other:?}"),
    }
}

fn conflict(response: LibraryResponse) -> PlaylistSnapshot {
    match response {
        LibraryResponse::PlaylistEdited(EditOutcome::Conflict { current }) => current,
        other => panic!("expected Conflict, got {other:?}"),
    }
}

fn track(hash: &str) -> PlaylistEntry {
    PlaylistEntry::Track {
        hash: CanonicalHash::try_from(ContentHash::new(hash)).expect("a full hash"),
    }
}

fn unknown_entry() -> PlaylistEntry {
    PlaylistEntry::Unresolved {
        error: EntryError::Unknown {
            hash: ContentHash::new("ffffffff"),
        },
    }
}

fn insert(at: usize, hash: &str, duplicates: Duplicates) -> EditOp {
    EditOp::Insert {
        at,
        hashes: vec![ContentHash::new(hash)],
        duplicates,
    }
}

fn stale() -> PlaylistVersion {
    PlaylistVersion::of(b"not the playlist")
}

fn assert_not_found(response: LibraryResponse) {
    assert!(
        matches!(
            response,
            LibraryResponse::Error(ProtocolError::PlaylistNotFound { .. })
        ),
        "expected PlaylistNotFound, got {response:?}"
    );
}

pub fn snapshot_has_the_version_of_the_bytes_and_the_entries(setup: Setup) {
    let handler = setup(&content());
    let snapshot = snapshot(&handler);
    assert_eq!(snapshot.version, PlaylistVersion::of(content().as_bytes()));
    assert_eq!(
        snapshot.entries,
        vec![track(&track_a()), track(&track_b1()), unknown_entry()]
    );
}

pub fn stale_edit_conflicts_with_the_current_snapshot_and_writes_nothing(setup: Setup) {
    let handler = setup(&content());
    let current = snapshot(&handler);
    let response = edit(
        &handler,
        &stale(),
        vec![insert(0, &track_b2(), Duplicates::Skip)],
    );
    assert_eq!(conflict(response), current);
    assert_eq!(bytes(&handler), content());
}

pub fn applied_edit_reports_the_version_of_the_new_bytes(setup: Setup) {
    let handler = setup(&content());
    let before = snapshot(&handler);
    let done = applied(edit(
        &handler,
        &before.version,
        vec![insert(1, &track_b2(), Duplicates::Skip)],
    ));
    let new = bytes(&handler);
    assert_eq!(done.snapshot.version, PlaylistVersion::of(new.as_bytes()));
    assert_eq!(done.snapshot, snapshot(&handler));
    assert_eq!(
        done.snapshot.entries,
        vec![
            track(&track_a()),
            track(&track_b2()),
            track(&track_b1()),
            unknown_entry()
        ]
    );
    assert!(done.skipped.is_empty());
    assert_eq!(done.undo_point().as_str(), content());
}

pub fn lines_an_edit_did_not_touch_stay_byte_identical(setup: Setup) {
    let handler = setup(&content());
    let version = snapshot(&handler).version;

    // The new line goes before entry 1, which is line 4.
    done_insert(&handler, &version);
    let new = bytes(&handler);
    let mut lines: Vec<&str> = new.lines().collect();
    lines.remove(4);
    assert_eq!(format!("{}\n", lines.join("\n")), content());

    // A move trades the entries' places; comments and blank lines stay put.
    let handler = setup(&content());
    let version = snapshot(&handler).version;
    applied(edit(
        &handler,
        &version,
        vec![EditOp::Move { from: 0, to: 1 }],
    ));
    assert_eq!(
        bytes(&handler),
        format!(
            "# set\n{}\n\n# note\naaaaaaaa  Artist - A  [3:00]\nffffffff  # unknown\n",
            track_b1()
        )
    );

    // A remove drops that line only, the `# reason` text of its neighbours included.
    let handler = setup(&content());
    let version = snapshot(&handler).version;
    applied(edit(&handler, &version, vec![EditOp::Remove { at: 0 }]));
    assert_eq!(
        bytes(&handler),
        format!("# set\n\n# note\n{}\nffffffff  # unknown\n", track_b1())
    );
}

fn done_insert(handler: &Handler, version: &PlaylistVersion) {
    applied(edit(
        handler,
        version,
        vec![insert(1, &track_b2(), Duplicates::Skip)],
    ));
}

pub fn an_edit_that_skips_everything_changes_nothing(setup: Setup) {
    let handler = setup(&content());
    let before = snapshot(&handler);
    let done = applied(edit(
        &handler,
        &before.version,
        vec![insert(2, &track_a(), Duplicates::Skip)],
    ));
    assert_eq!(
        done.skipped,
        vec![Skipped {
            hash: ContentHash::new(track_a()),
            existing_at: 0
        }]
    );
    assert_eq!(done.snapshot, before);
    assert_eq!(
        done.previous, None,
        "nothing was written, so nothing to undo"
    );
    assert_eq!(bytes(&handler), content());

    let done = applied(edit(&handler, &before.version, vec![]));
    assert_eq!(done.snapshot, before);
    assert_eq!(
        done.previous, None,
        "nothing was written, so nothing to undo"
    );
    assert_eq!(bytes(&handler), content());

    // A move onto the entry's own place is an identity edit too.
    let done = applied(edit(
        &handler,
        &before.version,
        vec![EditOp::Move { from: 1, to: 1 }],
    ));
    assert_eq!(done.snapshot, before);
    assert_eq!(
        done.previous, None,
        "nothing was written, so nothing to undo"
    );
    assert_eq!(bytes(&handler), content());
}

pub fn keep_adds_a_duplicate_again(setup: Setup) {
    let handler = setup(&content());
    let version = snapshot(&handler).version;
    let done = applied(edit(
        &handler,
        &version,
        vec![insert(1, &track_a(), Duplicates::Keep)],
    ));
    assert!(done.skipped.is_empty());
    assert_eq!(
        done.snapshot.entries,
        vec![
            track(&track_a()),
            track(&track_a()),
            track(&track_b1()),
            unknown_entry()
        ]
    );
}

pub fn revert_restores_the_exact_bytes_and_its_own_previous_redoes_the_edit(setup: Setup) {
    let handler = setup(&content());
    let v0 = snapshot(&handler).version;
    let edited = applied(edit(
        &handler,
        &v0,
        vec![insert(1, &track_b2(), Duplicates::Skip)],
    ));
    let edited_bytes = bytes(&handler);

    let undone = applied(revert(
        &handler,
        &edited.snapshot.version,
        edited.undo_point(),
    ));
    assert_eq!(bytes(&handler), content());
    assert_eq!(undone.snapshot.version, v0);
    assert!(undone.skipped.is_empty());
    assert_eq!(undone.undo_point().as_str(), edited_bytes);

    let redone = applied(revert(
        &handler,
        &undone.snapshot.version,
        undone.undo_point(),
    ));
    assert_eq!(bytes(&handler), edited_bytes);
    assert_eq!(redone.snapshot.version, edited.snapshot.version);
}

pub fn stale_revert_conflicts_and_writes_nothing(setup: Setup) {
    let handler = setup(&content());
    let current = snapshot(&handler);
    let response = revert(&handler, &stale(), &StoredPlaylist::new("other\n"));
    assert_eq!(conflict(response), current);
    assert_eq!(bytes(&handler), content());
}

pub fn versioned_delete_deletes(setup: Setup) {
    let handler = setup(&content());
    let version = snapshot(&handler).version;
    let response = handler(LibraryRequest::PlaylistDelete {
        name: set(),
        version,
    });
    assert!(
        matches!(response, LibraryResponse::PlaylistDeleted),
        "{response:?}"
    );
    assert_not_found(handler(LibraryRequest::PlaylistGet { name: set() }));
}

pub fn stale_delete_conflicts_and_keeps_the_file(setup: Setup) {
    let handler = setup(&content());
    let current = snapshot(&handler);
    let response = handler(LibraryRequest::PlaylistDelete {
        name: set(),
        version: stale(),
    });
    match response {
        LibraryResponse::PlaylistDeleteConflict { current: got } => assert_eq!(got, current),
        other => panic!("expected PlaylistDeleteConflict, got {other:?}"),
    }
    assert_eq!(bytes(&handler), content());
}

pub fn rename_is_refused_onto_an_existing_playlist(setup: Setup) {
    let handler = setup(&content());
    let other = PlaylistName::new("other").expect("valid name");
    handler(LibraryRequest::PlaylistNew {
        name: other.clone(),
        content: "x\n".to_string(),
    });

    let response = handler(LibraryRequest::PlaylistRename {
        from: set(),
        to: other.clone(),
    });
    assert!(
        matches!(
            response,
            LibraryResponse::Error(ProtocolError::PlaylistAlreadyExists { .. })
        ),
        "{response:?}"
    );
    assert_eq!(bytes(&handler), content());
    assert_eq!(get(&handler, &other), "x\n");

    let renamed = PlaylistName::new("renamed").expect("valid name");
    let response = handler(LibraryRequest::PlaylistRename {
        from: set(),
        to: renamed.clone(),
    });
    assert!(matches!(response, LibraryResponse::Pong), "{response:?}");
    assert_eq!(get(&handler, &renamed), content());
    assert_not_found(handler(LibraryRequest::PlaylistGet { name: set() }));
}

pub fn remove_deletes_unconditionally(setup: Setup) {
    let handler = setup(&content());
    let response = handler(LibraryRequest::PlaylistRemove { name: set() });
    assert!(matches!(response, LibraryResponse::Pong), "{response:?}");
    assert_not_found(handler(LibraryRequest::PlaylistRemove { name: set() }));
}

pub fn a_refused_insert_refuses_the_whole_edit_and_writes_nothing(setup: Setup) {
    let handler = setup(&content());
    let version = snapshot(&handler).version;

    let unknown = full_hash("ffff", 'f');
    let response = edit(
        &handler,
        &version,
        vec![
            insert(0, &track_b2(), Duplicates::Skip),
            insert(0, &unknown, Duplicates::Skip),
        ],
    );
    assert!(
        matches!(
            response,
            LibraryResponse::Error(ProtocolError::TrackNotFound { .. })
        ),
        "{response:?}"
    );

    let response = edit(&handler, &version, vec![insert(0, "ab", Duplicates::Skip)]);
    assert!(
        matches!(
            response,
            LibraryResponse::Error(ProtocolError::AmbiguousHash { .. })
        ),
        "{response:?}"
    );

    let response = edit(&handler, &version, vec![EditOp::Remove { at: 9 }]);
    assert!(
        matches!(
            response,
            LibraryResponse::Error(ProtocolError::InvalidEdit { op: 0, .. })
        ),
        "{response:?}"
    );

    assert_eq!(bytes(&handler), content());
}

/// CRLF text of entries A, B1 and an unknown track, after a comment.
fn crlf_content() -> String {
    format!(
        "# set\r\naaaaaaaa  Artist - A\r\n{}\r\nffffffff  # unknown\r\n",
        track_b1()
    )
}

pub fn a_crlf_playlist_keeps_its_bytes_through_read_and_edit(setup: Setup) {
    let handler = setup(&crlf_content());
    assert_eq!(bytes(&handler), crlf_content());
    let before = snapshot(&handler);
    assert_eq!(
        before.version,
        PlaylistVersion::of(crlf_content().as_bytes())
    );
    assert_eq!(
        before.entries,
        vec![track(&track_a()), track(&track_b1()), unknown_entry()]
    );

    let done = applied(edit(
        &handler,
        &before.version,
        vec![EditOp::Move { from: 0, to: 1 }],
    ));
    assert_eq!(
        bytes(&handler),
        format!(
            "# set\r\n{}\r\naaaaaaaa  Artist - A\r\nffffffff  # unknown\r\n",
            track_b1()
        )
    );
    assert_eq!(done.undo_point().as_str(), crlf_content());
}

pub fn append_joins_on_a_line_break(setup: Setup) {
    let append = |handler: &Handler, added: &str| {
        handler(LibraryRequest::PlaylistAppend {
            name: set(),
            content: added.to_string(),
        })
    };

    let handler = setup("first");
    let response = append(&handler, "second\n");
    assert!(
        matches!(&response, LibraryResponse::PlaylistContent(c) if c == "first\nsecond\n"),
        "{response:?}"
    );
    assert_eq!(bytes(&handler), "first\nsecond\n");

    let handler = setup("first\n");
    append(&handler, "second\n");
    assert_eq!(bytes(&handler), "first\nsecond\n");
}

pub fn a_missing_playlist_is_not_found_for_every_operation(setup: Setup) {
    let handler = setup(&content());
    let name = PlaylistName::new("nope").expect("valid name");
    let version = stale();
    let requests = [
        LibraryRequest::PlaylistSnapshot { name: name.clone() },
        LibraryRequest::PlaylistEdit {
            name: name.clone(),
            version: version.clone(),
            ops: vec![],
        },
        LibraryRequest::PlaylistRevert {
            name: name.clone(),
            version: version.clone(),
            content: StoredPlaylist::new(""),
        },
        LibraryRequest::PlaylistDelete {
            name: name.clone(),
            version,
        },
        LibraryRequest::PlaylistRemove { name: name.clone() },
        LibraryRequest::PlaylistAppend {
            name: name.clone(),
            content: "x\n".to_string(),
        },
        LibraryRequest::PlaylistRename {
            from: name.clone(),
            to: PlaylistName::new("elsewhere").expect("valid name"),
        },
    ];
    for request in requests {
        assert_not_found(handler(request));
    }
}
