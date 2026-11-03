//! Playlist names derived from user input or a source playlist.

/// Sanitise a free-form name into a playlist name: every character outside
/// `[a-zA-Z0-9_-]` becomes `-`, then leading/trailing `-` are trimmed.
/// The result may be empty; callers decide how to handle that.
pub fn sanitize_playlist_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Last non-empty path segment of a URL, or the input itself when it is a slug.
///
/// Deliberately duplicates `bandcamp_api::slug_from_url`: this component is
/// source-agnostic and must not depend on a source-specific crate.
pub fn slug_from_playlist_arg(arg: &str) -> &str {
    arg.split(['?', '#'])
        .next()
        .unwrap_or(arg)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(arg)
}

/// Default MDMA playlist name: sanitised title, else sanitised slug.
pub fn default_import_name(title: &str, slug_or_url: &str) -> String {
    let from_title = sanitize_playlist_name(title);
    if from_title.is_empty() {
        sanitize_playlist_name(slug_from_playlist_arg(slug_or_url))
    } else {
        from_title
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn sanitize_playlist_name_replaces_and_trims() {
        assert_eq!(sanitize_playlist_name("Friday Set #1!"), "Friday-Set--1");
        assert_eq!(sanitize_playlist_name("ok_name-2"), "ok_name-2");
        assert_eq!(sanitize_playlist_name("  --x--  "), "x");
    }

    #[test]
    fn sanitize_playlist_name_can_be_empty() {
        assert_eq!(sanitize_playlist_name("!!! ???"), "");
        assert_eq!(sanitize_playlist_name("åäö"), "");
    }

    #[test]
    fn default_import_name_prefers_title_then_slug() {
        assert_eq!(default_import_name("Friday Set", "friday"), "Friday-Set");
        assert_eq!(default_import_name("!!!", "my-slug"), "my-slug");
        assert_eq!(
            default_import_name("???", "https://bandcamp.com/u/x/playlists/cool-set/?a=1"),
            "cool-set"
        );
    }
}
