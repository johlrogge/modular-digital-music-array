//! Test helpers shared by several modules' tests.

use library_ipc_client::{ContentHash, TrackInfo};

/// Strip ANSI escape codes from a string.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(next) = chars.next() {
                if next == '[' {
                    for seq_char in chars.by_ref() {
                        if seq_char.is_ascii_alphabetic() || seq_char == '~' {
                            break;
                        }
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A track with only its content hash set: everything optional is `None`,
/// collections are empty. Build variations with `TrackInfo { .., ..track(hash) }`.
pub(crate) fn track(hash: &str) -> TrackInfo {
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
