//! Safe, deterministic mass renaming for regular files in one local folder.
//! Targets are preflighted and created with hard_link() (no-overwrite even if
//! another process creates a target in between checks), then source is unlinked.
//! No file data is copied and existing paths are never replaced.
use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::jobs::Progress;

pub const MAX_FILES: usize = 5000;
pub const MAX_PREFIX_CHARS: usize = 100;

#[derive(Debug, Clone)]
pub struct RenamePair {
    pub source: PathBuf,
    pub destination: PathBuf,
}

pub struct BatchRename {
    pub pairs: Vec<RenamePair>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub fn validate_prefix(prefix: &str) -> io::Result<()> {
    if prefix.is_empty() || prefix.trim() != prefix
        || prefix.chars().count() > MAX_PREFIX_CHARS
        || prefix == "." || prefix == ".."
        || prefix.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(invalid("Name must be 1–100 characters, without slashes, colons, whitespace at edges or control characters"));
    }
    Ok(())
}

pub fn proposed_name(source: &Path, prefix: &str, number: u32, digits: usize) -> OsString {
    let suffix = source.extension().map(|ext| {
        let mut s = OsString::from(".");
        s.push(ext);
        s
    }).unwrap_or_default();
    let mut name = OsString::from(format!("{prefix}_{number:0digits$}"));
    name.push(suffix);
    name
}

impl BatchRename {
    pub fn build(mut sources: Vec<PathBuf>, prefix: &str, start: u32, digits: usize) -> io::Result<Self> {
        validate_prefix(prefix)?;
        if sources.is_empty() || sources.len() > MAX_FILES {
            return Err(invalid("Select 1–5000 regular files"));
        }
        if !(1..=8).contains(&digits) {
            return Err(invalid("Digits must be between 1 and 8"));
        }
        if start == 0 || start.checked_add(sources.len() as u32 - 1).is_none() {
            return Err(invalid("Start number must be positive and within range"));
        }
        sources.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
        let folder = sources[0].parent().ok_or_else(|| invalid("Missing parent directory"))?.to_path_buf();
        let mut seen_src = HashSet::new();
        let mut seen_dst = HashSet::new();
        let mut pairs = Vec::with_capacity(sources.len());
        for (position, source) in sources.into_iter().enumerate() {
            if source.parent() != Some(folder.as_path()) || !seen_src.insert(source.clone()) {
                return Err(invalid("All source files must be distinct files in one directory"));
            }
            let file_type = fs::symlink_metadata(&source)?.file_type();
            if !file_type.is_file() {
                return Err(invalid(format!("Not a regular file: {}", source.display())));
            }
            let number = start + position as u32;
            let destination = folder.join(proposed_name(&source, prefix, number, digits));
            if !seen_dst.insert(destination.clone()) {
                return Err(invalid(format!("Duplicate destination: {}", destination.display())));
            }
            pairs.push(RenamePair { source, destination });
        }
        // Even existing source files are protected: the batch must not
        // quietly shuffle/overwrite other selected files.
        for pair in &pairs {
            if pair.destination != pair.source && fs::symlink_metadata(&pair.destination).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                    format!("Target already exists: {}", pair.destination.display())));
            }
        }
        Ok(Self { pairs })
    }

    pub fn execute(&self, progress: &Arc<Progress>) -> io::Result<String> {
        progress.set_total(self.pairs.len());
        let mut changed = 0;
        // Jobs can wait in the FIFO queue: revalidate all sources and targets
        // immediately before modifying anything.
        for pair in &self.pairs {
            if !fs::symlink_metadata(&pair.source)?.file_type().is_file() {
                return Err(invalid(format!("Source changed type: {}", pair.source.display())));
            }
            if pair.source != pair.destination && fs::symlink_metadata(&pair.destination).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                    format!("Target appeared: {}", pair.destination.display())));
            }
        }
        for pair in &self.pairs {
            if pair.source == pair.destination {
                progress.item_done();
                continue;
            }
            if let Err(error) = fs::hard_link(&pair.source, &pair.destination) {
                return Err(io::Error::other(format!(
                    "Renamed {changed} of {} files; failed to create {}: {error}",
                    self.pairs.len(), pair.destination.display()
                )));
            }
            if let Err(error) = fs::remove_file(&pair.source) {
                let _ = fs::remove_file(&pair.destination);
                return Err(io::Error::other(format!(
                    "Renamed {changed} of {} files; failed to remove source {}: {error}",
                    self.pairs.len(), pair.source.display()
                )));
            }
            changed += 1;
            progress.item_done();
        }
        Ok(format!("Renamed {changed} file(s)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn files(names: &[&str]) -> io::Result<(tempfile::TempDir, Vec<PathBuf>)> {
        let dir = tempfile::tempdir()?;
        let sources = names.iter().map(|n| {
            let file = dir.path().join(n);
            fs::write(&file, n)?;
            Ok(file)
        }).collect::<io::Result<Vec<_>>>()?;
        Ok((dir, sources))
    }

    #[test]
    fn numbered_jpgs_preserve_extension_and_sort_names() -> io::Result<()> {
        let (dir, sources) = files(&["IMG_003.JPG", "IMG_001.jpg", "IMG_002.png"])?;
        let plan = BatchRename::build(sources, "Urlaub_in_China", 1, 2)?;
        assert_eq!(plan.pairs.iter().map(|p| p.destination.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>(),
            ["Urlaub_in_China_01.jpg", "Urlaub_in_China_02.png", "Urlaub_in_China_03.JPG"]);
        let progress = Arc::new(Progress::new());
        plan.execute(&progress)?;
        assert_eq!(fs::read_to_string(dir.path().join("Urlaub_in_China_01.jpg"))?, "IMG_001.jpg");
        assert_eq!(fs::read_to_string(dir.path().join("Urlaub_in_China_03.JPG"))?, "IMG_003.JPG");
        assert_eq!(progress.snapshot().items, 3);
        Ok(())
    }

    #[test]
    fn preserves_all_originals_if_target_exists() -> io::Result<()> {
        let (dir, sources) = files(&["IMG_01.jpg", "IMG_02.jpg", "Holiday_02.jpg"])?;
        let plan = BatchRename::build(sources[..2].to_vec(), "Holiday", 1, 2);
        assert!(plan.is_err());
        assert_eq!(fs::read_to_string(dir.path().join("IMG_01.jpg"))?, "IMG_01.jpg");
        Ok(())
    }

    #[test]
    fn rejects_paths_and_symlinks_and_duplicate_sources() -> io::Result<()> {
        let (_dir, sources) = files(&["file.jpg"])?;
        assert!(BatchRename::build(sources.clone(), "../escape", 1, 2).is_err());
        assert!(BatchRename::build(sources.clone(), "A/B", 1, 2).is_err());
        assert!(BatchRename::build(vec![sources[0].clone(), sources[0].clone()], "test", 1, 2).is_err());
        #[cfg(unix)] {
            use std::os::unix::fs::symlink;
            let alias = sources[0].with_file_name("alias.jpg");
            symlink(&sources[0], &alias)?;
            assert!(BatchRename::build(vec![alias], "test", 1, 2).is_err());
        }
        Ok(())
    }

    #[test]
    fn rechecks_targets_when_queued_job_runs() -> io::Result<()> {
        let (dir, sources) = files(&["one.jpg", "two.jpg"])?;
        let plan = BatchRename::build(sources, "Holiday", 1, 2)?;
        fs::write(dir.path().join("Holiday_02.jpg"), "someone else's file")?;
        assert!(plan.execute(&Arc::new(Progress::new())).is_err());
        assert!(dir.path().join("one.jpg").exists());
        assert!(dir.path().join("two.jpg").exists());
        Ok(())
    }

    #[test]
    fn handles_unicode_no_extension_and_custom_start() -> io::Result<()> {
        let (dir, sources) = files(&["爱你.jpg", "README"])?;
        let plan = BatchRename::build(sources, "Urlaub_中国", 42, 4)?;
        plan.execute(&Arc::new(Progress::new()))?;
        assert!(dir.path().join("Urlaub_中国_0042").exists());
        assert!(dir.path().join("Urlaub_中国_0043.jpg").exists());
        Ok(())
    }
}
