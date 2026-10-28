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
        let mut entries = Vec::new();
        self.append(root, "", 0, &mut entries)?;
        Ok(entries)
    }

    fn append(
        &mut self,
        directory: &Path,
        indent: &str,
        depth: usize,
        entries: &mut Vec<Entry>,
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
            let expanded = entry.is_dir
                && !entry.is_symlink
                && self.expanded.contains(&path);
            self.rows.push(TreeRow {
                prefix,
                kind: RowKind::Entry { expanded },
            });
            entries.push(entry);

            if expanded && depth < MAX_DEPTH && entries.len() < MAX_VISIBLE {
                // Inaccessible folders are displayed with a closed branch,
                // rather than making the entire view unusable.
                if self.append(&path, &child_indent, depth + 1, entries).is_err() {
                    self.expanded.remove(&path);
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
        if !self.expanded.insert(path.to_path_buf()) {
            self.expanded.remove(path);
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
    fn nested_directories_are_only_read_on_expansion() -> io::Result<()> {
        let root = fixture();
        let nested = root.join("nested");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("child.txt"), b"x")?;
        let (mut tree, initial) = TreeState::open(&root)?;
        assert_eq!(initial.len(), 1);
        let expanded = tree.toggle_directory(&root, &nested)?;
        assert_eq!(expanded.len(), 2);
        assert!(tree.rows[1].prefix.contains("└──"));
        let collapsed = tree.toggle_directory(&root, &nested)?;
        assert_eq!(collapsed.len(), 1);
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
