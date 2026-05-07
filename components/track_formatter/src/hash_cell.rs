//! Hash cell types for track formatting.
//!
//! Two types embody two policies:
//!
//! - [`NonShrinkableHash`] — playlist serialization. The type-level promise is that the hash
//!   must remain unique across time and never be truncated by corsett. Its algorithm is
//!   [`FreeText`] (no ellipsis). The "never shrinks" guarantee is *structural*: playlist
//!   serializers (`format_track_line`, `format_playlist_content`) do **not** call
//!   `corsett::resize_columns` — they format directly. So the hash is always the full
//!   12-char prefix regardless of terminal width. The type carries the intent; the mechanism
//!   is "playlist serializer bypasses corsett."
//!
//! - [`ShrinkableHash`] — user-facing display (search, view). Uses
//!   [`RightEllipsis`] so that under extreme width pressure the hash can be
//!   ellipsis-truncated rather than causing the row to overflow. In practice, at typical
//!   terminal widths (≥80 chars) the hash is always the full 12-char prefix. The removal
//!   policy is [`RemovalPolicy::Never`] for both types: the hash column is never dropped,
//!   because uniqueness identification must always be present.

use corsett::{
    shortener::{FreeText, RightEllipsis},
    RemovalPolicy, Shorten,
};
use library_ipc_protocol::ContentHash;

use crate::short_hash;

// =============================================================================
// NonShrinkableHash — playlist-safe hash cell
// =============================================================================

/// Hash cell for playlist serialization.
///
/// Type-level promise: this hash must remain unique across time and must never be
/// truncated. The guarantee is structural — playlist formatters do not call
/// `corsett::resize_columns`, so the 12-char value flows through `format!()` unchanged.
///
/// Algorithm: [`FreeText`] (no ellipsis decoration).
pub struct NonShrinkableHash(String);

impl AsRef<str> for NonShrinkableHash {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Shorten for NonShrinkableHash {
    type Algorithm = FreeText;
}

impl From<&ContentHash> for NonShrinkableHash {
    fn from(hash: &ContentHash) -> Self {
        NonShrinkableHash(short_hash(hash).to_string())
    }
}

// =============================================================================
// ShrinkableHash — user-facing display hash cell
// =============================================================================

/// Hash cell for user-facing display (search, view).
///
/// Ellipsis-truncatable under extreme width pressure — but in practice at typical
/// terminal widths the hash is always the full 12-char prefix. The hash column is
/// never *removed* from the layout (uniqueness identification must always be visible).
///
/// Algorithm: [`RightEllipsis`] — adds `…` when shortened.
pub struct ShrinkableHash(String);

impl AsRef<str> for ShrinkableHash {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Shorten for ShrinkableHash {
    type Algorithm = RightEllipsis<'…', FreeText>;
}

impl From<&ContentHash> for ShrinkableHash {
    fn from(hash: &ContentHash) -> Self {
        ShrinkableHash(short_hash(hash).to_string())
    }
}

// =============================================================================
// HashCell trait — unifies the two hash types for the generic renderer
// =============================================================================

/// Marker trait that unifies [`NonShrinkableHash`] and [`ShrinkableHash`] for the
/// generic renderer. Carries the corsett [`RemovalPolicy`] at the type level so
/// the renderer can construct the correct policy array without knowing which
/// concrete hash type it is operating on.
///
/// Both types use `RemovalPolicy::Never` — the hash column is always present.
pub trait HashCell: Shorten + for<'a> From<&'a ContentHash> + 'static {
    /// Corsett column removal policy for the hash cell.
    ///
    /// Both concrete implementations return [`RemovalPolicy::Never`]: the hash is
    /// the identity column and must always be present in the output.
    const REMOVAL_POLICY: RemovalPolicy;
}

impl HashCell for NonShrinkableHash {
    const REMOVAL_POLICY: RemovalPolicy = RemovalPolicy::Never;
}

impl HashCell for ShrinkableHash {
    const REMOVAL_POLICY: RemovalPolicy = RemovalPolicy::Never;
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use super::*;

    // ── Algorithm identity assertions ────────────────────────────────────────

    /// Compile-time (via TypeId) assertion that NonShrinkableHash uses FreeText.
    #[test]
    fn non_shrinkable_hash_algorithm_is_free_text() {
        assert_eq!(
            TypeId::of::<<NonShrinkableHash as Shorten>::Algorithm>(),
            TypeId::of::<FreeText>(),
            "NonShrinkableHash must use FreeText — no ellipsis decoration for playlist hashes"
        );
    }

    /// Compile-time (via TypeId) assertion that ShrinkableHash uses RightEllipsis<'…', FreeText>.
    #[test]
    fn shrinkable_hash_algorithm_is_right_ellipsis() {
        assert_eq!(
            TypeId::of::<<ShrinkableHash as Shorten>::Algorithm>(),
            TypeId::of::<RightEllipsis<'…', FreeText>>(),
            "ShrinkableHash must use RightEllipsis<'…', FreeText>"
        );
    }

    /// The two algorithms must differ.
    #[test]
    fn algorithm_types_differ() {
        assert_ne!(
            TypeId::of::<<NonShrinkableHash as Shorten>::Algorithm>(),
            TypeId::of::<<ShrinkableHash as Shorten>::Algorithm>(),
            "NonShrinkableHash and ShrinkableHash must use different algorithms"
        );
    }

    // ── RemovalPolicy assertions ─────────────────────────────────────────────

    #[test]
    fn non_shrinkable_hash_removal_policy_is_never() {
        assert!(matches!(
            NonShrinkableHash::REMOVAL_POLICY,
            RemovalPolicy::Never
        ));
    }

    #[test]
    fn shrinkable_hash_removal_policy_is_never() {
        assert!(matches!(
            ShrinkableHash::REMOVAL_POLICY,
            RemovalPolicy::Never
        ));
    }

    // ── From<&ContentHash> assertions ────────────────────────────────────────

    #[test]
    fn non_shrinkable_hash_from_content_hash_strips_prefix_and_truncates() {
        let hash = ContentHash::new("sha256:abcdef1234567890");
        let cell = NonShrinkableHash::from(&hash);
        assert_eq!(cell.as_ref(), "abcdef123456");
    }

    #[test]
    fn shrinkable_hash_from_content_hash_strips_prefix_and_truncates() {
        let hash = ContentHash::new("sha256:abcdef1234567890");
        let cell = ShrinkableHash::from(&hash);
        assert_eq!(cell.as_ref(), "abcdef123456");
    }

    #[test]
    fn both_hash_types_produce_same_12_char_value() {
        let hash = ContentHash::new("sha256:deadbeef12345678");
        let non_shrinkable = NonShrinkableHash::from(&hash);
        let shrinkable = ShrinkableHash::from(&hash);
        assert_eq!(
            non_shrinkable.as_ref(),
            shrinkable.as_ref(),
            "both hash types must produce the same underlying value from the same ContentHash"
        );
        assert_eq!(non_shrinkable.as_ref().len(), 12);
    }
}
