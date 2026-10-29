//! Create an empty regular file or a directory with optional nested folders.
//! Plans are immutable snapshots so they can safely wait in the FIFO queue.
//! Every leaf is validated: relative paths cannot escape the new root.
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::{jobs::Progress, remote::Remote};

const MAX_CHILDREN: usize = 48;
const MAX_DEPTH: usize = 8;
const MAX_NAME_BYTES: usize = 255;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Kind {
    File,
    Folder,
}

pub struct CreatePlan {
    pub base: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub children: Vec<PathBuf>,
    pub remote: Option<Arc<Remote>>,
}

fn invalid(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason.into())
}

fn validate_segment(segment: &str) -> io::Result<()> {
    if segment.is_empty() || segment == "." || segment == ".." ||
        segment.as_bytes().len() > MAX_NAME_BYTES ||
        segment.trim() != segment ||
        segment.contains('\\') ||
        segment.chars().any(char::is_control)
    {
        return Err(invalid(format!("Invalid name: {segment:?}")));
    }
    Ok(())
}

fn parse_children(text: &str) -> io::Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    for item in text.split(',') {
        let item = item.trim();
        if item.is_empty() || item.starts_with('/') || item.ends_with('/') || item.contains("//") {
            return Err(invalid("Child folders must be relative paths, separated by commas"));
        }
        for part in item.split('/') { validate_segment(part)?; }
        let path = PathBuf::from(item);
        if path.components().count() > MAX_DEPTH ||
            !path.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(invalid("Child paths may have at most 8 levels and cannot use . or .."));
        }
        if !paths.insert(path) {
            return Err(invalid(format!("Duplicate child folder: {item}")));
        }
        if paths.len() > MAX_CHILDREN {
            return Err(invalid("At most 48 child folders are supported"));
        }
    }
    // Auto-create implicit parents; order them before their children.
    let mut all = BTreeSet::new();
    for path in paths {
        let mut parent = PathBuf::new();
        for part in path.components() {
            parent.push(part.as_os_str());
            all.insert(parent.clone());
        }
    }
    let mut ordered = all.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.components().count().cmp(&b.components().count()).then(a.cmp(b)));
    Ok(ordered)
}

impl CreatePlan {
    pub fn build(
        base: PathBuf, name: &str, kind: Kind, children: &str,
        remote: Option<Arc<Remote>>,
    ) -> io::Result<Self> {
        if !base.is_absolute() {
            return Err(invalid("Destination directory must be absolute"));
        }
        validate_segment(name)?;
        if name.contains('/') {
            return Err(invalid("Enter a single filename or folder name"));
        }
        if kind == Kind::File && !children.trim().is_empty() {
            return Err(invalid("Only folders can have subfolders"));
        }
        let children = if kind == Kind::Folder { parse_children(children)? } else { Vec::new() };
        let destination = base.join(name);
        // Local preflight: execution repeats the check with atomic create_new/mkdir.
        if remote.is_none() && fs::symlink_metadata(&destination).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                format!("Already exists: {}", destination.display())));
        }
        Ok(Self { base, name: name.to_owned(), kind, children, remote })
    }

    pub fn target(&self) -> PathBuf { self.base.join(&self.name) }

    pub fn description(&self) -> String {
        match self.kind {
            Kind::File => format!("Create file {}", self.name),
            Kind::Folder => format!("Create folder {} ({} subfolders)", self.name, self.children.len()),
        }
    }

    fn mkdir(&self, path: &Path) -> io::Result<()> {
        match &self.remote {
            Some(remote) => remote.mkdir(path),
            None => fs::create_dir(path),
        }
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        match &self.remote {
            Some(remote) => remote.rmdir(path),
            None => fs::remove_dir(path),
        }
    }

    fn touch(&self, path: &Path) -> io::Result<()> {
        match &self.remote {
            Some(remote) => remote.create_empty_file(path),
            None => {
                OpenOptions::new().write(true).create_new(true).open(path)?;
                Ok(())
            }
        }
    }

    pub fn execute(&self, progress: &Arc<Progress>) -> io::Result<String> {
        let target = self.target();
        progress.set_total(if self.kind == Kind::File { 1 } else { 1 + self.children.len() });
        if self.kind == Kind::File {
            self.touch(&target)?;
            progress.item_done();
            return Ok(format!("Created file {}", self.name));
        }
        // Create the root exclusively before touching any children.
        self.mkdir(&target)?;
        progress.item_done();
        let mut created = vec![target.clone()];
        for relative in &self.children {
            let subfolder = target.join(relative);
            if let Err(error) = self.mkdir(&subfolder) {
                // Roll back the directories we created, but never remove
                // anything added independently by the user or another process.
                let mut rollback_failed = false;
                for directory in created.iter().rev() {
                    if self.rmdir(directory).is_err() { rollback_failed = true; }
                }
                let message = if rollback_failed {
                    format!("{error}; some newly created directories could not be rolled back")
                } else {
                    format!("{error}; newly created empty directories rolled back")
                };
                return Err(io::Error::other(message));
            }
            created.push(subfolder);
            progress.item_done();
        }
        Ok(format!("Created {} with {} subfolders", self.name, self.children.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn folder() -> io::Result<tempfile::TempDir> { tempfile::tempdir() }

    #[test]
    fn makes_empty_file_and_rejects_overwrite() -> io::Result<()> {
        let dir = folder()?;
        let plan = CreatePlan::build(dir.path().to_path_buf(), "notes.txt", Kind::File, "", None)?;
        plan.execute(&Arc::new(Progress::new()))?;
        assert_eq!(fs::read(dir.path().join("notes.txt"))?, b"");
        assert!(plan.execute(&Arc::new(Progress::new())).is_err());
        assert!(CreatePlan::build(dir.path().to_path_buf(), "notes.txt", Kind::File, "", None).is_err());
        Ok(())
    }

    #[test]
    fn creates_parent_and_nested_children_without_duplicates() -> io::Result<()> {
        let dir = folder()?;
        let plan = CreatePlan::build(dir.path().to_path_buf(), "Urlaub China",
            Kind::Folder, "Fotos/Originale, Videos, Fotos/Bearbeitet, Dokumente", None)?;
        assert_eq!(plan.children.len(), 6);
        plan.execute(&Arc::new(Progress::new()))?;
        for name in ["Fotos", "Fotos/Originale", "Fotos/Bearbeitet", "Videos", "Dokumente"] {
            assert!(dir.path().join("Urlaub China").join(name).is_dir());
        }
        assert!(plan.execute(&Arc::new(Progress::new())).is_err());
        Ok(())
    }

    #[test]
    fn rejects_unsafe_children_before_any_file_creation() -> io::Result<()> {
        let dir = folder()?;
        for path in ["../escaped", "/absolute", "foo//bar", "x/../../y", "a,,b", "./folder", "folder/", "foo\\bar", "a,a"] {
            assert!(CreatePlan::build(dir.path().to_path_buf(), "safe", Kind::Folder, path, None).is_err(), "{path}");
            assert!(!dir.path().join("safe").exists());
        }
        for name in ["", "..", ".", "a/b", "a\\b", "foo\nbar"] {
            assert!(CreatePlan::build(dir.path().to_path_buf(), name, Kind::Folder, "", None).is_err());
        }
        assert!(CreatePlan::build(dir.path().to_path_buf(), "file", Kind::File, "sub", None).is_err());
        Ok(())
    }

    #[test]
    fn queued_job_rechecks_destination_atomically() -> io::Result<()> {
        let dir = folder()?;
        let plan = CreatePlan::build(dir.path().to_path_buf(), "project",
            Kind::Folder, "src, tests", None)?;
        fs::write(dir.path().join("project"), "existing")?;
        assert!(plan.execute(&Arc::new(Progress::new())).is_err());
        assert_eq!(fs::read_to_string(dir.path().join("project"))?, "existing");
        Ok(())
    }

    #[test]
    fn unicode_and_implicit_ancestors_are_supported() -> io::Result<()> {
        let dir = folder()?;
        let plan = CreatePlan::build(dir.path().to_path_buf(), "旅行",
            Kind::Folder, "Fotos/RAW/2026, Notizen", None)?;
        assert_eq!(plan.children.len(), 4);
        plan.execute(&Arc::new(Progress::new()))?;
        assert!(dir.path().join("旅行/Fotos/RAW/2026").is_dir());
        Ok(())
    }
}
