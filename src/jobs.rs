//! One filesystem job at a time, off the UI thread.
//!
//! Progress is read from atomics by the event loop, avoiding a flood of
//! messages for each copied chunk. The channel delivers the final result.
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{archive::ArchiveJob, fileops::{Transfer, TransferKind}, remote::{RemoteTransfer, RemoteSave}};

#[derive(Debug)]
pub struct Progress {
    bytes: AtomicU64,
    items: AtomicU64,
    total: AtomicU64,
    total_bytes: AtomicU64,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            bytes: AtomicU64::new(0),
            items: AtomicU64::new(0),
            total: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
        }
    }

    pub fn set_total(&self, total: usize) {
        self.total.store(total as u64, Ordering::Relaxed);
    }

    pub fn set_total_bytes(&self, bytes: u64) {
        self.total_bytes.store(bytes, Ordering::Relaxed);
    }

    pub fn item_done(&self) {
        self.items.fetch_add(1, Ordering::Relaxed);
    }

    pub fn bytes_done(&self, count: u64) {
        self.bytes.fetch_add(count, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            bytes: self.bytes.load(Ordering::Relaxed),
            items: self.items.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub bytes: u64,
    pub items: u64,
    pub total: u64,
    pub total_bytes: u64,
}

impl Snapshot {
    pub fn percent(self) -> Option<u64> {
        self.bytes
            .min(self.total_bytes)
            .saturating_mul(100)
            .checked_div(self.total_bytes)
            .or_else(|| {
                self.items
                    .min(self.total)
                    .saturating_mul(100)
                    .checked_div(self.total)
            })
    }
}

/// Count bytes as they are actually read, not while estimating the job.
pub struct TrackedReader<R> {
    inner: R,
    progress: Option<Arc<Progress>>,
}

impl<R> TrackedReader<R> {
    #[cfg(test)]
    pub fn new(inner: R, progress: &Arc<Progress>) -> Self {
        Self::optional(inner, Some(progress))
    }

    pub fn optional(inner: R, progress: Option<&Arc<Progress>>) -> Self {
        Self { inner, progress: progress.cloned() }
    }
}

impl<R: Read> Read for TrackedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buf)?;
        if let Some(progress) = &self.progress {
            progress.bytes_done(count as u64);
        }
        Ok(count)
    }
}

/// Count bytes written by compressors or file copies during a job.
pub struct TrackedWriter<W> {
    inner: W,
    progress: Option<Arc<Progress>>,
}

impl<W> TrackedWriter<W> {
    pub fn new(inner: W, progress: &Arc<Progress>) -> Self {
        Self::optional(inner, Some(progress))
    }

    pub fn optional(inner: W, progress: Option<&Arc<Progress>>) -> Self {
        Self { inner, progress: progress.cloned() }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for TrackedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        if let Some(progress) = &self.progress {
            progress.bytes_done(count as u64);
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub enum Work {
    Transfer(Transfer),
    Archive(ArchiveJob),
    Remote(RemoteTransfer),
    RemoteSave(RemoteSave),
}

impl Work {
    fn label(&self) -> &'static str {
        match self {
            Self::Transfer(transfer) => match transfer.kind {
                TransferKind::Copy => "Copying",
                TransferKind::Move => "Moving",
            },
            Self::Archive(ArchiveJob::Pack { .. }) => "Packing",
            Self::Archive(ArchiveJob::Unpack { .. }) => "Extracting",
            Self::Remote(job) => match job.kind {
                TransferKind::Copy => "SFTP copying",
                TransferKind::Move => "SFTP moving",
            },
            Self::RemoteSave(_) => "SFTP saving",
        }
    }

    /// Snapshot of the confirmed operation; independent of later pane navigation.
    pub fn description(&self) -> String {
        match self {
            Self::Transfer(transfer) => {
                let verb = transfer.kind.verb();
                let items = if transfer.sources.len() == 1 {
                    transfer.sources[0].file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "item".to_owned())
                } else {
                    format!("{} items", transfer.sources.len())
                };
                format!("{verb} {items} → {}", transfer.destination.display())
            }
            Self::Remote(job) => job.description(),
            Self::RemoteSave(job) => format!("Save remote · {}", job.target.display()),
            Self::Archive(archive) => match archive {
                ArchiveJob::Pack { sources, target } => format!(
                    "Pack {} item(s) → {}",
                    sources.len(),
                    target.file_name().unwrap_or_default().to_string_lossy()
                ),
                ArchiveJob::Unpack { source, target } => format!(
                    "Extract {} → {}",
                    source.file_name().unwrap_or_default().to_string_lossy(),
                    target.file_name().unwrap_or_default().to_string_lossy()
                ),
            }
        }
    }
}

pub enum Poll {
    Busy,
    Finished(io::Result<String>),
}

pub struct RunningJob {
    pub label: &'static str,
    pub description: String,
    pub progress: Arc<Progress>,
    pub started: Instant,
    receiver: Receiver<io::Result<String>>,
}

impl RunningJob {
    pub fn start(work: Work) -> io::Result<Self> {
        let progress = Arc::new(Progress::new());
        let worker_progress = Arc::clone(&progress);
        let label = work.label();
        let description = work.description();
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("rzpk-file-worker".to_owned())
            .spawn(move || {
                let result = match work {
                    Work::Transfer(transfer) => {
                        let kind = transfer.kind;
                        transfer.execute_with_progress(&worker_progress)
                            .map(|count| format!("{} {count} item(s)", kind.past_tense()))
                    }
                    Work::Archive(job) => job.execute_with_progress(&worker_progress),
                    Work::Remote(job) => job.execute(&worker_progress),
                    Work::RemoteSave(job) => job.execute(),
                };
                let _ = tx.send(result);
            })?;
        Ok(Self {
            label,
            description,
            progress,
            started: Instant::now(),
            receiver: rx,
        })
    }

    pub fn poll(&self) -> Poll {
        match self.receiver.try_recv() {
            Ok(result) => Poll::Finished(result),
            Err(TryRecvError::Empty) => Poll::Busy,
            Err(TryRecvError::Disconnected) => Poll::Finished(Err(io::Error::other("File worker stopped unexpectedly"))),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_snapshots_are_monotonic() {
        let progress = Arc::new(Progress::new());
        progress.set_total(5);
        progress.item_done();
        progress.bytes_done(4096);
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.percent(), Some(20));
        assert_eq!(snapshot.bytes, 4096);
    }

    #[test]
    fn byte_progress_takes_priority_over_completed_items() {
        let progress = Progress::new();
        progress.set_total(1);
        progress.set_total_bytes(1000);
        progress.bytes_done(250);
        assert_eq!(progress.snapshot().percent(), Some(25));
        progress.bytes_done(5000);
        assert_eq!(progress.snapshot().percent(), Some(100));
    }

    #[test]
    fn tracked_io_reports_bytes() -> io::Result<()> {
        let progress = Arc::new(Progress::new());
        let mut output = Vec::new();
        let input = io::Cursor::new(b"hello world".to_vec());
        let mut input = TrackedReader::new(input, &progress);
        io::copy(&mut input, &mut output)?;
        assert_eq!(output, b"hello world");
        assert_eq!(progress.snapshot().bytes, 11);
        let mut target = TrackedWriter::new(Vec::new(), &progress);
        target.write_all(b"world")?;
        assert_eq!(progress.snapshot().bytes, 16);
        Ok(())
    }
}
