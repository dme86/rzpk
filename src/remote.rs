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
    pub hostname: String,
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
        let remote = Arc::new(Self { name, hostname, root, rt, sftp });
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
        entries.sort_by(|a, b| (!a.is_dir, a.name.to_lowercase()).cmp(&(!b.is_dir, b.name.to_lowercase())));
        Ok(entries)
    }

    pub fn preview(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.rt.block_on(async {
            let mut filesystem = self.sftp.fs();
            let metadata = filesystem.metadata(path).await.map_err(other)?;
            if metadata.len().is_some_and(|len| len > MAX_PREVIEW as u64) {
                // Still allow the first MAX_PREVIEW bytes.
            }
            let mut file = self.sftp.open(path).await.map_err(other)?;
            let bytes = file.read_all(MAX_PREVIEW + 1, BytesMut::new()).await.map_err(other)?;
            Ok(bytes.to_vec())
        })
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

    pub fn exists(&self, path: &Path) -> io::Result<bool> {
        self.rt.block_on(async {
            match self.sftp.fs().symlink_metadata(path).await {
                Ok(_) => Ok(true),
                Err(error) => {
                    // Avoid treating permission/connection failures as "missing".
                    // SFTP errors are not a std::io::ErrorKind, so conservatively
                    // let create_new() enforce no overwrite on upload.
                    let message = error.to_string();
                    if message.contains("No such file") || message.contains("SSH_FX_NO_SUCH_FILE") {
                        Ok(false)
                    } else {
                        Err(other(error))
                    }
                }
            }
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
            Ok((
                kind.is_some_and(|t| t.is_dir()),
                kind.is_some_and(|t| t.is_symlink()),
                meta.len(),
            ))
        })
    }

    pub fn home(&self) -> &Path { &self.root }
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
    fn host_validation_rejects_command_injection_and_option_prefix() {
        for good in ["nas", "host.example.com", "deploy@server", "192.168.1.10"] {
            assert!(validate_host(good).is_ok());
        }
        for bad in ["-oProxyCommand=evil", "host;rm -rf /", "server name", "", "$(evil)"] {
            assert!(validate_host(bad).is_err());
        }
    }
}
