use ratatui::widgets::ListState;
use std::collections::BTreeSet;

/// Kakoune-style selection model with a filter stack.
///
/// Each `push_filter` narrows visibility; `pop_filter` restores the previous
/// visibility level. Selected indices always refer to positions in the
/// *currently visible* item list.
pub struct SelectionState {
    pub list_state: ListState,
    /// Indices into the *visible* list that are selected.
    pub selected: BTreeSet<usize>,
    /// Stack of visibility masks. Each mask has length == total_items.
    /// An item is visible if ALL masks in the stack have `true` at its index.
    filter_stack: Vec<Vec<bool>>,
    pub total_items: usize,
    /// Cached mapping: visible_index → data_index.
    pub visible_to_data: Vec<usize>,
}

#[allow(dead_code)]
impl SelectionState {
    /// Create a new selection state with all items visible and no selection.
    pub fn new(total_items: usize) -> Self {
        let visible_to_data: Vec<usize> = (0..total_items).collect();
        let mut list_state = ListState::default();
        if total_items > 0 {
            list_state.select(Some(0));
        }
        Self {
            list_state,
            selected: BTreeSet::new(),
            filter_stack: Vec::new(),
            total_items,
            visible_to_data,
        }
    }

    /// Reset all state for a new total item count.
    pub fn set_total_items(&mut self, total: usize) {
        self.total_items = total;
        self.filter_stack.clear();
        self.selected.clear();
        self.recompute_visible();
        if total > 0 {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    /// Number of currently visible items.
    pub fn visible_count(&self) -> usize {
        self.visible_to_data.len()
    }

    /// Map a visible index to its underlying data index.
    pub fn visible_index_to_data(&self, vis_idx: usize) -> Option<usize> {
        self.visible_to_data.get(vis_idx).copied()
    }

    /// Move the cursor down by one, clamping at the last visible item.
    pub fn move_cursor_down(&mut self) {
        let count = self.visible_count();
        if count == 0 {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        let next = (current + 1).min(count - 1);
        self.list_state.select(Some(next));
    }

    /// Move the cursor up by one, clamping at 0.
    pub fn move_cursor_up(&mut self) {
        if self.visible_count() == 0 {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        let next = current.saturating_sub(1);
        self.list_state.select(Some(next));
    }

    /// Current cursor position in the visible list.
    pub fn cursor_position(&self) -> Option<usize> {
        self.list_state.selected()
    }

    /// `x` key: add cursor position to selection, then advance cursor.
    pub fn extend_selection_down(&mut self) {
        if let Some(pos) = self.list_state.selected() {
            self.selected.insert(pos);
            self.move_cursor_down();
        }
    }

    /// `X` key: add cursor position to selection, then retreat cursor.
    pub fn extend_selection_up(&mut self) {
        if let Some(pos) = self.list_state.selected() {
            self.selected.insert(pos);
            self.move_cursor_up();
        }
    }

    /// `%` key: select all currently visible indices.
    pub fn select_all(&mut self) {
        for i in 0..self.visible_count() {
            self.selected.insert(i);
        }
    }

    /// Deselect everything.
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// `s` key: push a new filter onto the stack.
    ///
    /// `match_fn` receives the *data* index and returns `true` if the item
    /// should remain visible within the currently visible items.
    /// Items not currently visible are always excluded.
    pub fn push_filter(&mut self, match_fn: impl Fn(usize) -> bool) {
        // Build a new mask over ALL items. An item passes the new filter only
        // if it was already visible (in visible_to_data) AND match_fn returns
        // true for its data index.
        let mut mask = vec![false; self.total_items];
        for &data_idx in &self.visible_to_data {
            if match_fn(data_idx) {
                mask[data_idx] = true;
            }
        }
        self.filter_stack.push(mask);
        self.selected.clear();
        self.recompute_visible();
        self.list_state.select(if self.visible_count() > 0 {
            Some(0)
        } else {
            None
        });
    }

    /// `Escape` key: pop the top filter from the stack.
    ///
    /// Returns `false` if the stack was already empty.
    pub fn pop_filter(&mut self) -> bool {
        if self.filter_stack.is_empty() {
            return false;
        }
        self.filter_stack.pop();
        self.selected.clear();
        self.recompute_visible();
        self.list_state.select(if self.visible_count() > 0 {
            Some(0)
        } else {
            None
        });
        true
    }

    /// Recompute the `visible_to_data` cache from the current filter stack.
    fn recompute_visible(&mut self) {
        self.visible_to_data = (0..self.total_items)
            .filter(|&i| self.filter_stack.iter().all(|mask| mask[i]))
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_all_items_visible_nothing_selected() {
        let state = SelectionState::new(5);
        assert_eq!(state.visible_count(), 5);
        assert!(state.selected.is_empty());
        assert_eq!(state.cursor_position(), Some(0));
    }

    #[test]
    fn new_zero_items_no_cursor() {
        let state = SelectionState::new(0);
        assert_eq!(state.visible_count(), 0);
        assert_eq!(state.cursor_position(), None);
    }

    #[test]
    fn extend_selection_down_adds_current_advances_cursor() {
        let mut state = SelectionState::new(5);
        // cursor starts at 0
        state.extend_selection_down();
        assert!(state.selected.contains(&0));
        assert_eq!(state.cursor_position(), Some(1));
    }

    #[test]
    fn extend_selection_up_adds_current_retreats_cursor() {
        let mut state = SelectionState::new(5);
        // move cursor to 2 first
        state.move_cursor_down();
        state.move_cursor_down();
        assert_eq!(state.cursor_position(), Some(2));
        state.extend_selection_up();
        assert!(state.selected.contains(&2));
        assert_eq!(state.cursor_position(), Some(1));
    }

    #[test]
    fn select_all_selects_all_visible_indices() {
        let mut state = SelectionState::new(3);
        state.select_all();
        assert!(state.selected.contains(&0));
        assert!(state.selected.contains(&1));
        assert!(state.selected.contains(&2));
        assert_eq!(state.selected.len(), 3);
    }

    #[test]
    fn push_filter_reduces_visible_count() {
        let mut state = SelectionState::new(5);
        // only keep even data indices
        state.push_filter(|i| i % 2 == 0);
        // data indices 0, 2, 4 are visible
        assert_eq!(state.visible_count(), 3);
    }

    #[test]
    fn pop_filter_restores_previous_visibility() {
        let mut state = SelectionState::new(5);
        state.push_filter(|i| i % 2 == 0);
        assert_eq!(state.visible_count(), 3);
        let popped = state.pop_filter();
        assert!(popped);
        assert_eq!(state.visible_count(), 5);
    }

    #[test]
    fn pop_filter_on_empty_returns_false() {
        let mut state = SelectionState::new(5);
        let result = state.pop_filter();
        assert!(!result);
        // visibility unchanged
        assert_eq!(state.visible_count(), 5);
    }

    #[test]
    fn push_filter_clears_selection() {
        let mut state = SelectionState::new(5);
        state.select_all();
        assert_eq!(state.selected.len(), 5);
        state.push_filter(|_| true);
        assert!(state.selected.is_empty());
    }

    #[test]
    fn two_pushes_intersect_masks() {
        let mut state = SelectionState::new(6);
        // first filter: even indices → 0, 2, 4
        state.push_filter(|i| i % 2 == 0);
        // second filter: indices < 4 → within visible {0, 2, 4}, only 0 and 2
        state.push_filter(|i| i < 4);
        assert_eq!(state.visible_count(), 2);
        assert_eq!(state.visible_to_data, vec![0, 2]);
    }

    #[test]
    fn cursor_clamps_at_end() {
        let mut state = SelectionState::new(3);
        state.move_cursor_down();
        state.move_cursor_down();
        state.move_cursor_down(); // would be 3, clamped to 2
        state.move_cursor_down(); // still 2
        assert_eq!(state.cursor_position(), Some(2));
    }

    #[test]
    fn cursor_clamps_at_zero() {
        let mut state = SelectionState::new(3);
        state.move_cursor_up(); // already at 0
        assert_eq!(state.cursor_position(), Some(0));
    }

    #[test]
    fn visible_index_to_data_maps_correctly() {
        let mut state = SelectionState::new(5);
        state.push_filter(|i| i % 2 == 0);
        // visible: [0, 2, 4]
        assert_eq!(state.visible_index_to_data(0), Some(0));
        assert_eq!(state.visible_index_to_data(1), Some(2));
        assert_eq!(state.visible_index_to_data(2), Some(4));
        assert_eq!(state.visible_index_to_data(3), None);
    }
}
