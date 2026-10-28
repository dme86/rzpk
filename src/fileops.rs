//! Safe, explicit file transfers between two directory panes.
//!
//! Files are created exclusively, directory destinations must not exist, and
//! symlinks are recreated as links rather than traversed. A failed copy removes
//! its incomplete destination; a move deletes its source only after copying it.
//! Copy/move work is synchronous for now (a future worker can report progress).
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::jobs::{Progress, TrackedWriter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferKind {
    Copy,
    Move,
}

impl TransferKind {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Move => "Move",
        }
    }

    pub fn past_tense(self) -> &'static str {
        match self {
            Self::Copy => "Copied",
            Self::Move => "Moved",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Transfer {
    pub kind: TransferKind,
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message.into())
}

fn target_path(source: &Path, destination: &Path) -> io::Result<PathBuf> {
    let name = source.file_name().ok_or_else(|| invalid("Cannot transfer a filesystem root"))?;
    Ok(destination.join(name))
}

fn is_present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

impl Transfer {
    pub fn new(kind: TransferKind, sources: Vec<PathBuf>, destination: PathBuf) -> io::Result<Self> {
        if sources.is_empty() {
            return Err(invalid("No files or folders to transfer"));
        }

        let canonical_destination = fs::canonicalize(&destination)?;
        if !canonical_destination.is_dir() {
            return Err(invalid("Destination is not a directory"));
        }

        // Selecting an expanded folder and a file inside it should transfer the
        // folder only once, not attempt a duplicate nested transfer.
        let mut unique = Vec::new();
        let mut seen = HashSet::new();
        for source in sources {
            if seen.insert(source.clone()) {
                unique.push(source);
            }
        }
        unique.sort_by_key(|path| path.components().count());
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut canonical_roots: Vec<PathBuf> = Vec::new();
        for source in unique {
            let meta = fs::symlink_metadata(&source)?;
            if !meta.is_dir() && !meta.is_file() && !meta.file_type().is_symlink() {
                return Err(invalid(format!("Unsupported file type: {}", source.display())));
            }
            // Don't resolve the final component of a symlink: dangling links
            // are valid filesystem entries and should still be transferable.
            let canonical_source = if meta.file_type().is_symlink() {
                let parent = source.parent().ok_or_else(|| invalid("Source has no parent"))?;
                let name = source.file_name().ok_or_else(|| invalid("Source has no filename"))?;
                fs::canonicalize(parent)?.join(name)
            } else {
                fs::canonicalize(&source)?
            };
            if canonical_roots.iter().any(|root| canonical_source.starts_with(root)) {
                continue;
            }
            if meta.is_dir() && canonical_destination.starts_with(&canonical_source) {
                return Err(invalid(format!(
                    "Cannot transfer a directory into itself: {}",
                    source.display()
                )));
            }
            roots.push(source);
            canonical_roots.push(canonical_source);
        }

        let mut targets: HashSet<OsString> = HashSet::new();
        for source in &roots {
            let name = source.file_name().ok_or_else(|| invalid("Cannot transfer a filesystem root"))?;
            if !targets.insert(name.to_os_string()) {
                return Err(invalid(format!("Multiple sources share the name {}", name.to_string_lossy())));
            }
            let target = target_path(source, &canonical_destination)?;
            if is_present(&target)? {
                return Err(io::Error::new(
                    ErrorKind::AlreadyExists,
                    format!("Destination already exists: {}", target.display()),
                ));
            }
        }

        Ok(Self {
            kind,
            sources: roots,
            destination: canonical_destination,
        })
    }

    pub fn execute(&self) -> io::Result<usize> {
        self.execute_internal(None)
    }

    pub fn execute_with_progress(&self, progress: &Arc<Progress>) -> io::Result<usize> {
        self.execute_internal(Some(progress))
    }

    fn execute_internal(&self, progress: Option<&Arc<Progress>>) -> io::Result<usize> {
        // Check again immediately before executing, as paths may have changed
        // since the user opened the confirmation dialog.
        let checked = Self::new(self.kind, self.sources.clone(), self.destination.clone())?;
        let mut completed = 0;
        if let Some(progress) = progress {
            progress.set_total(checked.sources.len());
        }
        for source in &checked.sources {
            let target = target_path(source, &checked.destination)?;
            let meta = fs::symlink_metadata(source)?;
            // A same-volume regular-file move can be instantaneous: creating
            // a hard link is exclusive and cannot overwrite an existing file.
            // Cross-volume moves fall back to the safe copy-then-delete path.
            let linked = checked.kind == TransferKind::Move
                && meta.is_file()
                && fs::hard_link(source, &target).is_ok();
            if !linked {
                copy_item(source, &target, progress)?;
            }
            if checked.kind == TransferKind::Move {
                if meta.is_dir() {
                    fs::remove_dir_all(source)?;
                } else {
                    fs::remove_file(source)?;
                }
            }
            completed += 1;
            if let Some(progress) = progress {
                progress.item_done();
            }
        }
        Ok(completed)
    }
}

fn copy_item(source: &Path, destination: &Path, progress: Option<&Arc<Progress>>) -> io::Result<()> {
    let meta = fs::symlink_metadata(source)?;

    if meta.file_type().is_symlink() {
        copy_link(source, destination)
    } else if meta.is_file() {
        copy_file(source, destination, &meta, progress)
    } else if meta.is_dir() {
        copy_directory(source, destination, &meta, progress)
    } else {
        Err(invalid(format!("Unsupported file type: {}", source.display())))
    }
}

fn copy_file(source: &Path, destination: &Path, meta: &fs::Metadata, progress: Option<&Arc<Progress>>) -> io::Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
    let result = (|| {
        if let Some(progress) = progress {
            let mut tracked = TrackedWriter::new(&mut output, progress);
            io::copy(&mut input, &mut tracked)?;
        } else {
            io::copy(&mut input, &mut output)?;
        }
        output.sync_all()?;
        fs::set_permissions(destination, meta.permissions())
    })();
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

fn copy_directory(source: &Path, destination: &Path, meta: &fs::Metadata, progress: Option<&Arc<Progress>>) -> io::Result<()> {
    fs::create_dir(destination)?; // Never merge into an existing directory.
    let result = (|| {
        for child in fs::read_dir(source)? {
            let child = child?;
            copy_item(&child.path(), &destination.join(child.file_name()), progress)?;
        }
        fs::set_permissions(destination, meta.permissions())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

#[cfg(unix)]
fn copy_link(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::fs::symlink;
    symlink(fs::read_link(source)?, destination)
}

#[cfg(windows)]
fn copy_link(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let target = fs::read_link(source)?;
    if source.is_dir() {
        symlink_dir(target, destination)
    } else {
        symlink_file(target, destination)
    }
}

#[cfg(not(any(unix, windows)))]
fn copy_link(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(invalid("Symlink copies are not supported on this platform"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("rzpk-transfer-{}-{nonce}", std::process::id()));
        fs::create_dir_all(root.join("left")).unwrap();
        fs::create_dir_all(root.join("right")).unwrap();
        root
    }

    #[test]
    fn copies_file_without_removing_source() -> io::Result<()> {
        let root = fixture();
        let source = root.join("left").join("hello.txt");
        fs::write(&source, "hello")?;
        let transfer = Transfer::new(TransferKind::Copy, vec![source.clone()], root.join("right"))?;
        assert_eq!(transfer.execute()?, 1);
        assert_eq!(fs::read_to_string(root.join("right/hello.txt"))?, "hello");
        assert!(source.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn moves_multiple_files() -> io::Result<()> {
        let root = fixture();
        let a = root.join("left/a.txt");
        let b = root.join("left/b.txt");
        fs::write(&a, "a")?;
        fs::write(&b, "b")?;
        let transfer = Transfer::new(TransferKind::Move, vec![a.clone(), b.clone()], root.join("right"))?;
        assert_eq!(transfer.execute()?, 2);
        assert!(!a.exists() && !b.exists());
        assert!(root.join("right/a.txt").exists() && root.join("right/b.txt").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn recursive_directory_copy_and_move() -> io::Result<()> {
        let root = fixture();
        let source = root.join("left/sub");
        fs::create_dir_all(source.join("nested"))?;
        fs::write(source.join("nested/README.md"), "content")?;
        let copy = Transfer::new(TransferKind::Copy, vec![source.clone()], root.join("right"))?;
        copy.execute()?;
        assert_eq!(fs::read_to_string(root.join("right/sub/nested/README.md"))?, "content");
        assert!(source.exists());

        let moved = root.join("left/sub2");
        fs::create_dir(&moved)?;
        fs::write(moved.join("note.txt"), "note")?;
        Transfer::new(TransferKind::Move, vec![moved.clone()], root.join("right"))?.execute()?;
        assert!(!moved.exists());
        assert!(root.join("right/sub2/note.txt").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rejects_existing_target_before_any_copy() -> io::Result<()> {
        let root = fixture();
        let a = root.join("left/a.txt");
        let b = root.join("left/b.txt");
        fs::write(&a, "source")?;
        fs::write(&b, "b")?;
        fs::write(root.join("right/a.txt"), "original")?;
        let result = Transfer::new(TransferKind::Copy, vec![b, a], root.join("right"));
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(root.join("right/a.txt"))?, "original");
        assert!(!root.join("right/b.txt").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn rejects_copy_into_self_and_descendant() -> io::Result<()> {
        let root = fixture();
        let dir = root.join("left/mydir");
        let child = dir.join("child");
        fs::create_dir_all(&child)?;
        let err = Transfer::new(TransferKind::Copy, vec![dir], child).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn nested_marked_items_are_transferred_only_once() -> io::Result<()> {
        let root = fixture();
        let dir = root.join("left/folder");
        fs::create_dir(&dir)?;
        let child = dir.join("file.txt");
        fs::write(&child, "hello")?;
        let transfer = Transfer::new(TransferKind::Copy, vec![child, dir], root.join("right"))?;
        assert_eq!(transfer.sources.len(), 1);
        assert_eq!(transfer.execute()?, 1);
        assert!(root.join("right/folder/file.txt").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_copy_preserves_link_not_target() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let root = fixture();
        fs::write(root.join("left/original.txt"), "body")?;
        let src = root.join("left/link.txt");
        symlink("original.txt", &src)?;
        Transfer::new(TransferKind::Copy, vec![src], root.join("right"))?.execute()?;
        let dest = root.join("right/link.txt");
        assert!(fs::symlink_metadata(&dest)?.file_type().is_symlink());
        assert_eq!(fs::read_link(&dest)?, PathBuf::from("original.txt"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlinks_can_be_copied_without_following_them() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let root = fixture();
        let source = root.join("left/missing.txt");
        symlink("missing-target.txt", &source)?;
        Transfer::new(TransferKind::Copy, vec![source], root.join("right"))?.execute()?;
        assert_eq!(
            fs::read_link(root.join("right/missing.txt"))?,
            PathBuf::from("missing-target.txt")
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn no_overwrite_even_if_a_target_appears_after_confirmation() -> io::Result<()> {
        let root = fixture();
        let source = root.join("left/hello.txt");
        fs::write(&source, "source")?;
        let transfer = Transfer::new(TransferKind::Move, vec![source.clone()], root.join("right"))?;
        fs::write(root.join("right/hello.txt"), "new target")?;
        let err = transfer.execute().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(root.join("right/hello.txt"))?, "new target");
        assert_eq!(fs::read_to_string(source)?, "source");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn same_directory_is_not_allowed() -> io::Result<()> {
        let root = fixture();
        let source = root.join("left/a.txt");
        fs::write(&source, "hello")?;
        assert!(Transfer::new(TransferKind::Copy, vec![source], root.join("left")).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
