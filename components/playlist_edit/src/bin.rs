//! The playlist bin (ADR-033): the rules the library service and its stub share.
//!
//! No IO. The service moves files and the stub moves strings; both take their
//! ids, counts and order from here.

use crate::PlaylistDoc;
use library_ipc_protocol::{BinEntry, BinId, BinIdError, PlaylistName, ProtocolError};
use std::time::{Duration, SystemTime};

/// The id for binning `name` at `now`. When `taken` says the id is in use (the
/// same name binned within the same millisecond), the time moves on one
/// millisecond at a time until it is free, so binning never replaces an entry.
pub fn fresh_bin_id(
    name: &PlaylistName,
    now: SystemTime,
    taken: impl Fn(&BinId) -> bool,
) -> Result<BinId, BinIdError> {
    let mut at = now;
    loop {
        let id = BinId::new(name.clone(), at)?;
        if !taken(&id) {
            return Ok(id);
        }
        at += Duration::from_millis(1);
    }
}

/// The `BinList` entry of a binned playlist with these stored bytes. The
/// count is the one `PlaylistDoc` gives, as everywhere else.
pub fn bin_entry(id: BinId, content: &str) -> BinEntry {
    BinEntry {
        id,
        entry_count: PlaylistDoc::parse(content).entry_count(),
    }
}

/// A bin id that cannot be made (a time out of range) as the error answered.
pub fn bin_id_error(error: BinIdError) -> ProtocolError {
    ProtocolError::Internal {
        message: format!("Cannot name the bin entry: {error}"),
    }
}

/// The name a restore gives the playlist: `as_name`, else the name it had.
pub fn restore_target(id: &BinId, as_name: Option<PlaylistName>) -> PlaylistName {
    as_name.unwrap_or_else(|| id.name().clone())
}

/// Newest first; entries binned in the same millisecond come by name (two
/// entries of one name never share a millisecond), so the order is the same
/// on every run.
pub fn sort_newest_first(entries: &mut [BinEntry]) {
    entries.sort_by(|a, b| {
        b.id.binned_at_ms()
            .cmp(&a.id.binned_at_ms())
            .then_with(|| a.id.name().as_str().cmp(b.id.name().as_str()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::UNIX_EPOCH;

    fn name() -> PlaylistName {
        PlaylistName::new("set").unwrap()
    }

    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn a_free_id_is_used_as_it_is() {
        let id = fresh_bin_id(&name(), at(42), |_| false).unwrap();
        assert_eq!(id.to_string(), "set.42");
    }

    #[test]
    fn a_taken_id_moves_on_to_the_next_free_millisecond() {
        let asked = Cell::new(0);
        let id = fresh_bin_id(&name(), at(42), |_| {
            asked.set(asked.get() + 1);
            asked.get() <= 2
        })
        .unwrap();
        assert_eq!(id.to_string(), "set.44");
    }

    #[test]
    fn the_count_is_the_number_of_entry_lines() {
        let entry = bin_entry(
            fresh_bin_id(&name(), at(1), |_| false).unwrap(),
            "# c\r\naaaa  x\r\n\r\nbbbb\r\n",
        );
        assert_eq!(entry.entry_count, 2);
    }

    #[test]
    fn a_restore_goes_to_as_or_else_to_the_old_name() {
        let id: BinId = "set.7".parse().unwrap();
        let other = PlaylistName::new("other").unwrap();
        assert_eq!(restore_target(&id, None), name());
        assert_eq!(restore_target(&id, Some(other.clone())), other);
    }

    #[test]
    fn newest_comes_first_and_ties_are_ordered_by_id() {
        let entry = |n: &str, ms| BinEntry {
            id: format!("{n}.{ms}").parse().unwrap(),
            entry_count: 0,
        };
        let mut entries = vec![entry("b", 5), entry("a", 9), entry("a", 5)];
        sort_newest_first(&mut entries);
        let ids: Vec<String> = entries.iter().map(|e| e.id.to_string()).collect();
        assert_eq!(ids, vec!["a.9", "a.5", "b.5"]);
    }
}
