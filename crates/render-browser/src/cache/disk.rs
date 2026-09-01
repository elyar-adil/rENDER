//! Worker-callable, generation-based disk storage for HTTP cache payloads.
//!
//! [`DiskCacheStore`] deliberately owns no threads. Its methods perform file
//! I/O synchronously, so callers keep it on a dedicated cache-I/O worker and
//! communicate results back to the UI. Clearing swaps the active generation
//! quickly, then lets that worker delete the retired generation separately.
//! This keeps page rendering and settings interactions non-blocking.

use std::cmp::Ordering;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// Absolute upper bound for files retained by one disk cache store.
pub const MAX_DISK_CACHE_BYTES: u64 = 512 * 1024 * 1024;

const DEFAULT_MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const ACTIVE_DIRECTORY: &str = "active";
const ENTRIES_DIRECTORY: &str = "entries";
const ENTRY_SUFFIX: &str = ".entry";
const TEMPORARY_SUFFIX: &str = ".tmp";
const RETIRED_PREFIX: &str = "retired-";
const ENTRY_MAGIC: [u8; 8] = *b"RNDCCH01";
const ENTRY_HEADER_BYTES: usize = 28;
const MAX_KEY_BYTES: usize = 16 * 1024;
const UNIQUE_NAME_ATTEMPTS: usize = 16;
const WORKER_QUEUE_CAPACITY: usize = 64;

/// Filesystem configuration for an isolated disk cache store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskCacheConfig {
    /// Final root directory for this cache format, including its `http-v1` namespace.
    pub root: PathBuf,
    /// Maximum total bytes occupied by current-generation entry files.
    pub max_bytes: u64,
    /// Maximum bytes occupied by one serialized entry file.
    pub max_entry_bytes: u64,
}

impl DiskCacheConfig {
    /// Builds a configuration rooted at an explicit absolute directory.
    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_bytes: MAX_DISK_CACHE_BYTES,
            max_entry_bytes: DEFAULT_MAX_ENTRY_BYTES,
        }
    }

    /// Resolves the normal OS cache location or `RENDER_CACHE_DIR` override.
    ///
    /// An override must be absolute and names the final cache root directly.
    /// Without an override, the format lives under `rENDER/http-v1` in the
    /// platform cache directory.
    ///
    /// # Errors
    ///
    /// Returns an error when neither a valid override nor a platform cache
    /// directory is available.
    pub fn from_environment() -> Result<Self, DiskCacheError> {
        if let Some(root) = env::var_os("RENDER_CACHE_DIR") {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err(DiskCacheError::RelativeRoot(root));
            }
            return Ok(Self::with_root(root));
        }

        let root = platform_cache_directory()
            .ok_or(DiskCacheError::CacheDirectoryUnavailable)?
            .join("rENDER")
            .join("http-v1");
        Ok(Self::with_root(root))
    }
}

/// A cache-I/O error. These are non-fatal to browsing and should be surfaced
/// as best-effort disk-cache status rather than blocking a navigation.
#[derive(Debug)]
pub enum DiskCacheError {
    CacheDirectoryUnavailable,
    RelativeRoot(PathBuf),
    InvalidConfiguration(&'static str),
    UnsafePath(PathBuf),
    InvalidClearJob,
    QueueFull,
    WorkerStopped,
    Io {
        operation: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for DiskCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CacheDirectoryUnavailable => {
                formatter.write_str("the platform cache directory is unavailable")
            }
            Self::RelativeRoot(root) => {
                write!(
                    formatter,
                    "cache directory must be absolute: {}",
                    root.display()
                )
            }
            Self::InvalidConfiguration(message) => formatter.write_str(message),
            Self::UnsafePath(path) => write!(formatter, "unsafe cache path: {}", path.display()),
            Self::InvalidClearJob => formatter.write_str("invalid disk cache clear job"),
            Self::QueueFull => formatter.write_str("disk cache worker queue is full"),
            Self::WorkerStopped => formatter.write_str("disk cache worker stopped"),
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
        }
    }
}

impl Error for DiskCacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Generation token captured before queuing a disk-cache write.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiskCacheGeneration(u64);

impl DiskCacheGeneration {
    /// Returns the numeric generation for diagnostics and tests.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Result of trying to publish an entry in the active generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskCacheWriteOutcome {
    Stored { bytes: u64 },
    Skipped(DiskCacheSkipReason),
}

/// Why a disk cache write was ignored without affecting browsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskCacheSkipReason {
    StaleGeneration,
    KeyTooLarge,
    EntryTooLarge,
    CapacityUnavailable,
}

/// A successfully swapped active generation awaiting worker-side deletion.
#[derive(Debug)]
pub struct DiskCacheClearJob {
    retired_directory: PathBuf,
    retired_bytes: u64,
    generation: DiskCacheGeneration,
}

impl DiskCacheClearJob {
    /// Returns the new active generation after the swap.
    #[must_use]
    pub const fn generation(&self) -> DiskCacheGeneration {
        self.generation
    }

    /// Returns the known entry bytes in the retired generation.
    #[must_use]
    pub const fn retired_bytes(&self) -> u64 {
        self.retired_bytes
    }
}

/// Result of deleting a retired cache generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskCacheClearResult {
    /// The active generation that remains after cleanup.
    pub generation: DiskCacheGeneration,
    /// Entry-file bytes removed from the retired generation.
    pub retired_bytes: u64,
}

/// Identifier for an operation submitted to [`DiskCacheWorker`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiskCacheOperationId(u64);

impl DiskCacheOperationId {
    /// Returns the numeric operation identifier.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Result emitted by the asynchronous disk-cache worker.
#[derive(Debug)]
pub enum DiskCacheEvent {
    /// Worker initialization completed. A failed initialization leaves the
    /// worker unable to service subsequent operations.
    Ready {
        result: Result<DiskCacheGeneration, DiskCacheError>,
    },
    /// A value lookup completed.
    Read {
        id: DiskCacheOperationId,
        result: Result<Option<Vec<u8>>, DiskCacheError>,
    },
    /// A value write completed.
    Write {
        id: DiskCacheOperationId,
        result: Result<DiskCacheWriteOutcome, DiskCacheError>,
    },
    /// The active generation was swapped and retired-directory deletion is
    /// about to run.
    ClearStarted {
        id: DiskCacheOperationId,
        generation: DiskCacheGeneration,
        retired_bytes: u64,
    },
    /// Retired-directory deletion completed.
    ClearFinished {
        id: DiskCacheOperationId,
        result: Result<DiskCacheClearResult, DiskCacheError>,
    },
}

enum DiskCacheCommand {
    Read {
        id: DiskCacheOperationId,
        key: String,
    },
    Write {
        id: DiskCacheOperationId,
        key: String,
        value: Vec<u8>,
        generation: DiskCacheGeneration,
    },
    Clear {
        id: DiskCacheOperationId,
    },
}

/// Dedicated serial I/O worker for [`DiskCacheStore`].
///
/// All filesystem calls happen on the worker thread. Callers submit bounded
/// operations and drain [`DiskCacheEvent`] values with [`Self::poll`], so a
/// browser event loop never waits on cache I/O. The worker owns one store and
/// therefore preserves generation and capacity ordering naturally.
#[derive(Debug)]
pub struct DiskCacheWorker {
    commands: SyncSender<DiskCacheCommand>,
    events: Receiver<DiskCacheEvent>,
    next_id: AtomicU64,
    generation: Arc<AtomicU64>,
}

impl DiskCacheWorker {
    /// Starts a serial cache-I/O thread. Opening the directory is reported as
    /// a [`DiskCacheEvent::Ready`] result rather than performed by the caller.
    ///
    /// # Errors
    ///
    /// Returns an error only when the worker thread itself cannot be spawned.
    pub fn start(config: DiskCacheConfig) -> Result<Self, DiskCacheError> {
        validate_config(&config)?;
        let (commands, command_receiver) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
        let (event_sender, events) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        thread::Builder::new()
            .name("render-browser-cache-io".to_owned())
            .spawn(move || {
                run_worker(config, command_receiver, event_sender, worker_generation);
            })
            .map_err(|source| io_error("spawn disk cache worker", source))?;
        Ok(Self {
            commands,
            events,
            next_id: AtomicU64::new(1),
            generation,
        })
    }

    /// Returns the latest known active generation.
    #[must_use]
    pub fn generation(&self) -> DiskCacheGeneration {
        DiskCacheGeneration(self.generation.load(AtomicOrdering::Acquire))
    }

    /// Queues a non-blocking value lookup.
    ///
    /// # Errors
    ///
    /// Returns [`DiskCacheError::QueueFull`] when the bounded command queue is
    /// full, or [`DiskCacheError::WorkerStopped`] after the worker exits.
    pub fn read(&self, key: impl Into<String>) -> Result<DiskCacheOperationId, DiskCacheError> {
        let id = self.next_operation_id();
        self.enqueue(DiskCacheCommand::Read {
            id,
            key: key.into(),
        })?;
        Ok(id)
    }

    /// Queues a non-blocking value write for a captured generation.
    ///
    /// # Errors
    ///
    /// Returns [`DiskCacheError::QueueFull`] when the bounded command queue is
    /// full, or [`DiskCacheError::WorkerStopped`] after the worker exits.
    pub fn write(
        &self,
        key: impl Into<String>,
        value: Vec<u8>,
        generation: DiskCacheGeneration,
    ) -> Result<DiskCacheOperationId, DiskCacheError> {
        let id = self.next_operation_id();
        self.enqueue(DiskCacheCommand::Write {
            id,
            key: key.into(),
            value,
            generation,
        })?;
        Ok(id)
    }

    /// Queues a two-phase clear. `ClearStarted` is emitted immediately after
    /// the active generation swap; `ClearFinished` follows retired-directory
    /// deletion and may arrive later.
    ///
    /// # Errors
    ///
    /// Returns [`DiskCacheError::QueueFull`] when the bounded command queue is
    /// full, or [`DiskCacheError::WorkerStopped`] after the worker exits.
    pub fn clear(&self) -> Result<DiskCacheOperationId, DiskCacheError> {
        let id = self.next_operation_id();
        self.enqueue(DiskCacheCommand::Clear { id })?;
        Ok(id)
    }

    /// Drains one worker event without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Empty`] when no event is ready, or
    /// [`TryRecvError::Disconnected`] after the worker exits.
    pub fn poll(&self) -> Result<DiskCacheEvent, TryRecvError> {
        self.events.try_recv()
    }

    fn next_operation_id(&self) -> DiskCacheOperationId {
        DiskCacheOperationId(self.next_id.fetch_add(1, AtomicOrdering::Relaxed))
    }

    fn enqueue(&self, command: DiskCacheCommand) -> Result<(), DiskCacheError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => DiskCacheError::QueueFull,
                mpsc::TrySendError::Disconnected(_) => DiskCacheError::WorkerStopped,
            })
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the spawned thread must own its channel and generation handles"
)]
fn run_worker(
    config: DiskCacheConfig,
    commands: Receiver<DiskCacheCommand>,
    events: SyncSender<DiskCacheEvent>,
    generation: Arc<AtomicU64>,
) {
    let mut store = match DiskCacheStore::open(config) {
        Ok(store) => {
            generation.store(store.generation().as_u64(), AtomicOrdering::Release);
            let _ignored = events.send(DiskCacheEvent::Ready {
                result: Ok(store.generation()),
            });
            store
        }
        Err(error) => {
            let _ignored = events.send(DiskCacheEvent::Ready { result: Err(error) });
            return;
        }
    };

    for command in commands {
        match command {
            DiskCacheCommand::Read { id, key } => {
                let _ignored = events.send(DiskCacheEvent::Read {
                    id,
                    result: store.read(&key),
                });
            }
            DiskCacheCommand::Write {
                id,
                key,
                value,
                generation: submitted_generation,
            } => {
                let _ignored = events.send(DiskCacheEvent::Write {
                    id,
                    result: store.write(&key, &value, submitted_generation),
                });
            }
            DiskCacheCommand::Clear { id } => match store.begin_clear() {
                Ok(job) => {
                    let active_generation = job.generation();
                    generation.store(active_generation.as_u64(), AtomicOrdering::Release);
                    let _ignored = events.send(DiskCacheEvent::ClearStarted {
                        id,
                        generation: active_generation,
                        retired_bytes: job.retired_bytes(),
                    });
                    let result = store.finish_clear(job);
                    let _ignored = events.send(DiskCacheEvent::ClearFinished { id, result });
                }
                Err(error) => {
                    let _ignored = events.send(DiskCacheEvent::ClearFinished {
                        id,
                        result: Err(error),
                    });
                }
            },
        }
    }
}

/// Opaque on-disk key/value store for cache payloads.
///
/// Keep one instance on a serial cache-I/O worker. The type is intentionally
/// synchronous; UI callers should enqueue [`Self::write`], [`Self::read`],
/// [`Self::begin_clear`], and [`Self::finish_clear`] rather than call them on
/// the event loop.
#[derive(Debug)]
pub struct DiskCacheStore {
    config: DiskCacheConfig,
    active_directory: PathBuf,
    generation: DiskCacheGeneration,
    retired_bytes_pending: u64,
    sequence: u64,
}

impl DiskCacheStore {
    /// Opens or creates the configured cache root.
    ///
    /// # Errors
    ///
    /// Returns validation or filesystem errors. Call this from cache-I/O
    /// startup, not from the UI thread.
    pub fn open(config: DiskCacheConfig) -> Result<Self, DiskCacheError> {
        validate_config(&config)?;
        ensure_directory(&config.root)?;
        let active_directory = config.root.join(ACTIVE_DIRECTORY);
        ensure_directory(&active_directory)?;
        ensure_directory(&active_directory.join(ENTRIES_DIRECTORY))?;

        let mut store = Self {
            config,
            active_directory,
            generation: DiskCacheGeneration::default(),
            retired_bytes_pending: 0,
            sequence: 0,
        };
        store.reclaim_retired()?;
        Ok(store)
    }

    /// Returns the store configuration.
    #[must_use]
    pub fn config(&self) -> &DiskCacheConfig {
        &self.config
    }

    /// Returns the active generation to capture before queuing a write.
    #[must_use]
    pub const fn generation(&self) -> DiskCacheGeneration {
        self.generation
    }

    /// Reads an intact value from the active generation.
    ///
    /// Corrupt, truncated, or checksum-mismatched records are discarded and
    /// treated as cache misses. This never returns unverified bytes.
    ///
    /// # Errors
    ///
    /// Returns filesystem failures that prevent a safe lookup.
    pub fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, DiskCacheError> {
        if key.len() > MAX_KEY_BYTES {
            return Ok(None);
        }

        let hash = stable_hash(key.as_bytes());
        let mut candidates = self.entries_matching_hash(hash)?;
        candidates.sort_by(newest_first);

        for candidate in candidates {
            match read_entry(&candidate.path, key, self.config.max_entry_bytes)? {
                EntryRead::Value(value) => return Ok(Some(value)),
                EntryRead::KeyMismatch => {}
                EntryRead::Corrupt => {
                    let _ignored = fs::remove_file(&candidate.path);
                }
            }
        }

        Ok(None)
    }

    /// Atomically publishes a value in the active generation.
    ///
    /// The value is written to a unique temporary file, synced, then linked
    /// into place under a unique final filename. A crash therefore yields an
    /// intact previous entry, an ignored temporary file, or an intact new
    /// entry—never a partial record served to a page.
    ///
    /// # Errors
    ///
    /// Returns filesystem failures. Capacity and stale-generation outcomes are
    /// reported as [`DiskCacheWriteOutcome::Skipped`].
    pub fn write(
        &mut self,
        key: &str,
        value: &[u8],
        submitted_generation: DiskCacheGeneration,
    ) -> Result<DiskCacheWriteOutcome, DiskCacheError> {
        if submitted_generation != self.generation {
            return Ok(DiskCacheWriteOutcome::Skipped(
                DiskCacheSkipReason::StaleGeneration,
            ));
        }
        if key.len() > MAX_KEY_BYTES {
            return Ok(DiskCacheWriteOutcome::Skipped(
                DiskCacheSkipReason::KeyTooLarge,
            ));
        }

        let record_bytes = record_size(key, value)?;
        if record_bytes > self.config.max_entry_bytes || record_bytes > self.config.max_bytes {
            return Ok(DiskCacheWriteOutcome::Skipped(
                DiskCacheSkipReason::EntryTooLarge,
            ));
        }

        self.reclaim_orphaned_temporary_files()?;
        if record_bytes > self.available_active_bytes() || !self.prune_for_write(record_bytes)? {
            return Ok(DiskCacheWriteOutcome::Skipped(
                DiskCacheSkipReason::CapacityUnavailable,
            ));
        }
        self.write_atomic(key, value, record_bytes)?;
        Ok(DiskCacheWriteOutcome::Stored {
            bytes: record_bytes,
        })
    }

    /// Atomically replaces the active directory with a fresh generation.
    ///
    /// The returned job owns the retired directory and must be passed to
    /// [`Self::finish_clear`] on the cache-I/O worker. The new generation is
    /// usable as soon as this method returns, before the old files are deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the generation cannot be swapped safely. Callers
    /// should still retain their independently cleared memory-cache state.
    pub fn begin_clear(&mut self) -> Result<DiskCacheClearJob, DiskCacheError> {
        ensure_directory(&self.active_directory)?;
        ensure_directory(&self.entries_directory())?;
        let retired_bytes = self.current_entry_bytes()?;
        let retired_name = self.unique_name(RETIRED_PREFIX, "");
        let retired_directory = self.config.root.join(retired_name);

        fs::rename(&self.active_directory, &retired_directory)
            .map_err(|source| io_error("swap active disk cache generation", source))?;
        if let Err(error) = ensure_directory(&self.active_directory)
            .and_then(|()| ensure_directory(&self.entries_directory()))
        {
            let _ignored = fs::rename(&retired_directory, &self.active_directory);
            return Err(error);
        }

        self.retired_bytes_pending = self.retired_bytes_pending.saturating_add(retired_bytes);
        self.generation = DiskCacheGeneration(self.generation.0.saturating_add(1));
        Ok(DiskCacheClearJob {
            retired_directory,
            retired_bytes,
            generation: self.generation,
        })
    }

    /// Deletes the retired generation from a prior [`Self::begin_clear`].
    ///
    /// This is intentionally separate from the swap because recursive removal
    /// can take time. Invoke it only on the cache-I/O worker.
    ///
    /// # Errors
    ///
    /// Returns a filesystem error if cleanup fails; the fresh active cache
    /// generation remains valid.
    #[allow(clippy::needless_pass_by_value)]
    pub fn finish_clear(
        &mut self,
        job: DiskCacheClearJob,
    ) -> Result<DiskCacheClearResult, DiskCacheError> {
        if !is_retired_directory(&self.config.root, &job.retired_directory) {
            return Err(DiskCacheError::InvalidClearJob);
        }
        remove_directory_if_present(&job.retired_directory)?;
        self.retired_bytes_pending = self.retired_bytes_pending.saturating_sub(job.retired_bytes);
        Ok(DiskCacheClearResult {
            generation: self.generation,
            retired_bytes: job.retired_bytes,
        })
    }

    /// Performs both phases of a clear on the current worker.
    ///
    /// Prefer [`Self::begin_clear`] plus [`Self::finish_clear`] when the
    /// caller wants to report the generation swap before deletion completes.
    ///
    /// # Errors
    ///
    /// Returns errors from either phase without invalidating the new active
    /// generation after a successful swap.
    pub fn clear_blocking(&mut self) -> Result<DiskCacheClearResult, DiskCacheError> {
        let job = self.begin_clear()?;
        self.finish_clear(job)
    }

    /// Deletes abandoned retired generations and temporary files.
    ///
    /// This recovery helper is safe to call during cache-worker startup or
    /// idle time. It never removes the active directory or the cache root.
    ///
    /// # Errors
    ///
    /// Returns filesystem failures encountered while scanning or deleting
    /// cache-owned files.
    pub fn reclaim_retired(&mut self) -> Result<(), DiskCacheError> {
        self.reclaim_orphaned_temporary_files()?;
        for entry in read_directory(&self.config.root)? {
            let path = entry
                .map_err(|source| io_error("read cache directory entry", source))?
                .path();
            if is_retired_directory(&self.config.root, &path) {
                remove_directory_if_present(&path)?;
            }
        }
        Ok(())
    }

    fn entries_directory(&self) -> PathBuf {
        self.active_directory.join(ENTRIES_DIRECTORY)
    }

    fn current_entry_bytes(&self) -> Result<u64, DiskCacheError> {
        self.entry_files().map(|entries| {
            entries
                .into_iter()
                .fold(0_u64, |total, entry| total.saturating_add(entry.bytes))
        })
    }

    fn available_active_bytes(&self) -> u64 {
        self.config
            .max_bytes
            .saturating_sub(self.retired_bytes_pending)
    }

    fn prune_for_write(&mut self, new_entry_bytes: u64) -> Result<bool, DiskCacheError> {
        let mut entries = self.entry_files()?;
        let mut total = entries
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.bytes));
        let active_limit = self.available_active_bytes();
        if total.saturating_add(new_entry_bytes) <= active_limit {
            return Ok(true);
        }

        entries.sort_by(oldest_first);
        for entry in entries {
            fs::remove_file(&entry.path)
                .map_err(|source| io_error("remove old cache entry", source))?;
            total = total.saturating_sub(entry.bytes);
            if total.saturating_add(new_entry_bytes) <= active_limit {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn write_atomic(
        &mut self,
        key: &str,
        value: &[u8],
        record_bytes: u64,
    ) -> Result<(), DiskCacheError> {
        let entries_directory = self.entries_directory();
        let hash = stable_hash(key.as_bytes());
        let checksum = entry_checksum(key.as_bytes(), value);

        for _ in 0..UNIQUE_NAME_ATTEMPTS {
            let name = self.unique_name(&format!("{hash:016x}-"), ENTRY_SUFFIX);
            let final_path = entries_directory.join(&name);
            let temporary_path = entries_directory.join(format!("{name}{TEMPORARY_SUFFIX}"));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error("create temporary cache entry", source)),
            };

            let write_result = write_record(&mut file, key.as_bytes(), value, checksum)
                .and_then(|()| file.sync_all());
            if let Err(source) = write_result {
                let _ignored = fs::remove_file(&temporary_path);
                return Err(io_error("write temporary cache entry", source));
            }
            drop(file);

            match fs::hard_link(&temporary_path, &final_path) {
                Ok(()) => {
                    fs::remove_file(&temporary_path)
                        .map_err(|source| io_error("remove temporary cache entry", source))?;
                    debug_assert_eq!(
                        fs::metadata(&final_path)
                            .map_err(|source| io_error("inspect published cache entry", source))?
                            .len(),
                        record_bytes
                    );
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let _ignored = fs::remove_file(&temporary_path);
                }
                Err(source) => {
                    let _ignored = fs::remove_file(&temporary_path);
                    return Err(io_error("publish cache entry", source));
                }
            }
        }

        Err(io_error(
            "allocate a unique cache entry name",
            io::Error::new(io::ErrorKind::AlreadyExists, "cache entry name collision"),
        ))
    }

    fn entry_files(&self) -> Result<Vec<DiskEntryFile>, DiskCacheError> {
        let mut files = Vec::new();
        for entry in read_directory(&self.entries_directory())? {
            let path = entry
                .map_err(|source| io_error("read cache entry directory", source))?
                .path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|source| io_error("inspect cache entry", source))?;
            if !metadata.file_type().is_file() || !has_suffix(&path, ENTRY_SUFFIX) {
                continue;
            }
            files.push(DiskEntryFile {
                path,
                bytes: metadata.len(),
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            });
        }
        Ok(files)
    }

    fn entries_matching_hash(&self, hash: u64) -> Result<Vec<DiskEntryFile>, DiskCacheError> {
        let prefix = format!("{hash:016x}-");
        self.entry_files().map(|entries| {
            entries
                .into_iter()
                .filter(|entry| {
                    file_name(&entry.path).is_some_and(|name| name.starts_with(&prefix))
                })
                .collect()
        })
    }

    fn reclaim_orphaned_temporary_files(&mut self) -> Result<(), DiskCacheError> {
        for entry in read_directory(&self.entries_directory())? {
            let path = entry
                .map_err(|source| io_error("read cache entry directory", source))?
                .path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|source| io_error("inspect temporary cache entry", source))?;
            if metadata.file_type().is_file() && has_suffix(&path, TEMPORARY_SUFFIX) {
                fs::remove_file(path)
                    .map_err(|source| io_error("remove temporary cache entry", source))?;
            }
        }
        Ok(())
    }

    fn unique_name(&mut self, prefix: &str, suffix: &str) -> String {
        self.sequence = self.sequence.saturating_add(1);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        format!(
            "{prefix}{nanos:032x}-{:x}-{:016x}{suffix}",
            std::process::id(),
            self.sequence
        )
    }
}

#[derive(Debug)]
struct DiskEntryFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

enum EntryRead {
    Value(Vec<u8>),
    KeyMismatch,
    Corrupt,
}

fn validate_config(config: &DiskCacheConfig) -> Result<(), DiskCacheError> {
    if !config.root.is_absolute() {
        return Err(DiskCacheError::RelativeRoot(config.root.clone()));
    }
    if config.max_bytes == 0 {
        return Err(DiskCacheError::InvalidConfiguration(
            "disk cache capacity must be non-zero",
        ));
    }
    if config.max_bytes > MAX_DISK_CACHE_BYTES {
        return Err(DiskCacheError::InvalidConfiguration(
            "disk cache capacity exceeds the 512 MiB hard limit",
        ));
    }
    if config.max_entry_bytes == 0 || config.max_entry_bytes > config.max_bytes {
        return Err(DiskCacheError::InvalidConfiguration(
            "disk cache entry capacity must be between one byte and total capacity",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_cache_directory() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches"))
}

#[cfg(target_os = "windows")]
fn platform_cache_directory() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .or_else(|| env::var_os("APPDATA"))
        .map(PathBuf::from)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_cache_directory() -> Option<PathBuf> {
    env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
}

#[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
fn platform_cache_directory() -> Option<PathBuf> {
    None
}

fn ensure_directory(path: &Path) -> Result<(), DiskCacheError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(DiskCacheError::UnsafePath(path.to_owned()));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|source| io_error("create cache directory", source))?;
            let metadata = fs::symlink_metadata(path)
                .map_err(|source| io_error("inspect cache directory", source))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(DiskCacheError::UnsafePath(path.to_owned()));
            }
        }
        Err(source) => return Err(io_error("inspect cache directory", source)),
    }
    Ok(())
}

fn read_directory(path: &Path) -> Result<fs::ReadDir, DiskCacheError> {
    fs::read_dir(path).map_err(|source| io_error("read cache directory", source))
}

fn remove_directory_if_present(path: &Path) -> Result<(), DiskCacheError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(DiskCacheError::UnsafePath(path.to_owned()));
            }
            fs::remove_dir_all(path)
                .map_err(|source| io_error("remove retired cache generation", source))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect retired cache generation", source)),
    }
}

fn is_retired_directory(root: &Path, path: &Path) -> bool {
    path.parent() == Some(root)
        && file_name(path).is_some_and(|name| name.starts_with(RETIRED_PREFIX))
}

fn write_record(file: &mut File, key: &[u8], value: &[u8], checksum: u64) -> io::Result<()> {
    file.write_all(&ENTRY_MAGIC)?;
    file.write_all(
        &u32::try_from(key.len())
            .expect("key limit fits u32")
            .to_le_bytes(),
    )?;
    file.write_all(
        &u64::try_from(value.len())
            .expect("slice length fits u64")
            .to_le_bytes(),
    )?;
    file.write_all(&checksum.to_le_bytes())?;
    file.write_all(key)?;
    file.write_all(value)
}

fn read_entry(
    path: &Path,
    expected_key: &str,
    max_entry_bytes: u64,
) -> Result<EntryRead, DiskCacheError> {
    let metadata = fs::metadata(path).map_err(|source| io_error("inspect cache entry", source))?;
    if metadata.len() < u64::try_from(ENTRY_HEADER_BYTES).expect("header length fits u64")
        || metadata.len() > max_entry_bytes
    {
        return Ok(EntryRead::Corrupt);
    }

    let mut file = File::open(path).map_err(|source| io_error("open cache entry", source))?;
    let mut header = [0; ENTRY_HEADER_BYTES];
    if file.read_exact(&mut header).is_err() || header[..ENTRY_MAGIC.len()] != ENTRY_MAGIC {
        return Ok(EntryRead::Corrupt);
    }
    let key_bytes = u32::from_le_bytes(
        header[8..12]
            .try_into()
            .expect("fixed-size key length slice"),
    ) as usize;
    let value_bytes = u64::from_le_bytes(
        header[12..20]
            .try_into()
            .expect("fixed-size value length slice"),
    );
    let checksum = u64::from_le_bytes(
        header[20..28]
            .try_into()
            .expect("fixed-size checksum slice"),
    );
    let Ok(expected_bytes) = record_size_from_lengths(key_bytes, value_bytes) else {
        return Ok(EntryRead::Corrupt);
    };
    if expected_bytes != metadata.len()
        || key_bytes > MAX_KEY_BYTES
        || value_bytes > max_entry_bytes
    {
        return Ok(EntryRead::Corrupt);
    }
    let Ok(value_length) = usize::try_from(value_bytes) else {
        return Ok(EntryRead::Corrupt);
    };

    let mut key = vec![0; key_bytes];
    let mut value = vec![0; value_length];
    if file.read_exact(&mut key).is_err() || file.read_exact(&mut value).is_err() {
        return Ok(EntryRead::Corrupt);
    }
    if key != expected_key.as_bytes() {
        return Ok(EntryRead::KeyMismatch);
    }
    if entry_checksum(&key, &value) != checksum {
        return Ok(EntryRead::Corrupt);
    }
    Ok(EntryRead::Value(value))
}

fn record_size(key: &str, value: &[u8]) -> Result<u64, DiskCacheError> {
    record_size_from_lengths(
        key.len(),
        u64::try_from(value.len()).expect("slice length fits u64"),
    )
    .map_err(|()| DiskCacheError::InvalidConfiguration("disk cache record size overflow"))
}

fn record_size_from_lengths(key_bytes: usize, value_bytes: u64) -> Result<u64, ()> {
    u64::try_from(ENTRY_HEADER_BYTES)
        .map_err(|_| ())?
        .checked_add(u64::try_from(key_bytes).map_err(|_| ())?)
        .and_then(|size| size.checked_add(value_bytes))
        .ok_or(())
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn entry_checksum(key: &[u8], value: &[u8]) -> u64 {
    stable_hash(key)
        .to_le_bytes()
        .iter()
        .chain(value)
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn oldest_first(left: &DiskEntryFile, right: &DiskEntryFile) -> Ordering {
    left.modified
        .cmp(&right.modified)
        .then_with(|| left.path.cmp(&right.path))
}

fn newest_first(left: &DiskEntryFile, right: &DiskEntryFile) -> Ordering {
    oldest_first(right, left)
}

fn has_suffix(path: &Path, suffix: &str) -> bool {
    file_name(path).is_some_and(|name| name.ends_with(suffix))
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

fn io_error(operation: &'static str, source: io::Error) -> DiskCacheError {
    DiskCacheError::Io { operation, source }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::TryRecvError;
    use std::thread;
    use std::time::Duration;

    use super::{
        DiskCacheConfig, DiskCacheError, DiskCacheEvent, DiskCacheSkipReason, DiskCacheStore,
        DiskCacheWorker, DiskCacheWriteOutcome, MAX_DISK_CACHE_BYTES,
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "render-browser-disk-cache-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create isolated test cache directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ignored = fs::remove_dir_all(&self.0);
        }
    }

    fn store_for(directory: &TestDirectory, max_bytes: u64) -> DiskCacheStore {
        let mut config = DiskCacheConfig::with_root(directory.path().join("http-v1"));
        config.max_bytes = max_bytes;
        config.max_entry_bytes = max_bytes;
        DiskCacheStore::open(config).expect("open test disk cache")
    }

    #[test]
    fn atomic_records_round_trip_and_corruption_is_a_miss() {
        let directory = TestDirectory::new();
        let mut store = store_for(&directory, 1024);
        let generation = store.generation();

        assert_eq!(
            store
                .write("https://example.test/a", b"cached payload", generation)
                .expect("write cache entry"),
            DiskCacheWriteOutcome::Stored { bytes: 64 }
        );
        assert_eq!(
            store
                .read("https://example.test/a")
                .expect("read cache entry"),
            Some(b"cached payload".to_vec())
        );

        let entry = store
            .entry_files()
            .expect("entry files")
            .pop()
            .expect("one entry");
        fs::write(&entry.path, b"corrupted").expect("corrupt test record");
        assert_eq!(
            store
                .read("https://example.test/a")
                .expect("read cache entry"),
            None
        );
        assert!(!entry.path.exists());
    }

    #[test]
    fn clear_swaps_generations_and_rejects_late_writes() {
        let directory = TestDirectory::new();
        let mut store = store_for(&directory, 1024);
        let old_generation = store.generation();
        store
            .write("https://example.test/a", b"old", old_generation)
            .expect("write old entry");

        let job = store.begin_clear().expect("swap cache generation");
        assert!(job.generation() > old_generation);
        assert!(job.retired_bytes() > 0);
        assert_eq!(
            store
                .read("https://example.test/a")
                .expect("read cache entry"),
            None
        );
        assert_eq!(
            store
                .write("https://example.test/a", b"late", old_generation)
                .expect("stale write result"),
            DiskCacheWriteOutcome::Skipped(DiskCacheSkipReason::StaleGeneration)
        );
        let generation = store.generation();
        store
            .write("https://example.test/a", b"new", generation)
            .expect("write new entry");

        let result = store.finish_clear(job).expect("delete retired generation");
        assert_eq!(result.generation, generation);
        assert!(result.retired_bytes > 0);
        assert_eq!(
            store
                .read("https://example.test/a")
                .expect("read cache entry"),
            Some(b"new".to_vec())
        );
    }

    #[test]
    fn capacity_prunes_oldest_entries_before_publishing() {
        let directory = TestDirectory::new();
        let mut store = store_for(&directory, 100);
        let generation = store.generation();

        store
            .write("https://example.test/first", b"first payload", generation)
            .expect("write first entry");
        store
            .write("https://example.test/second", b"second value", generation)
            .expect("write second entry");

        assert_eq!(
            store
                .read("https://example.test/first")
                .expect("read cache entry"),
            None
        );
        assert_eq!(
            store
                .read("https://example.test/second")
                .expect("read cache entry"),
            Some(b"second value".to_vec())
        );
        assert!(store.current_entry_bytes().expect("entry bytes") <= 100);
    }

    #[test]
    fn configuration_cannot_exceed_the_hard_disk_limit() {
        let directory = TestDirectory::new();
        let mut config = DiskCacheConfig::with_root(directory.path().join("http-v1"));
        config.max_bytes = MAX_DISK_CACHE_BYTES + 1;
        config.max_entry_bytes = config.max_bytes;

        assert!(matches!(
            DiskCacheStore::open(config),
            Err(DiskCacheError::InvalidConfiguration(_))
        ));
    }

    fn next_worker_event(worker: &DiskCacheWorker) -> DiskCacheEvent {
        for _ in 0..500 {
            match worker.poll() {
                Ok(event) => return event,
                Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
                Err(TryRecvError::Disconnected) => panic!("disk cache worker stopped"),
            }
        }
        panic!("disk cache worker did not produce an event")
    }

    #[test]
    fn worker_keeps_filesystem_io_off_the_calling_thread() {
        let directory = TestDirectory::new();
        let worker =
            DiskCacheWorker::start(DiskCacheConfig::with_root(directory.path().join("http-v1")))
                .expect("start disk cache worker");
        assert!(matches!(
            next_worker_event(&worker),
            DiskCacheEvent::Ready { result: Ok(_) }
        ));
        let generation = worker.generation();
        let write_id = worker
            .write("https://example.test/a", b"payload".to_vec(), generation)
            .expect("queue cache write");
        let event = next_worker_event(&worker);
        assert!(matches!(
            event,
            DiskCacheEvent::Write {
                id,
                result: Ok(DiskCacheWriteOutcome::Stored { .. })
            } if id == write_id
        ));
        let read_id = worker
            .read("https://example.test/a")
            .expect("queue cache read");
        let event = next_worker_event(&worker);
        assert!(matches!(
            event,
            DiskCacheEvent::Read {
                id,
                result: Ok(Some(value))
            } if id == read_id && value == b"payload"
        ));
    }

    #[test]
    fn worker_reports_generation_swap_before_retired_cleanup() {
        let directory = TestDirectory::new();
        let worker =
            DiskCacheWorker::start(DiskCacheConfig::with_root(directory.path().join("http-v1")))
                .expect("start disk cache worker");
        assert!(matches!(
            next_worker_event(&worker),
            DiskCacheEvent::Ready { result: Ok(_) }
        ));
        let clear_id = worker.clear().expect("queue cache clear");
        let started = next_worker_event(&worker);
        let started_generation = match started {
            DiskCacheEvent::ClearStarted { id, generation, .. } => {
                assert_eq!(id, clear_id);
                generation
            }
            other => panic!("expected clear start, got {other:?}"),
        };
        let finished = next_worker_event(&worker);
        assert!(matches!(
            finished,
            DiskCacheEvent::ClearFinished {
                id,
                result: Ok(result)
            } if id == clear_id && result.generation == started_generation
        ));
        assert_eq!(worker.generation(), started_generation);
    }
}
