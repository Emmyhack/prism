//! Deterministic, content-addressed cache keys and safe artifact paths.
//!
//! Cache entries are disposable derived data. The cache layer never treats a
//! cache hit as authoritative project state and never uses source paths as
//! filenames, avoiding path leaks and traversal through media metadata.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Version for the cache-key encoding.
pub const CACHE_KEY_VERSION: u8 = 1;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A stable SHA-256 identifier for one derived artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
    /// Computes a key from a media fingerprint, operation, and parameters.
    ///
    /// Length prefixes make concatenated fields unambiguous. Callers should
    /// include all rendering-affecting settings in `parameters`.
    #[must_use]
    pub fn from_parts(
        namespace: &str,
        source_fingerprint: &[u8],
        operation: &str,
        parameters: &[u8],
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update([CACHE_KEY_VERSION]);
        update_field(&mut hasher, namespace.as_bytes());
        update_field(&mut hasher, source_fingerprint);
        update_field(&mut hasher, operation.as_bytes());
        update_field(&mut hasher, parameters);
        Self(hasher.finalize().into())
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Returns the lowercase hexadecimal digest used in filenames.
    #[must_use]
    pub fn as_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            let _ = write!(output, "{byte:02x}");
        }
        output
    }
}

fn update_field(hasher: &mut Sha256, field: &[u8]) {
    hasher.update((field.len() as u64).to_be_bytes());
    hasher.update(field);
}

/// Cache-root path construction failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CacheError {
    /// Extensions are restricted to filename-safe ASCII characters.
    #[error("cache extension must contain only ASCII letters, digits, '-' or '_'")]
    InvalidExtension,
    /// The derived artifact already exists and was not replaced.
    #[error("cache artifact already exists: {0}")]
    AlreadyExists(PathBuf),
    /// Filesystem operation failed while writing a cache artifact.
    #[error("cache filesystem operation failed: {0}")]
    Io(String),
}

/// Bounds applied when reclaiming disposable cache artifacts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheEvictionPolicy {
    /// Remove the least-recently-modified artifacts until this byte bound is met.
    pub max_bytes: Option<u64>,
    /// Remove artifacts older than this age before applying the size bound.
    pub max_age: Option<Duration>,
}

/// Result of one cache eviction pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheEvictionReport {
    /// Number of finalized artifacts removed.
    pub removed_files: usize,
    /// Total size of removed artifacts.
    pub removed_bytes: u64,
}

/// One derived artifact write scheduled on a background cache worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheWriteRequest {
    /// Content-addressed artifact key.
    pub key: CacheKey,
    /// Validated cache extension.
    pub extension: String,
    /// Finalized artifact bytes.
    pub contents: Vec<u8>,
}

/// Result delivered by a background cache worker.
#[derive(Debug)]
pub struct CacheWriteResult {
    /// Key supplied with the request.
    pub key: CacheKey,
    /// Extension supplied with the request.
    pub extension: String,
    /// Atomic write result.
    pub result: Result<PathBuf, CacheError>,
}

/// Nonblocking cache-worker submission failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum CacheSubmitError {
    /// The bounded queue is full.
    #[error("cache worker queue is full")]
    QueueFull,
    /// The worker has stopped accepting requests.
    #[error("cache worker is stopped")]
    WorkerStopped,
}

/// Bounded background scheduler for atomic cache writes.
pub struct CacheWorker {
    requests: Option<SyncSender<CacheWriteRequest>>,
    results: Receiver<CacheWriteResult>,
    thread: Option<JoinHandle<()>>,
}

/// Result delivered by a background eviction worker.
#[derive(Debug)]
pub struct CacheEvictionResult {
    /// Policy applied by the worker.
    pub policy: CacheEvictionPolicy,
    /// Eviction outcome.
    pub result: Result<CacheEvictionReport, CacheError>,
}

/// Bounded background scheduler for disposable cache eviction.
pub struct CacheEvictionWorker {
    requests: Option<SyncSender<CacheEvictionPolicy>>,
    results: Receiver<CacheEvictionResult>,
    thread: Option<JoinHandle<()>>,
}

impl CacheWorker {
    /// Starts a worker with an explicitly bounded pending queue.
    #[must_use]
    pub fn spawn(layout: CacheLayout, queue_capacity: usize) -> Self {
        let (request_tx, request_rx) = sync_channel::<CacheWriteRequest>(queue_capacity.max(1));
        let (result_tx, result_rx) =
            sync_channel::<CacheWriteResult>(queue_capacity.max(1).saturating_add(1));
        let worker_thread = thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let key = request.key;
                let extension = request.extension.clone();
                let result = layout.write_atomic(key, &extension, &request.contents);
                if result_tx
                    .send(CacheWriteResult {
                        key,
                        extension,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            requests: Some(request_tx),
            results: result_rx,
            thread: Some(worker_thread),
        }
    }

    /// Attempts to enqueue one write without blocking the caller.
    ///
    /// # Errors
    ///
    /// Returns [`CacheSubmitError::QueueFull`] under backpressure or
    /// [`CacheSubmitError::WorkerStopped`] after shutdown.
    pub fn try_submit(&self, request: CacheWriteRequest) -> Result<(), CacheSubmitError> {
        self.requests
            .as_ref()
            .ok_or(CacheSubmitError::WorkerStopped)?
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => CacheSubmitError::QueueFull,
                TrySendError::Disconnected(_) => CacheSubmitError::WorkerStopped,
            })
    }

    /// Polls for one completed write without blocking.
    ///
    /// # Errors
    ///
    /// Returns the channel receive error when no result is ready or the worker
    /// has disconnected.
    pub fn try_result(&self) -> Result<CacheWriteResult, TryRecvError> {
        self.results.try_recv()
    }

    /// Waits up to `timeout` for one completed write.
    ///
    /// # Errors
    ///
    /// Returns a timeout or disconnected channel error.
    pub fn result_timeout(&self, timeout: Duration) -> Result<CacheWriteResult, RecvTimeoutError> {
        self.results.recv_timeout(timeout)
    }
}

impl CacheEvictionWorker {
    /// Starts an eviction worker with an explicitly bounded queue.
    #[must_use]
    pub fn spawn(layout: CacheLayout, queue_capacity: usize) -> Self {
        let capacity = queue_capacity.max(1);
        let (request_tx, request_rx) = sync_channel::<CacheEvictionPolicy>(capacity);
        let (result_tx, result_rx) =
            sync_channel::<CacheEvictionResult>(capacity.saturating_add(1));
        let worker_thread = thread::spawn(move || {
            while let Ok(policy) = request_rx.recv() {
                let result = layout.evict(policy);
                if result_tx
                    .send(CacheEvictionResult { policy, result })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            requests: Some(request_tx),
            results: result_rx,
            thread: Some(worker_thread),
        }
    }

    /// Attempts to enqueue one eviction pass without blocking the caller.
    ///
    /// # Errors
    ///
    /// Returns [`CacheSubmitError::QueueFull`] under backpressure or
    /// [`CacheSubmitError::WorkerStopped`] after shutdown.
    pub fn try_submit(&self, policy: CacheEvictionPolicy) -> Result<(), CacheSubmitError> {
        self.requests
            .as_ref()
            .ok_or(CacheSubmitError::WorkerStopped)?
            .try_send(policy)
            .map_err(|error| match error {
                TrySendError::Full(_) => CacheSubmitError::QueueFull,
                TrySendError::Disconnected(_) => CacheSubmitError::WorkerStopped,
            })
    }

    /// Polls for one completed eviction pass without blocking.
    ///
    /// # Errors
    ///
    /// Returns the channel receive error when no result is ready or the worker
    /// has disconnected.
    pub fn try_result(&self) -> Result<CacheEvictionResult, TryRecvError> {
        self.results.try_recv()
    }

    /// Waits up to `timeout` for one completed eviction pass.
    ///
    /// # Errors
    ///
    /// Returns a timeout or disconnected channel error.
    pub fn result_timeout(
        &self,
        timeout: Duration,
    ) -> Result<CacheEvictionResult, RecvTimeoutError> {
        self.results.recv_timeout(timeout)
    }
}

impl Drop for CacheEvictionWorker {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CacheWorker {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Constructs sharded paths beneath a configured cache root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheLayout {
    root: PathBuf,
}

impl CacheLayout {
    /// Creates a layout rooted at `root`; the directory is not created eagerly.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the configured root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns a sharded, traversal-safe artifact path.
    ///
    /// The caller remains responsible for creating parent directories and
    /// atomically writing the artifact.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidExtension`] for path-like extensions.
    pub fn path_for(&self, key: CacheKey, extension: &str) -> Result<PathBuf, CacheError> {
        if extension.is_empty()
            || !extension.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '-' || character == '_'
            })
        {
            return Err(CacheError::InvalidExtension);
        }
        let hex = key.as_hex();
        Ok(self.root.join(&hex[..2]).join(format!("{hex}.{extension}")))
    }

    /// Returns the finalized artifact path when a cache entry exists.
    ///
    /// This performs no filesystem mutation and treats temporary files as
    /// misses.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidExtension`] for an unsafe extension.
    pub fn lookup(&self, key: CacheKey, extension: &str) -> Result<Option<PathBuf>, CacheError> {
        let path = self.path_for(key, extension)?;
        Ok(path.is_file().then_some(path))
    }

    /// Reads a finalized cache artifact, returning `None` for a cache miss.
    ///
    /// Temporary files and missing entries are treated as misses. The
    /// extension is still validated before any filesystem access.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidExtension`] for an unsafe extension or
    /// [`CacheError::Io`] when an existing artifact cannot be read.
    pub fn read(&self, key: CacheKey, extension: &str) -> Result<Option<Vec<u8>>, CacheError> {
        let Some(path) = self.lookup(key, extension)? else {
            return Ok(None);
        };
        fs::read(path)
            .map(Some)
            .map_err(|error| CacheError::Io(error.to_string()))
    }

    /// Returns the byte length of a finalized artifact, or `None` on a miss.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidExtension`] for an unsafe extension or
    /// [`CacheError::Io`] when metadata cannot be inspected.
    pub fn size(&self, key: CacheKey, extension: &str) -> Result<Option<u64>, CacheError> {
        let Some(path) = self.lookup(key, extension)? else {
            return Ok(None);
        };
        fs::metadata(path)
            .map(|metadata| Some(metadata.len()))
            .map_err(|error| CacheError::Io(error.to_string()))
    }

    /// Atomically writes a new cache artifact without replacing an existing one.
    ///
    /// Parent directories are created as needed. The temporary file is synced
    /// before finalization and removed when an error occurs.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::AlreadyExists`] when the target is present,
    /// [`CacheError::InvalidExtension`] for an unsafe extension, or
    /// [`CacheError::Io`] for filesystem failures.
    pub fn write_atomic(
        &self,
        key: CacheKey,
        extension: &str,
        contents: &[u8],
    ) -> Result<PathBuf, CacheError> {
        let target = self.path_for(key, extension)?;
        if target.exists() {
            return Err(CacheError::AlreadyExists(target));
        }
        let parent = target
            .parent()
            .ok_or_else(|| CacheError::Io("cache target has no parent".to_owned()))?;
        fs::create_dir_all(parent).map_err(|error| CacheError::Io(error.to_string()))?;
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{}.tmp-{}-{counter}",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("cache"),
            std::process::id()
        ));
        let result: Result<PathBuf, std::io::Error> = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(contents)?;
            file.sync_all()?;
            // A hard link is an atomic create-if-absent operation on the same
            // filesystem. Unlike rename, it cannot replace a target that
            // appeared after the initial existence check.
            fs::hard_link(&temporary, &target)?;
            fs::remove_file(&temporary)?;
            Ok(target.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|error| CacheError::Io(error.to_string()))
    }

    /// Removes disposable artifacts according to an explicit age/size policy.
    ///
    /// Only regular files directly inside the sharded cache directories are
    /// considered. Symlinks, directories, and temporary files are ignored so
    /// eviction cannot follow user-controlled paths or remove an in-progress
    /// atomic write. Age is evaluated first, then the oldest remaining files
    /// are removed until the byte bound is satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Io`] when the cache cannot be enumerated or an
    /// artifact cannot be inspected or removed.
    pub fn evict(&self, policy: CacheEvictionPolicy) -> Result<CacheEvictionReport, CacheError> {
        let mut artifacts = Vec::new();
        if !self.root.exists() {
            return Ok(CacheEvictionReport {
                removed_files: 0,
                removed_bytes: 0,
            });
        }
        for shard in fs::read_dir(&self.root).map_err(|error| CacheError::Io(error.to_string()))? {
            let shard = shard.map_err(|error| CacheError::Io(error.to_string()))?;
            let shard_type = shard
                .file_type()
                .map_err(|error| CacheError::Io(error.to_string()))?;
            if !shard_type.is_dir() {
                continue;
            }
            for entry in
                fs::read_dir(shard.path()).map_err(|error| CacheError::Io(error.to_string()))?
            {
                let entry = entry.map_err(|error| CacheError::Io(error.to_string()))?;
                let file_type = entry
                    .file_type()
                    .map_err(|error| CacheError::Io(error.to_string()))?;
                let name = entry.file_name();
                if !file_type.is_file() || name.to_str().is_some_and(|name| name.starts_with('.')) {
                    continue;
                }
                let metadata = entry
                    .metadata()
                    .map_err(|error| CacheError::Io(error.to_string()))?;
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                artifacts.push((entry.path(), metadata.len(), modified));
            }
        }
        artifacts.sort_by(|left, right| left.2.cmp(&right.2).then_with(|| left.0.cmp(&right.0)));
        let now = SystemTime::now();
        let mut total_bytes = artifacts.iter().map(|artifact| artifact.1).sum::<u64>();
        let mut removed_files = 0;
        let mut removed_bytes: u64 = 0;
        for (path, size, modified) in artifacts {
            let too_old = policy
                .max_age
                .is_some_and(|age| now.duration_since(modified).unwrap_or_default() > age);
            let over_budget = policy.max_bytes.is_some_and(|max| total_bytes > max);
            if !too_old && !over_budget {
                continue;
            }
            fs::remove_file(&path).map_err(|error| CacheError::Io(error.to_string()))?;
            total_bytes = total_bytes.saturating_sub(size);
            removed_files += 1;
            removed_bytes = removed_bytes.saturating_add(size);
        }
        Ok(CacheEvictionReport {
            removed_files,
            removed_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CacheError, CacheEvictionPolicy, CacheEvictionWorker, CacheKey, CacheLayout,
        CacheSubmitError, CacheWorker, CacheWriteRequest,
    };
    use std::time::Duration;

    #[test]
    fn keys_are_deterministic_and_partition_operations() {
        let first = CacheKey::from_parts("thumbnail", b"fingerprint", "rgba", b"width=320");
        let same = CacheKey::from_parts("thumbnail", b"fingerprint", "rgba", b"width=320");
        let different = CacheKey::from_parts("thumbnail", b"fingerprint", "rgba", b"width=321");
        assert_eq!(first, same);
        assert_ne!(first, different);
        assert_eq!(first.as_hex().len(), 64);
        assert_eq!(first.as_bytes().len(), 32);
    }

    #[test]
    fn layout_is_sharded_and_rejects_path_like_extensions() -> Result<(), CacheError> {
        let key = CacheKey::from_parts("waveform", b"media", "peak-rms", b"48k");
        let layout = CacheLayout::new("/tmp/prism-cache");
        let path = layout.path_for(key, "bin")?;
        assert!(path.starts_with(layout.root()));
        assert!(path.extension().is_some_and(|extension| extension == "bin"));
        assert_eq!(
            layout.path_for(key, "../bin"),
            Err(CacheError::InvalidExtension)
        );
        assert_eq!(layout.path_for(key, ""), Err(CacheError::InvalidExtension));
        Ok(())
    }

    #[test]
    fn atomic_write_creates_and_never_replaces_artifact() -> Result<(), CacheError> {
        let root = std::env::temp_dir().join(format!("prism-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let key = CacheKey::from_parts("thumbnail", b"media", "rgba", b"320x180");
        let layout = CacheLayout::new(&root);
        let path = layout.write_atomic(key, "bin", b"first")?;
        assert_eq!(layout.lookup(key, "bin")?, Some(path.clone()));
        assert_eq!(layout.lookup(key, "missing")?, None);
        assert_eq!(
            std::fs::read(&path).map_err(|error| CacheError::Io(error.to_string()))?,
            b"first"
        );
        assert_eq!(layout.read(key, "bin")?, Some(b"first".to_vec()));
        assert_eq!(layout.size(key, "bin")?, Some(5));
        assert_eq!(
            layout.read(
                CacheKey::from_parts("thumbnail", b"missing", "rgba", b"1"),
                "bin"
            )?,
            None
        );
        assert!(matches!(
            layout.write_atomic(key, "bin", b"second"),
            Err(CacheError::AlreadyExists(existing)) if existing == path
        ));
        assert_eq!(
            std::fs::read(&path).map_err(|error| CacheError::Io(error.to_string()))?,
            b"first"
        );
        std::fs::remove_dir_all(root).map_err(|error| CacheError::Io(error.to_string()))?;
        Ok(())
    }

    #[test]
    fn eviction_enforces_size_and_age_without_touching_temporary_files() -> Result<(), CacheError> {
        let root = std::env::temp_dir().join(format!(
            "prism-cache-eviction-test-{}-{}",
            std::process::id(),
            1u64
        ));
        let _ = std::fs::remove_dir_all(&root);
        let layout = CacheLayout::new(&root);
        let first = CacheKey::from_parts("waveform", b"media", "a", b"1");
        let second = CacheKey::from_parts("waveform", b"media", "b", b"2");
        layout.write_atomic(first, "bin", b"one")?;
        layout.write_atomic(second, "bin", b"two")?;
        let temporary = layout
            .path_for(first, "tmp")?
            .with_file_name(".in-progress.tmp");
        std::fs::write(&temporary, b"keep").map_err(|error| CacheError::Io(error.to_string()))?;

        let report = layout.evict(CacheEvictionPolicy {
            max_bytes: Some(0),
            max_age: None,
        })?;
        assert_eq!(report.removed_files, 2);
        assert_eq!(report.removed_bytes, 6);
        assert!(temporary.is_file());
        assert_eq!(
            layout.evict(CacheEvictionPolicy {
                max_bytes: None,
                max_age: Some(Duration::ZERO),
            })?,
            super::CacheEvictionReport {
                removed_files: 0,
                removed_bytes: 0,
            }
        );
        std::fs::remove_dir_all(root).map_err(|error| CacheError::Io(error.to_string()))?;
        Ok(())
    }

    #[test]
    fn worker_writes_off_thread_and_reports_backpressure() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = std::env::temp_dir().join(format!("prism-cache-worker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = CacheLayout::new(&root);
        let worker = CacheWorker::spawn(layout.clone(), 1);
        let first = CacheKey::from_parts("proxy", b"media", "transcode", b"a");
        let second = CacheKey::from_parts("proxy", b"media", "transcode", b"b");
        worker.try_submit(CacheWriteRequest {
            key: first,
            extension: "bin".to_owned(),
            contents: b"first".to_vec(),
        })?;
        let second_submission = worker.try_submit(CacheWriteRequest {
            key: second,
            extension: "bin".to_owned(),
            contents: b"second".to_vec(),
        });
        assert!(matches!(
            second_submission,
            Ok(()) | Err(CacheSubmitError::QueueFull)
        ));
        let result = worker.result_timeout(Duration::from_secs(2))?;
        assert_eq!(result.key, first);
        assert_eq!(result.result.as_ref().map(|path| path.is_file()), Ok(true));
        assert_eq!(std::fs::read(result.result?)?, b"first");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn eviction_worker_applies_policy_off_thread() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "prism-cache-eviction-worker-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let layout = CacheLayout::new(&root);
        let key = CacheKey::from_parts("waveform", b"media", "evict", b"1");
        let path = layout.write_atomic(key, "bin", b"artifact")?;
        let worker = CacheEvictionWorker::spawn(layout.clone(), 1);
        worker.try_submit(CacheEvictionPolicy {
            max_bytes: Some(0),
            max_age: None,
        })?;
        let result = worker.result_timeout(Duration::from_secs(2))?;
        assert_eq!(result.policy.max_bytes, Some(0));
        assert_eq!(result.result?.removed_files, 1);
        assert!(!path.exists());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
