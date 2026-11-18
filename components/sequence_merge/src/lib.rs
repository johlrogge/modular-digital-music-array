//! Generic three-way merge of sequences (ADR-032).
//!
//! `merge3(base, theirs, ours)` applies the changes made between `base` and
//! `theirs` onto `ours`. Local changes win. The crate knows nothing about
//! Bandcamp or playlists: it merges sequences of any hashable key.
//!
//! # Identity
//!
//! Duplicates are told apart by occurrence: internally each element is keyed
//! as `(key, n)`, where `n` is the number of equal keys before it in the same
//! sequence. The keying is an internal detail (the API stays
//! `&[K] -> Vec<K>`), so callers never see or build occurrence keys. A local
//! copy of an item gets a higher `n` and is therefore local-only, and a
//! removal in `theirs` takes away only the matching occurrence.
//!
//! Because the result is a plain `Vec<K>`, occurrence numbers are recomputed
//! from the result's own order.
//!
//! # Rules
//!
//! The result starts from `ours`.
//!
//! 1. Removed in theirs (in base, not in theirs): removed from ours if that
//!    occurrence is still present.
//! 2. Added in theirs (not in base): added unless ours already has it. It is
//!    placed after its nearest predecessor in theirs that is present in the
//!    result. With no such predecessor it goes first if it was first in
//!    theirs, otherwise last.
//! 3. A local deletion (in base, not in ours) stays deleted. This follows
//!    from rule 2 only adding items that are not in base, and from moves
//!    only repositioning items that ours still has.
//! 4. Moved in theirs. The shared items are those in base, theirs and ours.
//!    The longest common subsequence (LCS) of base and theirs among them is
//!    what Bandcamp kept in order; the other shared items were moved in
//!    theirs. The LCS of base and ours among the items Bandcamp kept is what
//!    both sides kept in order. A moved item counts as moved locally when
//!    the number of those items before it differs between base and ours. A
//!    move is applied unless the item was moved locally. Applying it
//!    repositions the item after its nearest predecessor in theirs that is
//!    present in the result, or first if it is first in theirs. If it has no
//!    such predecessor (its anchor is gone) it stays where it is in ours:
//!    unlike an add, a move never falls back to "last", because local state
//!    wins. LCS ties are broken deterministically (see `lcs`).
//!
//! Items are processed in theirs order, and the guarantees below depend on
//! it: each placement anchors on items that were already placed.
//!
//! # Limitations
//!
//! - Reordering only among tracks Bandcamp also moved is not detected, so
//!   Bandcamp's order wins there. With base `abcd`, theirs `cdab` and ours
//!   `bacd` the result is `cdab`.
//! - A local move counts only against items still shared by all three.
//!   Moving an item past one that Bandcamp has since removed does not count
//!   as a move.
//! - Placement follows theirs' neighbours, not ours'. With base `abc`,
//!   theirs `bca` and ours `acb` the result is `cab`, not a minimal edit of
//!   ours.
//! - With duplicates only the multiset bounds hold (a result item count is
//!   at most the larger of ours' and theirs'); idempotence is not claimed.
//!   Occurrences are renumbered from the result's order, so
//!   `merge3("aa", "aaa", "a")` gives `aa` and merging that again gives
//!   `aaa`. For unique keys idempotence holds.

pub mod scalar;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// The `nth` occurrence of `key` in a sequence: how duplicates are told apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Occurrence<K> {
    pub key: K,
    pub nth: usize,
}

/// Each item of `items` as an [`Occurrence`]: `nth` counts the equal keys
/// before it in `items`.
pub fn occurrences<K: Eq + Hash + Clone>(items: &[K]) -> Vec<Occurrence<K>> {
    let mut seen: HashMap<&K, usize> = HashMap::new();
    items
        .iter()
        .map(|key| {
            let nth = seen.entry(key).or_insert(0);
            let occurrence = Occurrence {
                key: key.clone(),
                nth: *nth,
            };
            *nth += 1;
            occurrence
        })
        .collect()
}

/// Longest common subsequence of `a` and `b`.
///
/// Deterministic: with equal heads the match is taken; otherwise the item of
/// `a` is skipped if that loses nothing (`>=`), else the item of `b`.
pub fn lcs<T: PartialEq + Copy>(a: &[T], b: &[T]) -> Vec<T> {
    // table[i][j] = length of the LCS of a[i..] and b[j..].
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(a[i]);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// For each item of `moved`, the number of `kept` items before it in `seq`.
fn slots<'a, K: Eq + Hash>(
    seq: &'a [Occurrence<K>],
    kept: &HashSet<&Occurrence<K>>,
    moved: &HashSet<&Occurrence<K>>,
) -> HashMap<&'a Occurrence<K>, usize> {
    let mut before = 0;
    let mut out = HashMap::new();
    for o in seq {
        if kept.contains(o) {
            before += 1;
        }
        if moved.contains(o) {
            out.insert(o, before);
        }
    }
    out
}

/// The items Bandcamp moved that were not moved locally.
///
/// Shared items are those in base, theirs and ours. The longest common
/// subsequence of base and theirs among them is what Bandcamp kept in order;
/// the other shared items were moved by Bandcamp. A moved item counts as
/// moved locally when it sits in a different slot between the items that both
/// base and ours kept in order (the LCS of base and ours among the items
/// Bandcamp kept).
fn moves_to_apply<K: Eq + Hash + Clone>(
    base: &[Occurrence<K>],
    theirs: &[Occurrence<K>],
    ours: &[Occurrence<K>],
) -> HashSet<Occurrence<K>> {
    let base_set: HashSet<&Occurrence<K>> = base.iter().collect();
    let theirs_set: HashSet<&Occurrence<K>> = theirs.iter().collect();
    let ours_set: HashSet<&Occurrence<K>> = ours.iter().collect();
    let shared = |o: &&Occurrence<K>| {
        base_set.contains(*o) && theirs_set.contains(*o) && ours_set.contains(*o)
    };

    let base_shared: Vec<&Occurrence<K>> = base.iter().filter(shared).collect();
    let theirs_shared: Vec<&Occurrence<K>> = theirs.iter().filter(shared).collect();
    let kept_by_theirs: HashSet<&Occurrence<K>> =
        lcs(&base_shared, &theirs_shared).into_iter().collect();
    let moved_by_theirs: HashSet<&Occurrence<K>> = base_shared
        .iter()
        .copied()
        .filter(|o| !kept_by_theirs.contains(o))
        .collect();

    let base_kept: Vec<&Occurrence<K>> =
        base.iter().filter(|o| kept_by_theirs.contains(o)).collect();
    let ours_kept: Vec<&Occurrence<K>> =
        ours.iter().filter(|o| kept_by_theirs.contains(o)).collect();
    let in_order: HashSet<&Occurrence<K>> = lcs(&base_kept, &ours_kept).into_iter().collect();

    let base_slots = slots(base, &in_order, &moved_by_theirs);
    let ours_slots = slots(ours, &in_order, &moved_by_theirs);
    moved_by_theirs
        .iter()
        .filter(|m| base_slots.get(*m) == ours_slots.get(*m))
        .map(|m| (*m).clone())
        .collect()
}

/// Nearest item before `idx` that satisfies `keep`.
fn predecessor<K>(
    items: &[Occurrence<K>],
    idx: usize,
    keep: impl Fn(&Occurrence<K>) -> bool,
) -> Option<&Occurrence<K>> {
    items[..idx].iter().rev().find(|o| keep(o))
}

/// Where an item from theirs goes in the result.
enum Anchor {
    /// First in theirs: goes first.
    Start,
    /// Insert at this index, i.e. right after the nearest present predecessor.
    After(usize),
}

/// The anchor for `theirs[i]`, or `None` when it is not first in theirs and
/// no predecessor in theirs is present in the result.
fn anchor<K: Eq + Hash>(
    theirs: &[Occurrence<K>],
    i: usize,
    result: &[Occurrence<K>],
    present: &HashSet<Occurrence<K>>,
) -> Option<Anchor> {
    if i == 0 {
        return Some(Anchor::Start);
    }
    let pred = predecessor(theirs, i, |o| present.contains(o))?;
    let pos = result.iter().position(|r| r == pred)?;
    Some(Anchor::After(pos + 1))
}

/// Three-way merge of `theirs`' changes (relative to `base`) onto `ours`.
pub fn merge3<K: Eq + Hash + Clone>(base: &[K], theirs: &[K], ours: &[K]) -> Vec<K> {
    let base = occurrences(base);
    let theirs = occurrences(theirs);
    let ours = occurrences(ours);

    let base_set: HashSet<&Occurrence<K>> = base.iter().collect();
    let theirs_set: HashSet<&Occurrence<K>> = theirs.iter().collect();
    let apply = moves_to_apply(&base, &theirs, &ours);

    // Rule 1: removals.
    let mut result: Vec<Occurrence<K>> = ours
        .iter()
        .filter(|o| !base_set.contains(o) || theirs_set.contains(o))
        .cloned()
        .collect();
    let mut present: HashSet<Occurrence<K>> = result.iter().cloned().collect();

    for (i, item) in theirs.iter().enumerate() {
        // `old` is where a move starts; `None` for an add.
        let old = if !base_set.contains(item) {
            // Rule 2: added in theirs.
            if present.contains(item) {
                continue;
            }
            None
        } else if apply.contains(item) {
            // Rule 4: moved in theirs, not moved locally.
            result.iter().position(|o| o == item)
        } else {
            continue;
        };
        if let Some(at) = old {
            result.remove(at);
            present.remove(item);
        }
        let at = match anchor(&theirs, i, &result, &present) {
            Some(Anchor::Start) => 0,
            Some(Anchor::After(at)) => at,
            // No anchor: an add goes last, a move stays put.
            None => old.unwrap_or(result.len()),
        };
        result.insert(at, item.clone());
        present.insert(item.clone());
    }

    result.into_iter().map(|o| o.key).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn m(base: &str, theirs: &str, ours: &str) -> String {
        let v = |s: &str| s.chars().collect::<Vec<_>>();
        merge3(&v(base), &v(theirs), &v(ours)).into_iter().collect()
    }

    #[test]
    fn an_item_added_in_theirs_goes_after_its_nearest_predecessor() {
        assert_eq!(m("ac", "abc", "ac"), "abc");
    }

    #[test]
    fn an_item_added_in_theirs_skips_predecessors_that_are_absent_from_ours() {
        // c follows b in theirs, but b was deleted locally: c goes after a.
        assert_eq!(m("abd", "abcd", "ad"), "acd");
    }

    #[test]
    fn an_item_first_in_theirs_with_no_predecessor_goes_first() {
        assert_eq!(m("a", "ba", "a"), "ba");
    }

    #[test]
    fn an_item_not_first_in_theirs_without_a_present_predecessor_goes_last() {
        // a was deleted locally, so c has no predecessor present: end.
        assert_eq!(m("ab", "acb", "b"), "bc");
    }

    #[test]
    fn an_item_added_in_theirs_that_ours_already_has_is_not_duplicated() {
        assert_eq!(m("a", "ab", "ba"), "ba");
    }

    #[test]
    fn a_local_deletion_stays_deleted_when_theirs_adds_something_else() {
        assert_eq!(m("ab", "abc", "b"), "bc");
    }

    #[test]
    fn an_item_removed_in_theirs_is_removed_from_ours() {
        assert_eq!(m("abc", "ac", "abc"), "ac");
    }

    #[test]
    fn an_item_removed_in_theirs_and_already_gone_locally_stays_gone() {
        assert_eq!(m("abc", "ac", "ac"), "ac");
    }

    #[test]
    fn a_removal_in_theirs_takes_away_only_the_matching_occurrence() {
        assert_eq!(m("aa", "a", "aab"), "ab");
    }

    #[test]
    fn a_local_copy_of_an_item_survives_when_theirs_is_unchanged() {
        assert_eq!(m("a", "a", "aa"), "aa");
    }

    #[test]
    fn a_move_in_theirs_is_applied_when_the_item_is_unmoved_locally() {
        assert_eq!(m("abc", "cab", "abc"), "cab");
    }

    #[test]
    fn a_move_in_theirs_is_skipped_when_the_item_was_moved_locally() {
        // c was moved before b locally.
        assert_eq!(m("abc", "cab", "acb"), "acb");
    }

    #[test]
    fn a_local_reordering_of_other_items_does_not_block_a_move() {
        // c is still last locally; only a and b were swapped.
        assert_eq!(m("abc", "cab", "bac"), "cba");
    }

    #[test]
    fn a_move_in_theirs_ignores_local_items_between_neighbours() {
        // x is local-only, so it does not count as moving c.
        assert_eq!(m("abc", "cab", "xabc"), "cxab");
    }

    #[test]
    fn a_move_in_theirs_is_applied_among_local_only_items() {
        assert_eq!(m("abc", "cab", "xaybzc"), "cxaybz");
    }

    #[test]
    fn a_reversal_in_theirs_is_applied_when_ours_is_unchanged() {
        assert_eq!(m("abcd", "dcba", "abcd"), "dcba");
    }

    #[test]
    fn reordering_only_among_tracks_bandcamp_moved_is_not_detected() {
        // Known exception: Bandcamp moved a and b behind c and d, and a and b
        // were swapped locally. The swap is not detected: Bandcamp's order wins.
        assert_eq!(m("abcd", "cdab", "bacd"), "cdab");
    }

    #[test]
    fn a_local_swap_of_tracks_bandcamp_kept_in_order_is_preserved() {
        // With ties broken towards cd, c and d count as kept by Bandcamp, so
        // their local swap stays and a, b move behind d.
        assert_eq!(m("abcd", "cdab", "abdc"), "dabc");
    }

    #[test]
    fn a_merge_that_skipped_a_local_move_is_stable_when_merged_again() {
        let (base, theirs, ours) = (vec![0, 1, 2, 3], vec![2, 1, 3], vec![1, 0, 2, 3]);
        let once = merge3(&base, &theirs, &ours);
        assert_eq!(once, vec![2, 1, 3]);
        assert_eq!(merge3(&base, &theirs, &once), once);
    }

    #[test]
    fn a_locally_deleted_item_is_not_brought_back_by_a_move_of_its_neighbour() {
        // c was deleted locally; a would move after c, but its anchor is
        // gone, so a stays where it is.
        assert_eq!(m("abc", "cab", "ab"), "ab");
    }

    #[test]
    fn placement_follows_theirs_neighbours_not_ours() {
        // b was moved locally so it stays; a moves after c.
        assert_eq!(m("abc", "bca", "acb"), "cab");
    }

    #[test]
    fn a_block_move_in_theirs_is_applied_when_ours_is_unchanged() {
        assert_eq!(m("abcd", "cdab", "abcd"), "cdab");
    }

    #[test]
    fn idempotence_is_not_exact_with_duplicates() {
        let once = m("aa", "aaa", "a");
        assert_eq!(once, "aa");
        assert_eq!(m("aa", "aaa", &once), "aaa");
    }

    // ---- properties with duplicates ----

    fn seq(alphabet: &'static str, max: usize) -> impl Strategy<Value = Vec<char>> {
        let letters = alphabet.chars().collect::<Vec<_>>();
        proptest::collection::vec(proptest::sample::select(letters), 0..=max)
    }

    fn count<K: PartialEq>(items: &[K], k: &K) -> usize {
        items.iter().filter(|i| *i == k).count()
    }

    proptest! {
        #[test]
        fn unchanged_theirs_yields_ours(
            base in seq("abc", 7),
            ours in seq("abcx", 7),
        ) {
            prop_assert_eq!(merge3(&base, &base, &ours), ours);
        }

        #[test]
        fn unchanged_ours_yields_theirs(
            base in seq("abc", 7),
            theirs in seq("abcd", 7),
        ) {
            prop_assert_eq!(merge3(&base, &theirs, &base), theirs);
        }

        #[test]
        fn a_local_deletion_never_comes_back_beyond_what_theirs_adds(
            base in seq("abc", 7),
            theirs in seq("abcd", 7),
            ours in seq("abc", 7),
        ) {
            let result = merge3(&base, &theirs, &ours);
            for k in "abcd".chars() {
                // Occurrences of k in theirs that are new to base can be added;
                // nothing else may raise the count above what ours holds.
                let added = count(&theirs, &k).saturating_sub(count(&base, &k));
                prop_assert!(count(&result, &k) <= count(&ours, &k) + added,
                    "{k}: result {result:?}");
            }
        }

        #[test]
        fn local_only_items_keep_their_relative_order(
            base in seq("abc", 7),
            theirs in seq("abcd", 7),
            mut ours in seq("abc", 7),
            local in seq("xyz", 4),
            at in 0usize..8,
        ) {
            // Weave local-only items (letters unknown to base and theirs) in.
            for (i, l) in local.iter().enumerate() {
                let pos = (at + i * 3).min(ours.len());
                ours.insert(pos, *l);
            }
            let result = merge3(&base, &theirs, &ours);
            let only = |v: &[char]| {
                v.iter().copied().filter(|c| "xyz".contains(*c)).collect::<Vec<_>>()
            };
            prop_assert_eq!(only(&result), only(&ours));
        }

        #[test]
        fn a_resync_with_no_bandcamp_change_is_a_no_op(
            theirs in seq("abcd", 7),
            ours in seq("abcx", 7),
        ) {
            prop_assert_eq!(merge3(&theirs, &theirs, &ours), ours);
        }

        #[test]
        fn every_result_item_comes_from_ours_or_theirs(
            base in seq("abc", 7),
            theirs in seq("abcd", 7),
            ours in seq("abcx", 7),
        ) {
            let result = merge3(&base, &theirs, &ours);
            for k in "abcdx".chars() {
                prop_assert!(
                    count(&result, &k) <= count(&ours, &k).max(count(&theirs, &k)),
                    "{k}: result {result:?}"
                );
            }
        }
    }

    // ---- properties with unique keys ----

    type Ops = Vec<(u8, usize, usize)>;

    fn ops() -> impl Strategy<Value = Ops> {
        proptest::collection::vec((0u8..3, 0usize..16, 0usize..16), 0..=4)
    }

    /// Applies deletions, inserts of fresh keys (from `fresh` up) and moves.
    fn derive(base: &[u32], ops: &Ops, fresh: u32) -> Vec<u32> {
        let mut v = base.to_vec();
        for (n, &(kind, a, b)) in ops.iter().enumerate() {
            match kind % 3 {
                0 if !v.is_empty() => {
                    v.remove(a % v.len());
                }
                1 => v.insert(a % (v.len() + 1), fresh + n as u32),
                2 if !v.is_empty() => {
                    let x = v.remove(a % v.len());
                    v.insert(b % (v.len() + 1), x);
                }
                _ => {}
            }
        }
        v
    }

    /// base is a shuffled 0..n; theirs and ours derive from it with disjoint
    /// fresh keys (100.. and 200..).
    fn unique() -> impl Strategy<Value = (Vec<u32>, Vec<u32>, Vec<u32>)> {
        (0u32..8)
            .prop_flat_map(|n| Just((0..n).collect::<Vec<_>>()).prop_shuffle())
            .prop_flat_map(|base| (Just(base), ops(), ops()))
            .prop_map(|(base, t, o)| {
                let theirs = derive(&base, &t, 100);
                let ours = derive(&base, &o, 200);
                (base, theirs, ours)
            })
    }

    proptest! {
        #[test]
        fn unique_a_local_deletion_never_comes_back((base, theirs, ours) in unique()) {
            let result = merge3(&base, &theirs, &ours);
            for k in base.iter().filter(|k| !ours.contains(k)) {
                prop_assert!(!result.contains(k), "{k} came back: {result:?}");
            }
        }

        #[test]
        fn unique_an_item_removed_in_theirs_is_absent((base, theirs, ours) in unique()) {
            let result = merge3(&base, &theirs, &ours);
            for k in base.iter().filter(|k| !theirs.contains(k)) {
                prop_assert!(!result.contains(k), "{k} survived: {result:?}");
            }
        }

        #[test]
        fn unique_an_item_added_in_theirs_is_present((base, theirs, ours) in unique()) {
            let result = merge3(&base, &theirs, &ours);
            for k in theirs.iter().filter(|k| !base.contains(k)) {
                prop_assert!(result.contains(k), "{k} missing: {result:?}");
            }
        }

        #[test]
        fn unique_ours_equal_to_theirs_yields_ours((base, theirs, _ours) in unique()) {
            prop_assert_eq!(merge3(&base, &theirs, &theirs), theirs);
        }

        #[test]
        fn unique_merging_is_idempotent((base, theirs, ours) in unique()) {
            let result = merge3(&base, &theirs, &ours);
            prop_assert_eq!(merge3(&base, &theirs, &result), result);
        }
    }
}
