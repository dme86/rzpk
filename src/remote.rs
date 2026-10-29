//! SFTP remote filesystem backed by the user's installed OpenSSH client.
//! Host-key checking is STRICT. ~/.ssh/config, ProxyJump, identities and
//! ssh-agent are handled by OpenSSH, not by rzpk. No passwords are persisted.
use std::{
    fs::{self, File, OpenOptions}, io::{self, Write}, path::{Path, PathBuf},
    sync::Arc,
};
use bytes::BytesMut;
use futures_util::StreamExt;
use openssh::{KnownHosts, Session};
use openssh_sftp_client::{Sftp, SftpOptions};
use tokio::runtime::{Builder, Runtime};
use crate::{Entry, jobs::Progress, fileops::TransferKind};

const MAX_LIST: usize = 2500;
pub const MAX_PREVIEW: usize = 24 * 1024;

pub struct Remote {
    pub name: String,
    pub root: PathBuf,
    rt: Runtime,
    sftp: Sftp,
}

#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: Option<u64>,
}

impl RemoteEntry {
    pub fn into_entry(self) -> Entry {
        Entry {
            path: self.path,
            name_key: self.name.to_lowercase(),
            name: self.name,
            is_dir: self.is_dir,
            is_symlink: self.is_symlink,
            permissions: "----------".to_owned(),
            size: self.size,
            modified: None,
        }
    }
}

fn other(error: impl std::fmt::Display) -> io::Error { io::Error::other(error.to_string()) }

pub fn validate_host(host: &str) -> io::Result<()> {
    // Accept OpenSSH host aliases, user@host, port via ~/.ssh/config.
    // Reject whitespace, shell metacharacters, and ssh flag injection.
    if host.is_empty() || host.starts_with('-') ||
        !host.chars().all(|ch| ch.is_ascii_alphanumeric() || ".-_@:".contains(ch))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid SSH host/alias"));
    }
    Ok(())
}

impl Remote {
    pub fn connect(name: String, hostname: String, root: PathBuf) -> io::Result<Arc<Self>> {
        validate_host(&hostname)?;
        if !root.is_absolute() && root.as_os_str() != "~" {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Remote root must be absolute or ~"));
        }
        let rt = Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
        let sftp = rt.block_on(async {
            let ssh = Session::connect(&hostname, KnownHosts::Strict).await.map_err(other)?;
            Sftp::from_session(ssh, SftpOptions::default()).await.map_err(other)
        })?;
        let remote = Arc::new(Self { name, root, rt, sftp });
        // Establish the initial directory rather than accepting an unusable connection.
        remote.list(&remote.root)?;
        Ok(remote)
    }

    pub fn list(&self, path: &Path) -> io::Result<Vec<RemoteEntry>> {
        let entries = self.rt.block_on(async {
            let mut filesystem = self.sftp.fs();
            let dir = filesystem.open_dir(path).await.map_err(other)?;
            let stream = dir.read_dir();
            tokio::pin!(stream);
            let mut rows = Vec::new();
            while let Some(entry) = stream.next().await {
                let entry = entry.map_err(other)?;
                let name = entry.filename().to_string_lossy().to_string();
                if name == "." || name == ".." { continue; }
                if rows.len() >= MAX_LIST {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Remote directory exceeds 2500 entries"));
                }
                // A malicious server cannot escape a pane via an entry name.
                if Path::new(&name).components().count() != 1 || name.contains('/') {
                    continue;
                }
                let kind = entry.file_type();
                rows.push(RemoteEntry {
                    path: path.join(&name),
                    name,
                    is_dir: kind.is_some_and(|kind| kind.is_dir()),
                    is_symlink: kind.is_some_and(|kind| kind.is_symlink()),
                    size: entry.metadata().len(),
                });
            }
            Ok::<_, io::Error>(rows)
        })?;
        let mut entries = entries;
        entries.sort_by_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
        Ok(entries)
    }

    /// Read *up to* a bounded prefix. SFTP read_all() requests exactly the
    /// specified byte count and returns an error at EOF for shorter files,
    /// which is unsuitable for previews and temporary editor downloads.
    fn read_prefix(&self, path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
        self.rt.block_on(async {
            let mut file = self.sftp.open(path).await.map_err(other)?;
            let mut bytes = Vec::new();
            while bytes.len() < max_bytes {
                let requested = (max_bytes - bytes.len()).min(64 * 1024) as u32;
                match file.read(requested, BytesMut::new()).await.map_err(other)? {
                    Some(chunk) => bytes.extend_from_slice(&chunk),
                    None => break,
                }
            }
            Ok(bytes)
        })
    }

    pub fn preview(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.read_prefix(path, MAX_PREVIEW + 1)
    }

    /// Copy one regular file from remote to local with exclusive destination.
    /// Symlinks and directories are validated by the caller.
    pub fn download(&self, source: &Path, target: &Path, progress: &Arc<Progress>) -> io::Result<()> {
        let mut output = OpenOptions::new().create_new(true).write(true).open(target)?;
        let result = self.rt.block_on(async {
            let mut file = self.sftp.open(source).await.map_err(other)?;
            loop {
                let bytes = file.read(64 * 1024, BytesMut::new()).await.map_err(other)?;
                let Some(bytes) = bytes else { break; };
                output.write_all(&bytes)?;
                progress.bytes_done(bytes.len() as u64);
            }
            Ok::<_, io::Error>(())
        });
        if result.is_err() { let _ = fs::remove_file(target); }
        result
    }

    pub fn upload(&self, source: &Path, target: &Path, progress: &Arc<Progress>) -> io::Result<()> {
        let mut input = File::open(source)?;
        self.rt.block_on(async {
            let mut file = self.sftp.options().write(true).create_new(true).open(target).await.map_err(other)?;
            let result = async {
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let n = std::io::Read::read(&mut input, &mut buffer)?;
                    if n == 0 { break; }
                    file.write_all(&buffer[..n]).await.map_err(other)?;
                    progress.bytes_done(n as u64);
                }
                file.close().await.map_err(other)
            }.await;
            if result.is_err() {
                let mut fs = self.sftp.fs();
                let _ = fs.remove_file(target).await;
            }
            result
        })
    }

    /// Same-host move and cleanup after cross-filesystem copying.
    pub fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.rt.block_on(async {
            self.sftp.fs().remove_file(path).await.map_err(other)
        })
    }

    pub fn edit_stamp(&self, path: &Path) -> io::Result<EditStamp> {
        self.rt.block_on(async {
            let meta = self.sftp.fs().symlink_metadata(path).await.map_err(other)?;
            if !meta.file_type().is_some_and(|kind| kind.is_file()) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Not a regular remote file"));
            }
            Ok(EditStamp { size: meta.len(), modified: format!("{:?}", meta.modified()) })
        })
    }

    pub fn edit_snapshot(&self, path: &Path) -> io::Result<(Vec<u8>, EditStamp)> {
        const MAX_EDIT: usize = 8 * 1024 * 1024;
        let stamp = self.edit_stamp(path)?;
        if stamp.size.is_some_and(|size| size > MAX_EDIT as u64) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Remote edit exceeds 8 MiB"));
        }
        let data = self.read_prefix(path, MAX_EDIT + 1)?;
        if data.len() > MAX_EDIT {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Remote edit exceeds 8 MiB"));
        }
        Ok((data, stamp))
    }

    pub fn save_edit(&self, local: &Path, remote_path: &Path, expected: &EditStamp) -> io::Result<()> {
        if self.edit_stamp(remote_path)? != *expected {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                "Remote file changed since editing began; refusing overwrite"));
        }
        let name = remote_path.file_name().ok_or_else(|| io::Error::other("Invalid remote filename"))?;
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let staging = remote_path.with_file_name(format!(
            ".{}.rzpk-{}-{suffix}.tmp", name.to_string_lossy(), std::process::id()));
        let progress = Arc::new(Progress::new());
        self.upload(local, &staging, &progress)?;
        let result = self.rt.block_on(async {
            let meta = self.sftp.fs().symlink_metadata(remote_path).await.map_err(other)?;
            let current = EditStamp { size: meta.len(), modified: format!("{:?}", meta.modified()) };
            if current != *expected {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "Remote file changed during upload"));
            }
            self.sftp.fs().rename(&staging, remote_path).await.map_err(other)
        });
        if result.is_err() { let _ = self.remove_file(&staging); }
        result
    }

    /// Exclusive SFTP empty-file creation; never truncate an existing file.
    pub fn create_empty_file(&self, path: &Path) -> io::Result<()> {
        self.rt.block_on(async {
            let file = self.sftp.options().write(true).create_new(true).open(path).await.map_err(other)?;
            file.close().await.map_err(other)
        })
    }

    pub fn mkdir(&self, path: &Path) -> io::Result<()> {
        self.rt.block_on(async { self.sftp.fs().create_dir(path).await.map_err(other) })
    }

    pub fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.rt.block_on(async { self.sftp.fs().remove_dir(path).await.map_err(other) })
    }

    pub fn stat_type(&self, path: &Path) -> io::Result<(bool, bool, Option<u64>)> {
        self.rt.block_on(async {
            let meta = self.sftp.fs().symlink_metadata(path).await.map_err(other)?;
            let kind = meta.file_type();
            if !kind.is_some_and(|t| t.is_file() || t.is_dir() || t.is_symlink()) {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "Unsupported remote file type"));
            }
            Ok((
                kind.is_some_and(|t| t.is_dir()),
                kind.is_some_and(|t| t.is_symlink()),
                meta.len(),
            ))
        })
    }

}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditStamp {
    pub size: Option<u64>,
    pub modified: String,
}

pub struct RemoteSave {
    pub remote: Arc<Remote>,
    pub target: PathBuf,
    pub local: PathBuf,
    pub tempdir: tempfile::TempDir,
    pub expected: EditStamp,
}

impl RemoteSave {
    pub fn execute(self) -> io::Result<String> {
        let result = self.remote.save_edit(&self.local, &self.target, &self.expected);
        match result {
            Ok(()) => Ok(format!("Saved remote file {}", self.target.display())),
            Err(error) => {
                let kept = self.tempdir.keep();
                Err(io::Error::other(format!("{error} · Edited copy preserved: {}", kept.display())))
            }
        }
    }
}

/// Snapshot of one local↔SFTP transfer. Sources are never interpreted as local
/// paths when the source is remote, or vice versa.
pub struct RemoteTransfer {
    pub kind: TransferKind,
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
    pub source_remote: Option<Arc<Remote>>,
    pub dest_remote: Option<Arc<Remote>>,
}

impl RemoteTransfer {
    pub fn new(
        kind: TransferKind, sources: Vec<PathBuf>, destination: PathBuf,
        source_remote: Option<Arc<Remote>>, dest_remote: Option<Arc<Remote>>,
    ) -> io::Result<Self> {
        if sources.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "No files selected"));
        }
        if source_remote.is_some() == dest_remote.is_some() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "Use one local and one SFTP pane"));
        }
        if sources.iter().any(|p| p.file_name().is_none()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Cannot transfer a filesystem root"));
        }
        Ok(Self { kind, sources, destination, source_remote, dest_remote })
    }

    fn file_kind(&self, path: &Path) -> io::Result<(bool, bool, Option<u64>)> {
        if let Some(remote) = &self.source_remote {
            remote.stat_type(path)
        } else {
            let meta = fs::symlink_metadata(path)?;
            if !meta.is_dir() && !meta.is_file() && !meta.file_type().is_symlink() {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "Unsupported local file type"));
            }
            Ok((meta.is_dir(), meta.file_type().is_symlink(), Some(meta.len())))
        }
    }

    fn children(&self, directory: &Path) -> io::Result<Vec<PathBuf>> {
        if let Some(remote) = &self.source_remote {
            Ok(remote.list(directory)?.into_iter().map(|entry| entry.path).collect())
        } else {
            fs::read_dir(directory)?.map(|entry| entry.map(|entry| entry.path())).collect()
        }
    }

    fn copy_file(&self, source: &Path, target: &Path, progress: &Arc<Progress>) -> io::Result<()> {
        if let Some(remote) = &self.source_remote {
            remote.download(source, target, progress)
        } else if let Some(remote) = &self.dest_remote {
            remote.upload(source, target, progress)
        } else {
            Err(io::Error::other("Transfer backend unavailable"))
        }
    }

    fn mkdir(&self, target: &Path) -> io::Result<()> {
        if let Some(remote) = &self.dest_remote { remote.mkdir(target) }
        else { fs::create_dir(target) }
    }

    fn remove_source_file(&self, path: &Path) -> io::Result<()> {
        if let Some(remote) = &self.source_remote { remote.remove_file(path) }
        else { fs::remove_file(path) }
    }

    fn remove_source_dir(&self, path: &Path) -> io::Result<()> {
        if let Some(remote) = &self.source_remote { remote.rmdir(path) }
        else { fs::remove_dir(path) }
    }

    pub fn execute(&self, progress: &Arc<Progress>) -> io::Result<String> {
        const MAX_ENTRIES: usize = 20_000;
        const MAX_DEPTH: usize = 64;
        progress.set_total(self.sources.len());
        let mut copied = 0usize;
        for source in &self.sources {
            let name = source.file_name().ok_or_else(|| io::Error::other("Missing source filename"))?;
            let target = self.destination.join(name);
            let mut stack = vec![(source.clone(), target, 0usize)];
            let mut copied_files = Vec::new();
            let mut copied_dirs = Vec::new();
            let mut visited = 0usize;
            while let Some((src, dst, depth)) = stack.pop() {
                visited += 1;
                if visited > MAX_ENTRIES || depth > MAX_DEPTH {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Transfer exceeds directory safety limits"));
                }
                let (is_dir, symlink, size) = self.file_kind(&src)?;
                if symlink {
                    return Err(io::Error::new(io::ErrorKind::Unsupported,
                        format!("SFTP transfers do not follow symbolic links: {}", src.display())));
                }
                if is_dir {
                    // create_dir must fail if the destination already exists.
                    self.mkdir(&dst)?;
                    copied_dirs.push(src.clone());
                    let children = self.children(&src)?;
                    for child in children.into_iter().rev() {
                        let filename = child.file_name().ok_or_else(|| io::Error::other("Invalid child path"))?.to_os_string();
                        stack.push((child, dst.join(filename), depth + 1));
                    }
                } else {
                    self.copy_file(&src, &dst, progress)?;
                    copied_files.push(src);
                    if size == Some(0) { /* valid empty file */ }
                }
            }
            if self.kind == TransferKind::Move {
                // Only remove source entries after the entire root has copied.
                for path in copied_files { self.remove_source_file(&path)?; }
                for path in copied_dirs.into_iter().rev() { self.remove_source_dir(&path)?; }
            }
            copied += 1;
            progress.item_done();
        }
        Ok(format!("{} {copied} item(s) via SFTP", self.kind.past_tense()))
    }

    pub fn description(&self) -> String {
        let first = self.sources.first().and_then(|p| p.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "files".to_owned());
        let host = self.source_remote.as_ref().or(self.dest_remote.as_ref())
            .map(|remote| remote.name.as_str()).unwrap_or("remote");
        format!("{} {} item(s) · {} · {first}", self.kind.verb(), self.sources.len(), host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sftp_transfer_refuses_invalid_backend_pairs_and_empty_sources() {
        let path = PathBuf::from("/tmp");
        assert!(RemoteTransfer::new(TransferKind::Copy, vec![], path.clone(), None, None).is_err());
        assert!(RemoteTransfer::new(TransferKind::Copy, vec![PathBuf::from("/tmp/f")], path.clone(), None, None).is_err());
        assert!(RemoteTransfer::new(TransferKind::Move, vec![PathBuf::from("/")], path, None, None).is_err());
    }

    /// GitHub Actions supplies a loopback OpenSSH test server. On developer
    /// machines without that environment variable this remains an offline test.
    #[test]
    fn real_sftp_roundtrip_when_test_server_is_available() -> io::Result<()> {
        let Ok(host) = std::env::var("RZPK_TEST_SFTP_HOST") else { return Ok(()); };
        let directory = PathBuf::from(std::env::var("RZPK_TEST_SFTP_DIR")
            .map_err(io::Error::other)?);
        let remote = Remote::connect("ci".to_owned(), host, directory.clone())?;
        let workspace = tempfile::tempdir()?;
        let source = workspace.path().join("nested");
        fs::create_dir(&source)?;
        fs::create_dir(source.join("deeper"))?;
        fs::write(source.join("hello.txt"), "roundtrip text")?;
        fs::write(source.join("deeper/other.txt"), "nested file")?;
        let remote_folder = directory.join(format!("rzpk-ci-{}", std::process::id()));
        remote.mkdir(&remote_folder)?;
        let uploaded = RemoteTransfer::new(TransferKind::Copy, vec![source],
            remote_folder.clone(), None, Some(remote.clone()))?;
        let progress = Arc::new(Progress::new());
        uploaded.execute(&progress)?;
        let list = remote.list(&remote_folder.join("nested"))?;
        assert!(list.iter().any(|entry| entry.name == "hello.txt"));
        assert!(list.iter().any(|entry| entry.name == "deeper" && entry.is_dir));
        let downloaded = workspace.path().join("downloaded");
        fs::create_dir(&downloaded)?;
        let download = RemoteTransfer::new(TransferKind::Copy,
            vec![remote_folder.join("nested")], downloaded.clone(),
            Some(remote.clone()), None)?;
        download.execute(&Arc::new(Progress::new()))?;
        assert_eq!(fs::read_to_string(downloaded.join("nested/hello.txt"))?, "roundtrip text");
        assert_eq!(fs::read_to_string(downloaded.join("nested/deeper/other.txt"))?, "nested file");

        // Remote editing must upload changes and reject stale snapshots.
        let editable = remote_folder.join("nested/hello.txt");
        let (original, stamp) = remote.edit_snapshot(&editable)
            .map_err(|error| io::Error::other(format!("edit_snapshot: {error}")))?;
        assert_eq!(original, b"roundtrip text");
        let working_copy = workspace.path().join("edited.txt");
        fs::write(&working_copy, b"remote file changed and saved")?;
        remote.save_edit(&working_copy, &editable, &stamp)
            .map_err(|error| io::Error::other(format!("save_edit: {error}")))?;
        assert_eq!(remote.preview(&editable)?, b"remote file changed and saved");
        assert!(remote.save_edit(&working_copy, &editable, &stamp).is_err());

        // Move in both directions, validating that the source is removed only
        // after a successful transfer.
        let movable = workspace.path().join("move-me.txt");
        fs::write(&movable, b"move via sftp")?;
        let upload_move = RemoteTransfer::new(TransferKind::Move, vec![movable.clone()],
            remote_folder.clone(), None, Some(remote.clone()))?;
        upload_move.execute(&Arc::new(Progress::new()))
            .map_err(|error| io::Error::other(format!("upload_move: {error}")))?;
        assert!(!movable.exists());
        let moved_remote = remote_folder.join("move-me.txt");
        assert!(remote.list(&remote_folder)?.iter().any(|entry| entry.name == "move-me.txt"));
        let download_move = RemoteTransfer::new(TransferKind::Move,
            vec![moved_remote], downloaded.clone(), Some(remote.clone()), None)?;
        download_move.execute(&Arc::new(Progress::new()))
            .map_err(|error| io::Error::other(format!("download_move: {error}")))?;
        assert_eq!(fs::read(downloaded.join("move-me.txt"))?, b"move via sftp");
        assert!(!remote.list(&remote_folder)?.iter().any(|entry| entry.name == "move-me.txt"));

        // Create a nested SFTP project tree and an empty file without overwriting.
        let created = crate::create::CreatePlan::build(remote_folder.clone(), "new-project",
            crate::create::Kind::Folder, "src/main, docs, assets",
            Some(remote.clone()))?;
        created.execute(&Arc::new(Progress::new()))?;
        assert!(remote.list(&remote_folder.join("new-project"))?
            .iter().any(|entry| entry.name == "src" && entry.is_dir));
        let blank = crate::create::CreatePlan::build(remote_folder.join("new-project"),
            "README.md", crate::create::Kind::File, "", Some(remote.clone()))?;
        blank.execute(&Arc::new(Progress::new()))?;
        assert_eq!(remote.preview(&remote_folder.join("new-project/README.md"))?, b"");
        assert!(blank.execute(&Arc::new(Progress::new())).is_err());
        remote.remove_file(&remote_folder.join("new-project/README.md"))?;
        for path in ["src/main", "assets", "docs", "src", ""] {
            remote.rmdir(&remote_folder.join("new-project").join(path))?;
        }

        remote.remove_file(&remote_folder.join("nested/deeper/other.txt"))?;
        remote.remove_file(&remote_folder.join("nested/hello.txt"))?;
        remote.rmdir(&remote_folder.join("nested/deeper"))?;
        remote.rmdir(&remote_folder.join("nested"))?;
        remote.rmdir(&remote_folder)?;
        Ok(())
    }

    #[test]
    fn host_validation_rejects_command_injection_and_option_prefix() {
        for good in ["nas", "host.example.com", "deploy@server", "192.168.1.10"] {
            assert!(validate_host(good).is_ok());
        }
        for bad in ["-oProxyCommand=evil", "host;rm -rf /", "server name", "", "$(evil)"] {
            assert!(validate_host(bad).is_err());
        }
    }
}
