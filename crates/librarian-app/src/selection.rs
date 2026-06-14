//! Multi-selection state for the file list.
//!
//! Models Windows Explorer's two-pointer scheme:
//! - the **anchor** is the pivot a Shift-range extends from, and
//! - the **lead** is the focused row the arrow keys move and that inline rename
//!   and "open" act on.
//!
//! Keeping this here (off the UI type) makes the selection rules — which are
//! fiddly and easy to get subtly wrong — testable in isolation.

use std::collections::BTreeSet;

#[derive(Debug, Default, Clone)]
pub struct Selection {
    items: BTreeSet<usize>,
    anchor: Option<usize>,
    lead: Option<usize>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn contains(&self, index: usize) -> bool {
        self.items.contains(&index)
    }

    /// The focused row, which arrow keys move and rename/open target.
    pub fn lead(&self) -> Option<usize> {
        self.lead
    }

    /// The selected indices, ascending.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.items.iter().copied()
    }

    /// The lone selected index, if exactly one row is selected.
    pub fn single(&self) -> Option<usize> {
        match self.len() {
            1 => self.items.iter().next().copied(),
            _ => None,
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.anchor = None;
        self.lead = None;
    }

    /// Select exactly `index` — a plain click or an unmodified arrow press.
    pub fn select_one(&mut self, index: usize) {
        self.items.clear();
        self.items.insert(index);
        self.anchor = Some(index);
        self.lead = Some(index);
    }

    /// Toggle `index`'s membership (Ctrl+click). The toggled row becomes both
    /// the new anchor and lead, matching Explorer.
    pub fn toggle(&mut self, index: usize) {
        if !self.items.insert(index) {
            self.items.remove(&index);
        }
        self.anchor = Some(index);
        self.lead = Some(index);
    }

    /// Replace the selection with the contiguous run from the anchor to `index`
    /// (Shift+click / Shift+arrow). The anchor stays put so the range can be
    /// resized; only the lead moves.
    pub fn select_range(&mut self, index: usize) {
        let anchor = self.anchor.unwrap_or(index);
        let (lo, hi) = (anchor.min(index), anchor.max(index));
        self.items.clear();
        self.items.extend(lo..=hi);
        self.anchor = Some(anchor);
        self.lead = Some(index);
    }

    /// Move focus to `index` without changing what's selected (Ctrl+arrow).
    pub fn move_lead(&mut self, index: usize) {
        self.lead = Some(index);
        self.anchor = Some(index);
    }

    /// Replace the selection with exactly `indices` — used to re-establish a
    /// selection by path after the directory was re-enumerated. The lead and
    /// anchor become the lowest restored index.
    pub fn set_many(&mut self, indices: impl IntoIterator<Item = usize>) {
        self.items = indices.into_iter().collect();
        let first = self.items.iter().next().copied();
        self.anchor = first;
        self.lead = first;
    }

    /// Select every row in `0..len` (Ctrl+A), leaving the lead where it is.
    pub fn select_all(&mut self, len: usize) {
        self.items = (0..len).collect();
        if self.lead.is_none() && len > 0 {
            self.lead = Some(0);
            self.anchor = Some(0);
        }
    }

    /// Drop any indices that no longer exist after the row count shrank to
    /// `len`, keeping the selection consistent with the list.
    pub fn retain_below(&mut self, len: usize) {
        self.items.retain(|&i| i < len);
        self.anchor = self.anchor.filter(|&i| i < len);
        self.lead = self.lead.filter(|&i| i < len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(sel: &Selection) -> Vec<usize> {
        sel.iter().collect()
    }

    #[test]
    fn select_one_replaces_and_sets_pointers() {
        let mut sel = Selection::default();
        sel.select_one(3);
        assert_eq!(items(&sel), [3]);
        assert_eq!(sel.lead(), Some(3));
        assert_eq!(sel.single(), Some(3));

        sel.select_one(5);
        assert_eq!(items(&sel), [5]);
        assert_eq!(sel.lead(), Some(5));
    }

    #[test]
    fn toggle_adds_then_removes() {
        let mut sel = Selection::default();
        sel.select_one(1);
        sel.toggle(4);
        assert_eq!(items(&sel), [1, 4]);
        assert_eq!(sel.lead(), Some(4));
        assert_eq!(sel.single(), None);

        sel.toggle(4);
        assert_eq!(items(&sel), [1]);
        // Lead follows the toggled row even when it was removed.
        assert_eq!(sel.lead(), Some(4));
    }

    #[test]
    fn range_extends_from_anchor_both_directions() {
        let mut sel = Selection::default();
        sel.select_one(5); // anchor = 5
        sel.select_range(2);
        assert_eq!(items(&sel), [2, 3, 4, 5]);
        assert_eq!(sel.lead(), Some(2));

        // Re-ranging from the same anchor replaces, not unions.
        sel.select_range(7);
        assert_eq!(items(&sel), [5, 6, 7]);
        assert_eq!(sel.lead(), Some(7));
    }

    #[test]
    fn range_without_anchor_selects_single() {
        let mut sel = Selection::default();
        sel.select_range(3);
        assert_eq!(items(&sel), [3]);
        assert_eq!(sel.lead(), Some(3));
    }

    #[test]
    fn move_lead_keeps_selection() {
        let mut sel = Selection::default();
        sel.select_one(2);
        sel.move_lead(6);
        assert_eq!(items(&sel), [2]);
        assert_eq!(sel.lead(), Some(6));
        // A subsequent range uses the moved anchor.
        sel.select_range(8);
        assert_eq!(items(&sel), [6, 7, 8]);
    }

    #[test]
    fn set_many_restores_arbitrary_indices() {
        let mut sel = Selection::default();
        sel.set_many([5, 1, 3]);
        assert_eq!(items(&sel), [1, 3, 5]);
        // Lead/anchor snap to the lowest index, so a later range works.
        assert_eq!(sel.lead(), Some(1));
        sel.select_range(2);
        assert_eq!(items(&sel), [1, 2]);

        // An empty restore clears everything.
        sel.set_many([]);
        assert!(sel.is_empty());
        assert_eq!(sel.lead(), None);
    }

    #[test]
    fn select_all_and_retain_below() {
        let mut sel = Selection::default();
        sel.select_all(4);
        assert_eq!(items(&sel), [0, 1, 2, 3]);

        sel.retain_below(2);
        assert_eq!(items(&sel), [0, 1]);
        assert!(sel.lead().is_none_or(|l| l < 2));
    }
}
