//! Selected entries in the current directory.
//! A temporary range makes Shift+movement reversible while independent marks persist.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy)]
struct Range {
    anchor: usize,
    cursor: usize,
}

impl Range {
    fn bounds(self) -> std::ops::RangeInclusive<usize> {
        self.anchor.min(self.cursor)..=self.anchor.max(self.cursor)
    }
}

#[derive(Debug, Default)]
pub struct Selection {
    explicit: BTreeSet<usize>,
    range: Option<Range>,
}

impl Selection {
    /// Begin or extend a shift-selection, keeping the first cursor as its anchor.
    pub fn extend(&mut self, from: usize, to: usize) {
        let anchor = self.range.map_or(from, |range| range.anchor);
        self.range = Some(Range { anchor, cursor: to });
    }

    /// Preserve the current range before plain navigation or individual toggling.
    pub fn commit(&mut self) {
        if let Some(range) = self.range.take() {
            self.explicit.extend(range.bounds());
        }
    }

    pub fn toggle(&mut self, index: usize) {
        self.commit();
        if !self.explicit.insert(index) {
            self.explicit.remove(&index);
        }
    }

    pub fn contains(&self, index: usize) -> bool {
        self.explicit.contains(&index)
            || self.range.is_some_and(|range| range.bounds().contains(&index))
    }

    pub fn count(&self) -> usize {
        match self.range {
            None => self.explicit.len(),
            Some(range) => {
                let bounds = range.bounds();
                bounds.clone().count() + self.explicit.len()
                    - self.explicit.range(bounds).count()
            }
        }
    }

    /// Selections are scoped to the current directory, never reused by index elsewhere.
    pub fn clear(&mut self) {
        self.explicit.clear();
        self.range = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_range_grows_and_shrinks() {
        let mut selection = Selection::default();
        selection.extend(2, 3);
        selection.extend(3, 5);
        assert_eq!(selection.count(), 4);
        assert!((2..=5).all(|index| selection.contains(index)));
        selection.extend(5, 3);
        assert_eq!(selection.count(), 2);
        assert!(!selection.contains(4));
        assert!(!selection.contains(5));
    }

    #[test]
    fn range_can_grow_backwards() {
        let mut selection = Selection::default();
        selection.extend(4, 3);
        selection.extend(3, 2);
        assert_eq!(selection.count(), 3);
        assert!((2..=4).all(|index| selection.contains(index)));
    }

    #[test]
    fn disjoint_toggle_preserves_existing_marks() {
        let mut selection = Selection::default();
        selection.extend(1, 3);
        selection.commit();
        selection.toggle(7);
        assert_eq!(selection.count(), 4);
        assert!(selection.contains(7));
        selection.toggle(7);
        assert_eq!(selection.count(), 3);
    }

    #[test]
    fn explicit_marks_survive_range_contraction() {
        let mut selection = Selection::default();
        selection.toggle(5);
        selection.extend(1, 4);
        assert_eq!(selection.count(), 5);
        selection.extend(4, 2);
        assert_eq!(selection.count(), 3);
        assert!(selection.contains(5));
        assert!(!selection.contains(4));
    }

    #[test]
    fn overlapping_marks_are_not_double_counted() {
        let mut selection = Selection::default();
        selection.toggle(2);
        selection.extend(1, 3);
        assert_eq!(selection.count(), 3);
        selection.commit();
        assert_eq!(selection.count(), 3);
    }

    #[test]
    fn reset_drops_all_selection_state() {
        let mut selection = Selection::default();
        selection.toggle(1);
        selection.extend(3, 4);
        selection.clear();
        assert_eq!(selection.count(), 0);
        assert!(!selection.contains(1));
        assert!(!selection.contains(4));
    }
}
