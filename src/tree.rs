//! Lazily expanded, deliberately bounded directory tree.
//!
//! Reading a tree never recursively scans the filesystem. Each expanded
//! directory fetches a small page and shows an explicit "more" entry.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io,
    path::{Path, PathBuf},
};

use crate::{Entry, directory_entry, sort_entries};

pub const PAGE_SIZE: usize = 24;
pub const MAX_VISIBLE: usize = 360;
pub const MAX_DEPTH: usize = 12;
// Expand the first two directory levels, but only a few branches automatically.
// Further branches are opt-in with Enter, so a large repository remains readable.
pub const AUTO_DEPTH: usize = 2;
pub const MAX_AUTO_EXPAND: usize = 6;

#[derive(Debug)]
pub enum RowKind {
    Entry { expanded: bool },
    More(PathBuf),
    Limit,
}

#[derive(Debug)]
pub struct TreeRow {
    pub prefix: String,
    pub kind: RowKind,
}

impl TreeRow {
    pub fn real_entry(&self) -> bool {
        matches!(self.kind, RowKind::Entry { .. })
    }
}

#[derive(Debug, Default)]
pub struct TreeState {
    pub rows: Vec<TreeRow>,
    expanded: HashSet<PathBuf>,
    collapsed: HashSet<PathBuf>,
    auto_opened: HashSet<PathBuf>,
    pages: HashMap<PathBuf, usize>,
}

impl TreeState {
    pub fn open(root: &Path) -> io::Result<(Self, Vec<Entry>)> {
        let mut state = Self::default();
        let entries = state.rebuild(root)?;
        Ok((state, entries))
    }

    /// Rebuild only when the user enters, collapses or expands a directory.
    /// Rendering never scans the filesystem.
    pub fn rebuild(&mut self, root: &Path) -> io::Result<Vec<Entry>> {
        self.rows.clear();
        self.auto_opened.clear();
        let mut entries = Vec::new();
        let mut auto_budget = MAX_AUTO_EXPAND;
        self.append(root, "", 0, &mut entries, &mut auto_budget)?;
        Ok(entries)
    }

    fn append(
        &mut self,
        directory: &Path,
        indent: &str,
        depth: usize,
        entries: &mut Vec<Entry>,
        auto_budget: &mut usize,
    ) -> io::Result<()> {
        if depth > MAX_DEPTH || entries.len() >= MAX_VISIBLE {
            return Ok(());
        }
        let requested = *self.pages.get(directory).unwrap_or(&PAGE_SIZE);
        // Read at most one additional entry: no full materialization of huge dirs.
        let mut batch = Vec::new();
        let mut listing = fs::read_dir(directory)?;
        for item in listing.by_ref().take(requested + 1) {
            batch.push(directory_entry(item?)?);
        }
        let has_more = batch.len() > requested;
        batch.truncate(requested);
        sort_entries(&mut batch);
        let visible_count = batch.len();

        for (idx, entry) in batch.into_iter().enumerate() {
            if entries.len() >= MAX_VISIBLE {
                break;
            }
            let has_next = idx + 1 < visible_count || has_more;
            // Parent guides remain visible through expanded descendants.
            let branch = if has_next { "├── " } else { "└── " };
            let prefix = format!("{indent}{branch}");
            let child_indent = format!("{indent}{}", if has_next { "│   " } else { "    " });
            let path = entry.path.clone();
            // Explicitly toggled branches override automatic expansion. The
            // budget limits how many branches are opened during a rebuild.
            let auto_expand = depth < AUTO_DEPTH
                && !self.collapsed.contains(&path)
                && !self.expanded.contains(&path)
                && *auto_budget > 0;
            if auto_expand && entry.is_dir && !entry.is_symlink {
                *auto_budget -= 1;
                self.auto_opened.insert(path.clone());
            }
            let expanded = entry.is_dir
                && !entry.is_symlink
                && (self.expanded.contains(&path) || auto_expand);
            self.rows.push(TreeRow {
                prefix,
                kind: RowKind::Entry { expanded },
            });
            entries.push(entry);

            if expanded && depth < MAX_DEPTH && entries.len() < MAX_VISIBLE {
                // Inaccessible folders are displayed with a closed branch,
                // rather than making the entire view unusable.
                if self.append(&path, &child_indent, depth + 1, entries, auto_budget).is_err() {
                    self.expanded.remove(&path);
                    self.collapsed.insert(path);
                }
            }
        }

        if has_more && entries.len() < MAX_VISIBLE {
            let path = directory.to_path_buf();
            self.rows.push(TreeRow {
                prefix: format!("{indent}└── "),
                kind: RowKind::More(path.clone()),
            });
            entries.push(placeholder(path, "… more entries (Enter)"));
        } else if entries.len() >= MAX_VISIBLE {
            // A noninteractive safety marker avoids runaway expansion.
            if let Some(last) = entries.last_mut()
                && let Some(row) = self.rows.last_mut()
            {
                *last = placeholder(directory.to_path_buf(), "… tree limit (collapse branches)");
                row.kind = RowKind::Limit;
            }
        }
        Ok(())
    }

    pub fn activate(&mut self, root: &Path, at: usize) -> io::Result<Vec<Entry>> {
        let Some(row) = self.rows.get(at) else {
            return Ok(Vec::new());
        };
        match &row.kind {
            RowKind::More(directory) => {
                let current = self.pages.get(directory).copied().unwrap_or(PAGE_SIZE);
                self.pages.insert(directory.clone(), current.saturating_add(PAGE_SIZE).min(MAX_VISIBLE));
            }
            RowKind::Entry { .. } => return Ok(Vec::new()),
            RowKind::Limit => return Ok(Vec::new()),
        }
        self.rebuild(root)
    }

    pub fn toggle_directory(&mut self, root: &Path, path: &Path) -> io::Result<Vec<Entry>> {
        // Auto-opened directories are open too, even though they were never
        // added to the explicit expansion set.
        if self.expanded.remove(path) || self.auto_opened.contains(path) {
            self.collapsed.insert(path.to_path_buf());
        } else {
            self.collapsed.remove(path);
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild(root)
    }

    pub fn is_real(&self, index: usize) -> bool {
        self.rows.get(index).is_some_and(TreeRow::real_entry)
    }
}

fn placeholder(path: PathBuf, name: &str) -> Entry {
    Entry {
        path,
        name: name.to_string(),
        name_key: String::new(),
        is_dir: false,
        is_symlink: false,
        permissions: String::new(),
        size: None,
        modified: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rzpk-tree-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn big_directory_is_bounded_and_more_is_visible() -> io::Result<()> {
        let root = fixture();
        for index in 0..(PAGE_SIZE + 15) {
            fs::write(root.join(format!("file_{index:03}.txt")), b"hi")?;
        }
        let (mut tree, entries) = TreeState::open(&root)?;
        assert_eq!(entries.len(), PAGE_SIZE + 1);
        assert!(matches!(tree.rows.last().unwrap().kind, RowKind::More(_)));
        let more = tree.activate(&root, PAGE_SIZE)?;
        assert_eq!(more.len(), PAGE_SIZE + 15);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn directories_open_automatically_and_can_be_collapsed_and_reopened() -> io::Result<()> {
        let root = fixture();
        let nested = root.join("nested");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("child.txt"), b"x")?;
        let (mut tree, initial) = TreeState::open(&root)?;
        assert_eq!(initial.len(), 2);
        assert!(matches!(tree.rows[0].kind, RowKind::Entry { expanded: true }));
        assert_eq!(initial[1].name, "child.txt");
        let collapsed = tree.toggle_directory(&root, &nested)?;
        assert_eq!(collapsed.len(), 1);
        assert!(matches!(tree.rows[0].kind, RowKind::Entry { expanded: false }));
        let reopened = tree.toggle_directory(&root, &nested)?;
        assert_eq!(reopened.len(), 2);
        assert!(matches!(tree.rows[0].kind, RowKind::Entry { expanded: true }));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn defaults_expand_only_two_directory_levels() -> io::Result<()> {
        let root = fixture();
        let first = root.join("a");
        let second = first.join("b");
        let third = second.join("c");
        fs::create_dir_all(&third)?;
        fs::write(third.join("hidden.txt"), b"x")?;
        let (mut tree, initial) = TreeState::open(&root)?;
        assert_eq!(initial.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]);
        assert!(matches!(tree.rows[0].kind, RowKind::Entry { expanded: true }));
        assert!(matches!(tree.rows[1].kind, RowKind::Entry { expanded: true }));
        assert!(matches!(tree.rows[2].kind, RowKind::Entry { expanded: false }));
        let opened = tree.toggle_directory(&root, &third)?;
        assert_eq!(opened.len(), 4);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn auto_expansion_has_a_strict_branch_budget() -> io::Result<()> {
        let root = fixture();
        for index in 0..14 {
            let dir = root.join(format!("folder_{index:02}"));
            fs::create_dir(&dir)?;
            fs::write(dir.join("inside.txt"), b"x")?;
        }
        let (tree, entries) = TreeState::open(&root)?;
        let auto_open = tree.rows.iter().filter(|row|
            matches!(row.kind, RowKind::Entry { expanded: true })
        ).count();
        assert_eq!(auto_open, MAX_AUTO_EXPAND);
        assert_eq!(entries.len(), 14 + MAX_AUTO_EXPAND);
        assert!(entries.len() <= MAX_VISIBLE);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn many_files_still_require_pagination() -> io::Result<()> {
        let root = fixture();
        let dir = root.join("src");
        fs::create_dir(&dir)?;
        for index in 0..80 {
            fs::write(dir.join(format!("file_{index:03}.rs")), b"hi")?;
        }
        let (mut tree, initial) = TreeState::open(&root)?;
        assert_eq!(initial.len(), 1 + PAGE_SIZE + 1);
        let more_pos = tree.rows.iter().position(|row| matches!(row.kind, RowKind::More(_))).unwrap();
        let more = tree.activate(&root, more_pos)?;
        assert_eq!(more.len(), 1 + PAGE_SIZE * 2 + 1);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinked_folders() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let root = fixture();
        fs::create_dir(root.join("real"))?;
        symlink(root.join("real"), root.join("link"))?;
        let (tree, entries) = TreeState::open(&root)?;
        assert_eq!(entries.len(), 2);
        assert!(tree.rows.iter().all(TreeRow::real_entry));
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
