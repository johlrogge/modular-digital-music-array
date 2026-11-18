//! Three-way merge of a single value (ADR-034), local wins.
//!
//! The scalar sibling of [`crate::merge3`]: `base` is the last value exchanged,
//! `theirs` the other side now, `ours` the local value now.

/// What to do with the local value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision<T> {
    /// Theirs did not change: keep ours.
    Keep,
    /// Theirs changed and ours did not: take theirs.
    Take(T),
    /// Both changed to the same value: nothing to write.
    Converged,
    /// Both changed, differently. Local wins; the caller reports it.
    Conflict,
}

/// Merge one value three ways. Pure local-wins: any caller-specific rule, such
/// as letting a local clear give way, stays with the caller.
pub fn merge3_scalar<T: Eq>(base: T, theirs: T, ours: T) -> Decision<T> {
    if theirs == base {
        Decision::Keep
    } else if ours == base {
        Decision::Take(theirs)
    } else if ours == theirs {
        Decision::Converged
    } else {
        Decision::Conflict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn theirs_unchanged_keeps_ours() {
        assert_eq!(merge3_scalar(1, 1, 1), Decision::Keep);
        assert_eq!(merge3_scalar(1, 1, 5), Decision::Keep);
    }

    #[test]
    fn ours_unchanged_takes_theirs() {
        assert_eq!(merge3_scalar(1, 3, 1), Decision::Take(3));
    }

    #[test]
    fn both_changed_to_the_same_converges() {
        assert_eq!(merge3_scalar(1, 3, 3), Decision::Converged);
    }

    #[test]
    fn both_changed_differently_conflicts() {
        assert_eq!(merge3_scalar(1, 3, 4), Decision::Conflict);
    }

    proptest! {
        #[test]
        fn theirs_equal_to_base_keeps(b in 0u8..6, o in 0u8..6) {
            prop_assert_eq!(merge3_scalar(b, b, o), Decision::Keep);
        }

        #[test]
        fn ours_equal_to_base_takes_theirs(b in 0u8..6, t in 0u8..6) {
            prop_assume!(t != b);
            prop_assert_eq!(merge3_scalar(b, t, b), Decision::Take(t));
        }

        #[test]
        fn the_outcome_is_always_ours_or_theirs(b in 0u8..6, t in 0u8..6, o in 0u8..6) {
            let result = match merge3_scalar(b, t, o) {
                Decision::Keep | Decision::Conflict | Decision::Converged => o,
                Decision::Take(v) => v,
            };
            prop_assert!(result == o || result == t);
        }

        /// Running again with base advanced to theirs never changes anything.
        #[test]
        fn merging_twice_with_base_advanced_keeps(b in 0u8..6, t in 0u8..6, o in 0u8..6) {
            let after = match merge3_scalar(b, t, o) {
                Decision::Take(v) => v,
                _ => o,
            };
            prop_assert_eq!(merge3_scalar(t, t, after), Decision::Keep);
        }
    }
}
