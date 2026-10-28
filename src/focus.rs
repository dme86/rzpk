//! Bounded, pane-local history of the last focused entry in each directory.
//! This is session state, not persisted bookmarks or marked-file selection.
use std::{collections::VecDeque, path::{Path, PathBuf}};

const MAX_DIRECTORIES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Focus {
    pub entry: PathBuf,
    pub scroll: usize,
}

#[derive(Debug, Default)]
pub struct FocusHistory {
    recent: VecDeque<(PathBuf, Focus)>,
}

impl FocusHistory {
    pub fn remember(&mut self, directory: &Path, entry: &Path, scroll: usize) {
        // Only store real descendants of the directory, not virtual parent
        // rows or a tree pagination placeholder.
        if entry == directory || !entry.starts_with(directory) {
            return;
        }
        if let Some(index) = self.recent.iter().position(|(path, _)| path == directory) {
            self.recent.remove(index);
        }
        self.recent.push_back((
            directory.to_path_buf(),
            Focus { entry: entry.to_path_buf(), scroll },
        ));
        if self.recent.len() > MAX_DIRECTORIES {
            self.recent.pop_front();
        }
    }

    pub fn recall(&self, directory: &Path) -> Option<&Focus> {
        self.recent.iter().rev().find(|(path, _)| path == directory).map(|(_, focus)| focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_latest_focus_and_scroll_for_each_directory() {
        let mut history = FocusHistory::default();
        history.remember(Path::new("/repo"), Path::new("/repo/README.md"), 7);
        history.remember(Path::new("/other"), Path::new("/other/file"), 2);
        history.remember(Path::new("/repo"), Path::new("/repo/src"), 12);
        assert_eq!(history.recall(Path::new("/repo")).unwrap().entry, Path::new("/repo/src"));
        assert_eq!(history.recall(Path::new("/repo")).unwrap().scroll, 12);
        assert_eq!(history.recall(Path::new("/other")).unwrap().entry, Path::new("/other/file"));
    }

    #[test]
    fn does_not_store_virtual_parent_or_unrelated_entry() {
        let mut history = FocusHistory::default();
        history.remember(Path::new("/repo"), Path::new("/repo"), 0);
        history.remember(Path::new("/repo"), Path::new("/other/file"), 0);
        assert!(history.recall(Path::new("/repo")).is_none());
    }

    #[test]
    fn oldest_directories_are_discarded_when_bounded_capacity_is_reached() {
        let mut history = FocusHistory::default();
        for i in 0..=MAX_DIRECTORIES {
            history.remember(
                Path::new("/tmp"),
                &PathBuf::from(format!("/tmp/{i}")),
                i,
            );
            // Same directory replaces its previous slot.
        }
        assert_eq!(history.recall(Path::new("/tmp")).unwrap().scroll, MAX_DIRECTORIES);
        for i in 0..=MAX_DIRECTORIES {
            history.remember(
                &PathBuf::from(format!("/folder-{i}")),
                &PathBuf::from(format!("/folder-{i}/a")),
                i,
            );
        }
        assert!(history.recall(Path::new("/folder-0")).is_none());
        assert!(history.recall(Path::new("/folder-1")).is_some());
        assert!(history.recall(Path::new("/folder-128")).is_some());
    }
}
