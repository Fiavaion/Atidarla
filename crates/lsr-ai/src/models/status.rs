//! Whether a model is in the cache, full verification, and background jobs.

use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use sha2::{Digest, Sha256};

use super::catalogue::Model;
use super::paths::Store;

/// A model's state in the cache, from file sizes alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// None of its files (nor partial downloads) are there.
    NotDownloaded,
    /// Some files are there or partly downloaded; the rest are missing.
    Partial,
    /// Every file is there with the catalogue size. Run [`verify`] for the
    /// SHA-256 check.
    Ready,
    /// A file has the wrong size (or [`verify`] found a wrong hash).
    Corrupt,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotDownloaded => "not downloaded",
            Self::Partial => "partial",
            Self::Ready => "ready",
            Self::Corrupt => "corrupt",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn size_of(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

/// The model's status from file sizes (fast: metadata only).
pub fn status(store: &Store, model: &Model) -> Status {
    let (mut ok, mut some) = (0, false);
    for f in &model.files {
        match size_of(&store.file(&model.id, &f.path)) {
            Some(n) if n == f.size => ok += 1,
            Some(_) => return Status::Corrupt,
            None => some |= size_of(&store.part(&model.id, &f.path)).is_some(),
        }
    }
    if ok == model.files.len() {
        Status::Ready
    } else if ok > 0 || some {
        Status::Partial
    } else {
        Status::NotDownloaded
    }
}

/// Progress of a [`Job`], shared with the thread doing it.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
    file: Mutex<String>,
}

/// A copy of [`Progress`] for the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub done: u64,
    pub total: u64,
    /// The file being worked on.
    pub file: String,
}

impl ProgressSnapshot {
    /// 0 to 1.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).min(1.0) as f32
        }
    }
}

impl Progress {
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            done: self.done.load(Ordering::Acquire),
            total: self.total.load(Ordering::Acquire),
            file: self.file.lock().map(|f| f.clone()).unwrap_or_default(),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    pub(crate) fn set_total(&self, n: u64) {
        self.total.store(n, Ordering::Release);
    }

    pub(crate) fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::AcqRel);
    }

    pub(crate) fn sub(&self, n: u64) {
        self.done.fetch_sub(n, Ordering::AcqRel);
    }

    pub(crate) fn set_file(&self, f: &str) {
        if let Ok(mut s) = self.file.lock() {
            f.clone_into(&mut s);
        }
    }
}

/// Work on its own thread (a download or a verification). Poll it from the
/// UI; it never blocks the caller unless [`Job::wait`] is called.
#[derive(Debug)]
pub struct Job<T> {
    progress: Arc<Progress>,
    handle: Option<JoinHandle<T>>,
}

impl<T: Send + 'static> Job<T> {
    /// Runs `work` on a new named thread.
    pub fn spawn(name: &str, work: impl FnOnce(&Progress) -> T + Send + 'static) -> Self {
        let progress = Arc::new(Progress::default());
        let p = progress.clone();
        let handle = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || work(&p))
            .expect("spawn a model-manager thread");
        Self {
            progress,
            handle: Some(handle),
        }
    }

    pub fn progress(&self) -> ProgressSnapshot {
        self.progress.snapshot()
    }

    /// Asks the job to stop at its next chunk. A cancelled download keeps
    /// its `.part` files, so it resumes later.
    pub fn cancel(&self) {
        self.progress.cancel();
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// The result once the job has finished, else `None` (without waiting).
    /// Returns `Some` once.
    pub fn try_take(&mut self) -> Option<std::thread::Result<T>> {
        if self.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            self.handle.take().map(JoinHandle::join)
        } else {
            None
        }
    }

    /// Waits for the result (CLI and tests; never on the render thread).
    pub fn wait(mut self) -> std::thread::Result<T> {
        self.handle.take().expect("job result already taken").join()
    }
}

/// A file's verification result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Ok,
    Missing,
    WrongSize { size: u64 },
    WrongHash { sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCheck {
    pub path: String,
    pub expected_sha256: String,
    pub state: FileState,
}

/// Every file's SHA-256 compared with the catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub id: String,
    pub files: Vec<FileCheck>,
    /// The check was cancelled before every file was read.
    pub cancelled: bool,
}

impl VerifyReport {
    /// Ready, Corrupt, Partial or NotDownloaded, now with hashes checked.
    pub fn status(&self) -> Status {
        let missing = self
            .files
            .iter()
            .filter(|f| f.state == FileState::Missing)
            .count();
        if self.files.iter().any(|f| {
            matches!(
                f.state,
                FileState::WrongSize { .. } | FileState::WrongHash { .. }
            )
        }) {
            Status::Corrupt
        } else if missing == 0 && !self.cancelled {
            Status::Ready
        } else if missing == self.files.len() {
            Status::NotDownloaded
        } else {
            Status::Partial
        }
    }
}

/// SHA-256 of a reader, reporting bytes to `progress` and stopping (with
/// `None`) when it is cancelled.
pub(crate) fn hash_reader(
    mut r: impl Read,
    hasher: &mut Sha256,
    progress: &Progress,
    buf: &mut [u8],
) -> std::io::Result<Option<u64>> {
    let mut n_total = 0;
    loop {
        if progress.cancelled() {
            return Ok(None);
        }
        let n = r.read(buf)?;
        if n == 0 {
            return Ok(Some(n_total));
        }
        hasher.update(&buf[..n]);
        n_total += n as u64;
        progress.add(n as u64);
    }
}

pub(crate) fn hex(digest: &[u8]) -> String {
    use std::fmt::Write;
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Reads every file of `model` and compares sizes and SHA-256 with the
/// catalogue. Blocking: run it with [`start_verify`] from the UI.
pub fn verify(store: &Store, model: &Model, progress: &Progress) -> VerifyReport {
    progress.set_total(model.total_size());
    let mut buf = vec![0u8; 1 << 20];
    let mut files = Vec::with_capacity(model.files.len());
    let mut cancelled = false;
    for f in &model.files {
        progress.set_file(&f.path);
        let path = store.file(&model.id, &f.path);
        let state = match size_of(&path) {
            None => FileState::Missing,
            Some(size) if size != f.size => FileState::WrongSize { size },
            Some(_) => {
                let mut hasher = Sha256::new();
                match std::fs::File::open(&path)
                    .and_then(|file| hash_reader(file, &mut hasher, progress, &mut buf))
                {
                    Ok(Some(_)) => {
                        let got = hex(&hasher.finalize());
                        if got == f.sha256 {
                            FileState::Ok
                        } else {
                            FileState::WrongHash { sha256: got }
                        }
                    }
                    Ok(None) => {
                        cancelled = true;
                        break;
                    }
                    Err(_) => FileState::Missing,
                }
            }
        };
        files.push(FileCheck {
            path: f.path.clone(),
            expected_sha256: f.sha256.clone(),
            state,
        });
    }
    VerifyReport {
        id: model.id.clone(),
        files,
        cancelled,
    }
}

/// [`verify`] on its own thread.
pub fn start_verify(store: Store, model: Model) -> Job<VerifyReport> {
    Job::spawn("lsr-model-verify", move |p| verify(&store, &model, p))
}
