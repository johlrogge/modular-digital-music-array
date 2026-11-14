//! Origin kinds the CLI owns. With the user's tool (`mdma`) each is a source of its
//! own, so what an import or an upload wrote stays distinguishable from manual edits
//! and can be retracted on its own (`mdma fact retract --origin <KIND>`).

use library_ipc_client::USER_TOOL;
use music_facts::FactSource;

/// Origin kind of facts imported from Rekordbox.
pub(crate) const REKORDBOX_ORIGIN: &str = "Rekordbox";

/// Origin kind of the tags of files uploaded with `mdma transfer`.
pub(crate) const UPLOAD_ORIGIN: &str = "Upload";

/// The source of facts a Rekordbox import writes.
pub(crate) fn rekordbox_source() -> FactSource {
    FactSource::new(USER_TOOL, env!("CARGO_PKG_VERSION"), REKORDBOX_ORIGIN)
}

/// The source of the tags and facts of an uploaded file.
pub(crate) fn upload_source() -> FactSource {
    FactSource::new(USER_TOOL, env!("CARGO_PKG_VERSION"), UPLOAD_ORIGIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_ipc_client::user_fact_source;

    #[test]
    fn upload_and_rekordbox_are_their_own_identities_under_the_user_tool() {
        let user = user_fact_source("0");
        for (source, origin) in [
            (upload_source(), "Upload"),
            (rekordbox_source(), "Rekordbox"),
        ] {
            assert_eq!(source.tool, "mdma");
            assert_eq!(source.origin.kind, origin);
            assert!(!source.same_source(&user));
        }
        assert!(!upload_source().same_source(&rekordbox_source()));
    }
}
