//! Argument helpers shared by several command modules.

use playlist_import::sanitize_playlist_name;

pub(crate) fn parse_playlist_name(name: &str) -> library_ipc_client::PlaylistName {
    library_ipc_client::PlaylistName::new(name).unwrap_or_else(|e| {
        eprintln!("Invalid playlist name: {}", e);
        std::process::exit(1);
    })
}

/// Sanitise a user-supplied playlist name for `flag`, exiting if nothing is left.
/// Prints a note when the name was changed.
pub(crate) fn sanitized_name_or_exit(raw: &str, flag: &str) -> String {
    let sanitized = sanitize_playlist_name(raw);
    if sanitized.is_empty() {
        eprintln!("Invalid {} '{}': name sanitizes to empty", flag, raw);
        std::process::exit(1);
    }
    if sanitized != raw {
        eprintln!(
            "Note: playlist name sanitized from '{}' to '{}'",
            raw, sanitized
        );
    }
    sanitized
}
