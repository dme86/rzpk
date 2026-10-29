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
use crate::{Entry, jobs::Progress};

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
            let ssh = Session::connect_mux(&hostname, KnownHosts::Strict).await.map_err(other)?;
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
            let mut stream = dir.read_dir();
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

    pub fn home(&self) -> &Path { &self.root }
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
