//! `SQLite` persistence adapter for the UI-independent Prism domain model.

use prism_core::{ClipId, MarkerId, MediaAssetId, ProjectId, SequenceId, TrackId, TransitionId};
use prism_project::{
    Clip, Marker, MediaAsset, Project, ProjectError, ProxySourceFingerprint, RestoredProject,
    Sequence, Track, TrackKind, Transition, TransitionAlignment, TransitionKind,
};
use prism_timeline::{RationalTime, TimeError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::{self, JoinHandle},
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;

const STORAGE_SCHEMA_VERSION: u32 = 8;
type StoredSequences = (
    BTreeMap<SequenceId, Sequence>,
    BTreeMap<SequenceId, RationalTime>,
);

/// Persistence and stored-data validation failures.
#[derive(Debug, Error)]
pub enum PersistenceError {
    /// `SQLite` operation failed.
    #[error("SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    /// Filesystem operation failed.
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    /// Stored identifier was not a UUID.
    #[error("invalid UUID in {field}: {value}")]
    InvalidUuid {
        /// Column or object containing the invalid value.
        field: &'static str,
        /// Invalid stored text.
        value: String,
    },
    /// Stored integer could not fit its domain representation.
    #[error("invalid integer in {field}: {value}")]
    InvalidInteger {
        /// Invalid column.
        field: &'static str,
        /// Invalid stored value.
        value: i64,
    },
    /// Stored enum text was not recognized.
    #[error("invalid value in {field}: {value}")]
    InvalidEnum {
        /// Invalid column.
        field: &'static str,
        /// Invalid stored value.
        value: String,
    },
    /// `SQLite` integrity validation reported corruption.
    #[error("project database integrity check failed: {0}")]
    Integrity(String),
    /// Database contains no project root.
    #[error("project database has no project root")]
    MissingProject,
    /// Stored schema is newer than this build understands.
    #[error("unsupported storage schema {found}; maximum supported is {supported}")]
    UnsupportedStorageSchema {
        /// Stored schema version.
        found: u32,
        /// Maximum supported schema version.
        supported: u32,
    },
    /// Recovery backups never replace an existing artifact.
    #[error("backup already exists: {0}")]
    BackupExists(PathBuf),
    /// Autosave queues must have at least one pending slot.
    #[error("autosave queue capacity must be positive")]
    InvalidAutosaveQueueCapacity,
    /// Stored relationship references a missing parent.
    #[error("stored {child} references missing {parent} {id}")]
    Orphan {
        /// Child object kind.
        child: &'static str,
        /// Parent object kind.
        parent: &'static str,
        /// Missing parent identifier.
        id: String,
    },
    /// Exact stored time was invalid.
    #[error("invalid stored time: {0}")]
    Time(#[from] TimeError),
    /// Restored domain state violated project requirements.
    #[error("invalid restored project: {0}")]
    Project(#[from] ProjectError),
}

/// Saves a complete project to an open `SQLite` connection transactionally.
///
/// # Errors
///
/// Returns [`PersistenceError`] if schema initialization or any write fails.
/// The `SQLite` transaction rolls back automatically on failure.
pub fn save_to_connection(
    connection: &mut Connection,
    project: &Project,
) -> Result<(), PersistenceError> {
    check_writable_storage_version(connection)?;
    upgrade_storage_schema(connection)?;
    initialize(connection)?;
    let transaction = connection.transaction()?;
    clear_project(&transaction)?;
    write_project(&transaction, project)?;
    transaction.commit()?;
    Ok(())
}

/// Loads and validates a complete project from `SQLite`.
///
/// # Errors
///
/// Returns [`PersistenceError`] for unsupported schemas, malformed values,
/// broken relationships, or `SQLite` failures.
pub fn load_from_connection(connection: &Connection) -> Result<Project, PersistenceError> {
    upgrade_storage_schema(connection)?;
    check_storage_version(connection)?;
    validate_database(connection)?;
    let root = read_root(connection)?;
    let media = read_media(connection)?;
    let (mut sequences, sequence_frame_rates) = read_sequences(connection)?;
    read_markers(connection, &mut sequences)?;
    read_transitions(connection, &mut sequences)?;
    read_tracks(connection, &mut sequences)?;
    read_clips(connection, &mut sequences, &media)?;
    Project::restore(RestoredProject {
        id: root.id,
        name: root.name,
        schema_version: root.domain_schema,
        revision: root.revision,
        media,
        sequences,
        sequence_frame_rates,
    })
    .map_err(PersistenceError::from)
}

/// Writes a new database beside the destination and atomically renames it.
///
/// On Unix this also syncs the file and parent directory. Replacement semantics
/// for an existing destination on Windows require a future native adapter.
///
/// # Errors
///
/// Returns [`PersistenceError`] if the temporary database cannot be written,
/// synced, validated, or renamed into place.
pub fn save_atomic(project: &Project, path: &Path) -> Result<(), PersistenceError> {
    let temporary = temporary_path(path);
    let result = save_atomic_inner(project, path, &temporary);
    if result.is_err() {
        let _cleanup_result = std::fs::remove_file(&temporary);
    }
    result
}

/// Writes a recovery backup without replacing an existing backup artifact.
///
/// Callers should choose a unique, versioned destination (for example, one
/// file per autosave revision). The active project is never modified.
///
/// # Errors
///
/// Returns [`PersistenceError::BackupExists`] when the destination already
/// exists, or another persistence error when the atomic write fails.
pub fn save_backup(project: &Project, path: &Path) -> Result<(), PersistenceError> {
    if path.exists() {
        return Err(PersistenceError::BackupExists(path.to_path_buf()));
    }
    save_atomic(project, path)
}

/// Writes a revision-scoped autosave beside an active project.
///
/// The generated name is deterministic for a project revision and uses the
/// `.prism.autosave` suffix reserved by the workspace. Existing autosaves are
/// never replaced.
///
/// # Errors
///
/// Returns [`PersistenceError::BackupExists`] when this revision was already
/// autosaved, or another persistence error when the artifact cannot be saved.
pub fn save_autosave(project: &Project, active_path: &Path) -> Result<PathBuf, PersistenceError> {
    let file_name = active_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project.prism");
    let autosave =
        active_path.with_file_name(format!("{file_name}.autosave.{}", project.revision()));
    save_backup(project, &autosave)?;
    Ok(autosave)
}

/// Validates an autosave and restores it to a new destination without
/// replacing an existing project.
///
/// The source autosave is loaded through the same integrity and schema checks
/// as an ordinary project. The destination is written with no-clobber backup
/// semantics, so recovery cannot destroy an existing project by accident.
///
/// # Errors
///
/// Returns [`PersistenceError`] when the source fails integrity/schema
/// validation or the destination already exists/cannot be written.
pub fn recover_autosave(source: &Path, destination: &Path) -> Result<u64, PersistenceError> {
    let project = load(source)?;
    let revision = project.revision();
    save_backup(&project, destination)?;
    Ok(revision)
}

/// Report from one autosave retention pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutosaveRetentionReport {
    /// Revision-scoped autosaves removed by the pass.
    pub removed: Vec<PathBuf>,
    /// Revision-scoped autosaves retained after the pass.
    pub retained: Vec<PathBuf>,
}

/// A discoverable revision-scoped autosave candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutosaveCandidate {
    /// Revision captured by the autosave.
    pub revision: u64,
    /// Candidate project path.
    pub path: PathBuf,
}

/// Lists matching autosaves newest first without opening or mutating them.
///
/// This is the discovery boundary used by a future recovery UI. Unrelated,
/// malformed, and non-regular entries are ignored; project contents are
/// validated only when a caller explicitly loads a selected candidate.
///
/// # Errors
///
/// Returns [`PersistenceError::Io`] when the parent directory cannot be
/// enumerated or an entry cannot be inspected.
pub fn list_autosaves(active_path: &Path) -> Result<Vec<AutosaveCandidate>, PersistenceError> {
    let parent = active_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = active_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project.prism");
    let prefix = format!("{file_name}.autosave.");
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(revision) = name
            .strip_prefix(&prefix)
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        candidates.push(AutosaveCandidate { revision, path });
    }
    candidates.sort_by(|left, right| right.revision.cmp(&left.revision));
    Ok(candidates)
}

/// Removes older revision-scoped autosaves while preserving newest revisions.
///
/// Only regular files matching `<active filename>.autosave.<revision>` in the
/// active file's parent directory are considered. The active project and
/// unrelated files are never touched.
///
/// # Errors
///
/// Returns [`PersistenceError::Io`] if the parent cannot be enumerated or a
/// matching artifact cannot be inspected or removed.
pub fn retain_autosaves(
    active_path: &Path,
    keep_latest: usize,
) -> Result<AutosaveRetentionReport, PersistenceError> {
    let parent = active_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = active_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project.prism");
    let prefix = format!("{file_name}.autosave.");
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(revision) = name
            .strip_prefix(&prefix)
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        candidates.push((revision, path));
    }
    candidates.sort_by_key(|(revision, _)| *revision);
    let remove_count = candidates.len().saturating_sub(keep_latest);
    let mut removed = Vec::with_capacity(remove_count);
    let mut retained = Vec::with_capacity(candidates.len().saturating_sub(remove_count));
    for (index, (_, path)) in candidates.into_iter().enumerate() {
        if index < remove_count {
            std::fs::remove_file(&path)?;
            removed.push(path);
        } else {
            retained.push(path);
        }
    }
    Ok(AutosaveRetentionReport { removed, retained })
}

/// A completed revision-scoped autosave result.
#[derive(Debug)]
pub struct AutosaveResult {
    /// Revision captured when the request was submitted.
    pub revision: u64,
    /// Finalized autosave path on success.
    pub path: Option<PathBuf>,
    /// Persistence outcome from the worker.
    pub result: Result<(), PersistenceError>,
}

/// Outcome when submitting a project snapshot to an autosave worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutosaveSubmit {
    /// Snapshot was queued for background persistence.
    Queued,
    /// The bounded worker queue is full; caller may retry after polling.
    QueueFull,
    /// The worker has already been shut down.
    WorkerStopped,
}

enum AutosaveMessage {
    Project(Project),
    Shutdown,
}

/// Bounded background autosave scheduler.
///
/// The scheduler clones validated in-memory snapshots, writes them through
/// [`save_autosave`] on a dedicated thread, and never performs filesystem I/O
/// on the caller. Repeated submissions of a revision already written by this
/// worker are ignored. The worker owns no project mutation state.
pub struct AutosaveScheduler {
    sender: SyncSender<AutosaveMessage>,
    results: Receiver<AutosaveResult>,
    worker: Option<JoinHandle<()>>,
    active_path: PathBuf,
}

impl AutosaveScheduler {
    /// Starts a scheduler with a bounded request queue.
    ///
    /// A capacity of zero is rejected because it would make nonblocking
    /// submission impossible.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::InvalidAutosaveQueueCapacity`] for zero
    /// capacity or [`PersistenceError::Io`] if the worker cannot start.
    pub fn spawn(
        active_path: impl Into<PathBuf>,
        queue_capacity: usize,
    ) -> Result<Self, PersistenceError> {
        if queue_capacity == 0 {
            return Err(PersistenceError::InvalidAutosaveQueueCapacity);
        }
        let active_path = active_path.into();
        let (sender, receiver) = mpsc::sync_channel(queue_capacity);
        let (result_sender, results) = mpsc::channel();
        let worker_path = active_path.clone();
        let worker = thread::Builder::new()
            .name("prism-autosave".to_owned())
            .spawn(move || autosave_worker(receiver, result_sender, worker_path))
            .map_err(PersistenceError::Io)?;
        Ok(Self {
            sender,
            results,
            worker: Some(worker),
            active_path,
        })
    }

    /// Queues a cloned project snapshot without blocking.
    #[must_use]
    pub fn try_submit(&self, project: &Project) -> AutosaveSubmit {
        match self
            .sender
            .try_send(AutosaveMessage::Project(project.clone()))
        {
            Ok(()) => AutosaveSubmit::Queued,
            Err(TrySendError::Full(_)) => AutosaveSubmit::QueueFull,
            Err(TrySendError::Disconnected(_)) => AutosaveSubmit::WorkerStopped,
        }
    }

    /// Returns the next completed autosave result, if one is ready.
    #[must_use]
    pub fn try_result(&self) -> Option<AutosaveResult> {
        self.results.try_recv().ok()
    }

    /// Waits up to `timeout` for a completed autosave result.
    #[must_use]
    pub fn result_timeout(&self, timeout: Duration) -> Option<AutosaveResult> {
        self.results.recv_timeout(timeout).ok()
    }

    /// Returns the active project path used to derive autosave names.
    #[must_use]
    pub fn active_path(&self) -> &Path {
        &self.active_path
    }
}

impl Drop for AutosaveScheduler {
    fn drop(&mut self) {
        let _ = self.sender.send(AutosaveMessage::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn autosave_worker(
    receiver: Receiver<AutosaveMessage>,
    results: mpsc::Sender<AutosaveResult>,
    active_path: PathBuf,
) {
    let mut last_saved_revision = None;
    while let Ok(message) = receiver.recv() {
        match message {
            AutosaveMessage::Shutdown => break,
            AutosaveMessage::Project(project) => {
                let revision = project.revision();
                if last_saved_revision.is_some_and(|saved| revision <= saved) {
                    continue;
                }
                let outcome = save_autosave(&project, &active_path);
                let (path, result) = match outcome {
                    Ok(path) => (Some(path), Ok(())),
                    Err(error) => (None, Err(error)),
                };
                if result.is_ok() {
                    last_saved_revision = Some(revision);
                }
                let _ = results.send(AutosaveResult {
                    revision,
                    path,
                    result,
                });
            }
        }
    }
}

/// Opens and loads a project database from disk.
///
/// # Errors
///
/// Returns [`PersistenceError`] if the file cannot be opened or validated.
pub fn load(path: &Path) -> Result<Project, PersistenceError> {
    let connection = Connection::open(path)?;
    load_from_connection(&connection)
}

fn save_atomic_inner(
    project: &Project,
    path: &Path,
    temporary: &Path,
) -> Result<(), PersistenceError> {
    let mut connection = Connection::open(temporary)?;
    save_to_connection(&mut connection, project)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    connection.close().map_err(|(_, error)| error)?;
    File::open(temporary)?.sync_all()?;
    std::fs::rename(temporary, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project.prism");
    path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()))
}

fn initialize(connection: &Connection) -> Result<(), PersistenceError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = DELETE;
         CREATE TABLE IF NOT EXISTS project (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             id TEXT NOT NULL,
             name TEXT NOT NULL,
             domain_schema INTEGER NOT NULL,
             revision INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS media (
             id TEXT PRIMARY KEY,
             source_path TEXT NOT NULL,
             proxy_path TEXT,
             proxy_source_sha256 TEXT,
             proxy_source_bytes INTEGER
         );
         CREATE TABLE IF NOT EXISTS sequence (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL,
             frame_rate_value INTEGER NOT NULL DEFAULT 30 CHECK(frame_rate_value > 0),
             frame_rate_scale INTEGER NOT NULL DEFAULT 1 CHECK(frame_rate_scale > 0)
         );
         CREATE TABLE IF NOT EXISTS track (
             id TEXT PRIMARY KEY,
             sequence_id TEXT NOT NULL REFERENCES sequence(id) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('video', 'audio')),
             locked INTEGER NOT NULL CHECK(locked IN (0, 1)),
             visible INTEGER NOT NULL DEFAULT 1 CHECK(visible IN (0, 1)),
             muted INTEGER NOT NULL DEFAULT 0 CHECK(muted IN (0, 1)),
             UNIQUE(sequence_id, position)
         );
         CREATE TABLE IF NOT EXISTS marker (
             id TEXT PRIMARY KEY,
             sequence_id TEXT NOT NULL REFERENCES sequence(id) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             time_value INTEGER NOT NULL,
             time_scale INTEGER NOT NULL CHECK(time_scale > 0),
             label TEXT NOT NULL,
             UNIQUE(sequence_id, position)
         );
         CREATE TABLE IF NOT EXISTS transition (
             id TEXT PRIMARY KEY,
             sequence_id TEXT NOT NULL REFERENCES sequence(id) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             outgoing_clip_id TEXT NOT NULL,
             incoming_clip_id TEXT NOT NULL,
             duration_value INTEGER NOT NULL CHECK(duration_value > 0),
             duration_scale INTEGER NOT NULL CHECK(duration_scale > 0),
             alignment TEXT NOT NULL DEFAULT 'centered' CHECK(alignment IN ('start_at_cut', 'centered', 'end_at_cut')),
             kind TEXT NOT NULL CHECK(kind IN ('dissolve')),
             UNIQUE(sequence_id, position)
         );
         CREATE TABLE IF NOT EXISTS clip (
             id TEXT PRIMARY KEY,
             track_id TEXT NOT NULL REFERENCES track(id) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             media_id TEXT NOT NULL REFERENCES media(id),
             timeline_value INTEGER NOT NULL,
             timeline_scale INTEGER NOT NULL CHECK(timeline_scale > 0),
             source_value INTEGER NOT NULL,
             source_scale INTEGER NOT NULL CHECK(source_scale > 0),
             duration_value INTEGER NOT NULL CHECK(duration_value > 0),
             duration_scale INTEGER NOT NULL CHECK(duration_scale > 0),
             UNIQUE(track_id, position)
         );
         PRAGMA user_version = 8;",
    )?;
    Ok(())
}

fn upgrade_storage_schema(connection: &Connection) -> Result<(), PersistenceError> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 1 {
        connection.execute_batch(
            "ALTER TABLE track ADD COLUMN visible INTEGER NOT NULL DEFAULT 1 CHECK(visible IN (0, 1));
             ALTER TABLE track ADD COLUMN muted INTEGER NOT NULL DEFAULT 0 CHECK(muted IN (0, 1));
             PRAGMA user_version = 2;",
        )?;
    }
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 2 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS marker (
                 id TEXT PRIMARY KEY,
                 sequence_id TEXT NOT NULL REFERENCES sequence(id) ON DELETE CASCADE,
                 position INTEGER NOT NULL,
                 time_value INTEGER NOT NULL,
                 time_scale INTEGER NOT NULL CHECK(time_scale > 0),
                 label TEXT NOT NULL,
                 UNIQUE(sequence_id, position)
             );
             PRAGMA user_version = 3;",
        )?;
    }
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 3 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS transition (
                 id TEXT PRIMARY KEY,
                 sequence_id TEXT NOT NULL REFERENCES sequence(id) ON DELETE CASCADE,
                 position INTEGER NOT NULL,
                 outgoing_clip_id TEXT NOT NULL,
                 incoming_clip_id TEXT NOT NULL,
                 duration_value INTEGER NOT NULL CHECK(duration_value > 0),
                 duration_scale INTEGER NOT NULL CHECK(duration_scale > 0),
                 kind TEXT NOT NULL CHECK(kind IN ('dissolve')),
                 UNIQUE(sequence_id, position)
             );
             PRAGMA user_version = 4;",
        )?;
    }
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 4 {
        connection.execute_batch(
            "ALTER TABLE media ADD COLUMN proxy_path TEXT;
             PRAGMA user_version = 5;",
        )?;
    }
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 5 {
        connection.execute_batch(
            "ALTER TABLE media ADD COLUMN proxy_source_sha256 TEXT;
             ALTER TABLE media ADD COLUMN proxy_source_bytes INTEGER;
             PRAGMA user_version = 6;",
        )?;
    }
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 6 {
        connection.execute_batch(
            "ALTER TABLE sequence ADD COLUMN frame_rate_value INTEGER NOT NULL DEFAULT 30 CHECK(frame_rate_value > 0);
             ALTER TABLE sequence ADD COLUMN frame_rate_scale INTEGER NOT NULL DEFAULT 1 CHECK(frame_rate_scale > 0);
             PRAGMA user_version = 7;",
        )?;
    }
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 7 {
        connection.execute_batch(
            "ALTER TABLE transition ADD COLUMN alignment TEXT NOT NULL DEFAULT 'centered' CHECK(alignment IN ('start_at_cut', 'centered', 'end_at_cut'));
             PRAGMA user_version = 8;",
        )?;
    }
    Ok(())
}

fn check_storage_version(connection: &Connection) -> Result<(), PersistenceError> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        return Err(PersistenceError::MissingProject);
    }
    if version > STORAGE_SCHEMA_VERSION {
        return Err(PersistenceError::UnsupportedStorageSchema {
            found: version,
            supported: STORAGE_SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn check_writable_storage_version(connection: &Connection) -> Result<(), PersistenceError> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > STORAGE_SCHEMA_VERSION {
        return Err(PersistenceError::UnsupportedStorageSchema {
            found: version,
            supported: STORAGE_SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn validate_database(connection: &Connection) -> Result<(), PersistenceError> {
    let check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(PersistenceError::Integrity(check));
    }
    let foreign_key_error = connection
        .query_row("PRAGMA foreign_key_check", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    if let Some(table) = foreign_key_error {
        return Err(PersistenceError::Integrity(format!(
            "foreign-key violation in table {table}"
        )));
    }
    Ok(())
}

fn clear_project(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "DELETE FROM transition;
         DELETE FROM clip;
         DELETE FROM marker;
         DELETE FROM track;
         DELETE FROM sequence;
         DELETE FROM media;
         DELETE FROM project;",
    )
}

fn write_project(transaction: &Transaction<'_>, project: &Project) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO project(singleton, id, name, domain_schema, revision)
         VALUES(1, ?1, ?2, ?3, ?4)",
        params![
            project.id.as_uuid().to_string(),
            project.name,
            i64::from(project.schema_version),
            to_i64(project.revision(), "revision")?
        ],
    )?;
    for asset in project.media.values() {
        transaction.execute(
            "INSERT INTO media(
                 id, source_path, proxy_path, proxy_source_sha256, proxy_source_bytes
             ) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                asset.id.as_uuid().to_string(),
                asset.source_path,
                asset.proxy_path,
                asset
                    .proxy_source_fingerprint
                    .as_ref()
                    .map(|fingerprint| fingerprint.sha256.as_str()),
                asset
                    .proxy_source_fingerprint
                    .as_ref()
                    .map(|fingerprint| to_i64(fingerprint.byte_length, "proxy_source_bytes"))
                    .transpose()?
            ],
        )?;
    }
    for sequence in project.sequences.values() {
        let frame_rate =
            project
                .sequence_frame_rate(sequence.id)
                .ok_or(PersistenceError::Orphan {
                    child: "sequence settings",
                    parent: "sequence",
                    id: sequence.id.as_uuid().to_string(),
                })?;
        transaction.execute(
            "INSERT INTO sequence(id, name, frame_rate_value, frame_rate_scale)
             VALUES(?1, ?2, ?3, ?4)",
            params![
                sequence.id.as_uuid().to_string(),
                sequence.name,
                frame_rate.value(),
                i64::from(frame_rate.timescale())
            ],
        )?;
        write_markers(transaction, sequence)?;
        write_transitions(transaction, sequence)?;
        write_tracks(transaction, sequence)?;
    }
    Ok(())
}

fn write_markers(
    transaction: &Transaction<'_>,
    sequence: &Sequence,
) -> Result<(), PersistenceError> {
    for (position, marker) in sequence.markers.iter().enumerate() {
        transaction.execute(
            "INSERT INTO marker(id, sequence_id, position, time_value, time_scale, label)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                marker.id.as_uuid().to_string(),
                sequence.id.as_uuid().to_string(),
                to_i64_usize(position, "marker.position")?,
                marker.time.value(),
                i64::from(marker.time.timescale()),
                marker.label,
            ],
        )?;
    }
    Ok(())
}

fn write_transitions(
    transaction: &Transaction<'_>,
    sequence: &Sequence,
) -> Result<(), PersistenceError> {
    for (position, transition) in sequence.transitions.iter().enumerate() {
        transaction.execute(
            "INSERT INTO transition(id, sequence_id, position, outgoing_clip_id, incoming_clip_id, duration_value, duration_scale, alignment, kind)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                transition.id.as_uuid().to_string(),
                sequence.id.as_uuid().to_string(),
                to_i64_usize(position, "transition.position")?,
                transition.outgoing_clip.as_uuid().to_string(),
                transition.incoming_clip.as_uuid().to_string(),
                transition.duration.value(),
                i64::from(transition.duration.timescale()),
                match transition.alignment {
                    TransitionAlignment::StartAtCut => "start_at_cut",
                    TransitionAlignment::Centered => "centered",
                    TransitionAlignment::EndAtCut => "end_at_cut",
                },
                match transition.kind {
                    TransitionKind::Dissolve => "dissolve",
                },
            ],
        )?;
    }
    Ok(())
}

fn write_tracks(
    transaction: &Transaction<'_>,
    sequence: &Sequence,
) -> Result<(), PersistenceError> {
    for (track_position, track) in sequence.tracks.iter().enumerate() {
        transaction.execute(
            "INSERT INTO track(id, sequence_id, position, kind, locked, visible, muted)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                track.id.as_uuid().to_string(),
                sequence.id.as_uuid().to_string(),
                to_i64_usize(track_position, "track.position")?,
                match track.kind {
                    TrackKind::Video => "video",
                    TrackKind::Audio => "audio",
                },
                i64::from(track.locked),
                i64::from(track.visible),
                i64::from(track.muted)
            ],
        )?;
        for (clip_position, clip) in track.clips.iter().enumerate() {
            write_clip(transaction, track.id, clip_position, clip)?;
        }
    }
    Ok(())
}

fn write_clip(
    transaction: &Transaction<'_>,
    track_id: TrackId,
    position: usize,
    clip: &Clip,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO clip(
            id, track_id, position, media_id,
            timeline_value, timeline_scale, source_value, source_scale,
            duration_value, duration_scale
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            clip.id.as_uuid().to_string(),
            track_id.as_uuid().to_string(),
            to_i64_usize(position, "clip.position")?,
            clip.media_id.as_uuid().to_string(),
            clip.timeline_start.value(),
            i64::from(clip.timeline_start.timescale()),
            clip.source_start.value(),
            i64::from(clip.source_start.timescale()),
            clip.source_duration.value(),
            i64::from(clip.source_duration.timescale())
        ],
    )?;
    Ok(())
}

struct RootRow {
    id: ProjectId,
    name: String,
    domain_schema: u32,
    revision: u64,
}

fn read_root(connection: &Connection) -> Result<RootRow, PersistenceError> {
    let raw = connection
        .query_row(
            "SELECT id, name, domain_schema, revision FROM project WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(PersistenceError::MissingProject)?;
    Ok(RootRow {
        id: ProjectId::from_uuid(parse_uuid("project.id", &raw.0)?),
        name: raw.1,
        domain_schema: to_u32(raw.2, "project.domain_schema")?,
        revision: to_u64(raw.3, "project.revision")?,
    })
}

fn read_media(
    connection: &Connection,
) -> Result<BTreeMap<MediaAssetId, MediaAsset>, PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT id, source_path, proxy_path, proxy_source_sha256, proxy_source_bytes
             FROM media ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut media = BTreeMap::new();
    for row in rows {
        let (raw_id, source_path, proxy_path, proxy_source_sha256, proxy_source_bytes) = row?;
        let proxy_source_fingerprint = match (proxy_source_sha256, proxy_source_bytes) {
            (Some(sha256), Some(byte_length)) => Some(ProxySourceFingerprint {
                sha256,
                byte_length: to_u64(byte_length, "media.proxy_source_bytes")?,
            }),
            (None, None) => None,
            _ => {
                return Err(PersistenceError::Integrity(
                    "proxy source fingerprint columns must both be present or absent".to_owned(),
                ));
            }
        };
        let id = MediaAssetId::from_uuid(parse_uuid("media.id", &raw_id)?);
        media.insert(
            id,
            MediaAsset {
                id,
                source_path,
                proxy_path,
                proxy_source_fingerprint,
            },
        );
    }
    Ok(media)
}

fn read_sequences(connection: &Connection) -> Result<StoredSequences, PersistenceError> {
    let mut statement = connection
        .prepare("SELECT id, name, frame_rate_value, frame_rate_scale FROM sequence ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut sequences = BTreeMap::new();
    let mut frame_rates = BTreeMap::new();
    for row in rows {
        let (raw_id, name, frame_rate_value, frame_rate_scale) = row?;
        let id = SequenceId::from_uuid(parse_uuid("sequence.id", &raw_id)?);
        let frame_rate = RationalTime::new(
            frame_rate_value,
            to_u32(frame_rate_scale, "sequence.frame_rate_scale")?,
        )?;
        sequences.insert(
            id,
            Sequence {
                id,
                name,
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: Vec::new(),
            },
        );
        frame_rates.insert(id, frame_rate);
    }
    Ok((sequences, frame_rates))
}

fn read_markers(
    connection: &Connection,
    sequences: &mut BTreeMap<SequenceId, Sequence>,
) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT id, sequence_id, time_value, time_scale, label
         FROM marker ORDER BY sequence_id, position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (raw_id, raw_sequence_id, time_value, time_scale, label) = row?;
        let sequence_id =
            SequenceId::from_uuid(parse_uuid("marker.sequence_id", &raw_sequence_id)?);
        let sequence = sequences
            .get_mut(&sequence_id)
            .ok_or(PersistenceError::Orphan {
                child: "marker",
                parent: "sequence",
                id: raw_sequence_id,
            })?;
        sequence.markers.push(Marker {
            id: MarkerId::from_uuid(parse_uuid("marker.id", &raw_id)?),
            time: read_time(time_value, time_scale, "marker.time_scale")?,
            label,
        });
    }
    Ok(())
}

fn read_transitions(
    connection: &Connection,
    sequences: &mut BTreeMap<SequenceId, Sequence>,
) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT id, sequence_id, outgoing_clip_id, incoming_clip_id, duration_value, duration_scale, alignment, kind
         FROM transition ORDER BY sequence_id, position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    for row in rows {
        let (
            raw_id,
            raw_sequence_id,
            raw_outgoing,
            raw_incoming,
            duration_value,
            duration_scale,
            raw_alignment,
            raw_kind,
        ) = row?;
        let sequence_id =
            SequenceId::from_uuid(parse_uuid("transition.sequence_id", &raw_sequence_id)?);
        let sequence = sequences
            .get_mut(&sequence_id)
            .ok_or(PersistenceError::Orphan {
                child: "transition",
                parent: "sequence",
                id: raw_sequence_id,
            })?;
        sequence.transitions.push(Transition {
            id: TransitionId::from_uuid(parse_uuid("transition.id", &raw_id)?),
            outgoing_clip: ClipId::from_uuid(parse_uuid(
                "transition.outgoing_clip_id",
                &raw_outgoing,
            )?),
            incoming_clip: ClipId::from_uuid(parse_uuid(
                "transition.incoming_clip_id",
                &raw_incoming,
            )?),
            duration: read_time(duration_value, duration_scale, "transition.duration_scale")?,
            alignment: parse_transition_alignment(&raw_alignment)?,
            kind: parse_transition_kind(&raw_kind)?,
        });
    }
    Ok(())
}

fn read_tracks(
    connection: &Connection,
    sequences: &mut BTreeMap<SequenceId, Sequence>,
) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT id, sequence_id, kind, locked, visible, muted FROM track ORDER BY sequence_id, position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, bool>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;
    for row in rows {
        let (raw_id, raw_sequence_id, raw_kind, locked, visible, muted) = row?;
        let sequence_id = SequenceId::from_uuid(parse_uuid("track.sequence_id", &raw_sequence_id)?);
        let sequence = sequences
            .get_mut(&sequence_id)
            .ok_or(PersistenceError::Orphan {
                child: "track",
                parent: "sequence",
                id: raw_sequence_id,
            })?;
        sequence.tracks.push(Track {
            id: TrackId::from_uuid(parse_uuid("track.id", &raw_id)?),
            kind: parse_track_kind(&raw_kind)?,
            clips: Vec::new(),
            locked,
            visible,
            muted,
        });
    }
    Ok(())
}

fn read_clips(
    connection: &Connection,
    sequences: &mut BTreeMap<SequenceId, Sequence>,
    media: &BTreeMap<MediaAssetId, MediaAsset>,
) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT id, track_id, media_id, timeline_value, timeline_scale,
                source_value, source_scale, duration_value, duration_scale
         FROM clip ORDER BY track_id, position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
        ))
    })?;
    for row in rows {
        let raw = row?;
        let track_id = TrackId::from_uuid(parse_uuid("clip.track_id", &raw.1)?);
        let track = find_track(sequences, track_id).ok_or(PersistenceError::Orphan {
            child: "clip",
            parent: "track",
            id: raw.1,
        })?;
        let media_id = MediaAssetId::from_uuid(parse_uuid("clip.media_id", &raw.2)?);
        if !media.contains_key(&media_id) {
            return Err(PersistenceError::Orphan {
                child: "clip",
                parent: "media",
                id: raw.2,
            });
        }
        track.clips.push(Clip {
            id: ClipId::from_uuid(parse_uuid("clip.id", &raw.0)?),
            media_id,
            timeline_start: read_time(raw.3, raw.4, "clip.timeline_scale")?,
            source_start: read_time(raw.5, raw.6, "clip.source_scale")?,
            source_duration: read_time(raw.7, raw.8, "clip.duration_scale")?,
        });
    }
    Ok(())
}

fn find_track(
    sequences: &mut BTreeMap<SequenceId, Sequence>,
    track_id: TrackId,
) -> Option<&mut Track> {
    sequences.values_mut().find_map(|sequence| {
        sequence
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
    })
}

fn parse_track_kind(value: &str) -> Result<TrackKind, PersistenceError> {
    match value {
        "video" => Ok(TrackKind::Video),
        "audio" => Ok(TrackKind::Audio),
        _ => Err(PersistenceError::InvalidEnum {
            field: "track.kind",
            value: value.to_owned(),
        }),
    }
}

fn parse_transition_kind(value: &str) -> Result<TransitionKind, PersistenceError> {
    match value {
        "dissolve" => Ok(TransitionKind::Dissolve),
        _ => Err(PersistenceError::InvalidEnum {
            field: "transition.kind",
            value: value.to_owned(),
        }),
    }
}

fn parse_transition_alignment(value: &str) -> Result<TransitionAlignment, PersistenceError> {
    match value {
        "start_at_cut" => Ok(TransitionAlignment::StartAtCut),
        "centered" => Ok(TransitionAlignment::Centered),
        "end_at_cut" => Ok(TransitionAlignment::EndAtCut),
        _ => Err(PersistenceError::InvalidEnum {
            field: "transition.alignment",
            value: value.to_owned(),
        }),
    }
}

fn read_time(
    value: i64,
    scale: i64,
    field: &'static str,
) -> Result<RationalTime, PersistenceError> {
    RationalTime::new(value, to_u32(scale, field)?).map_err(PersistenceError::from)
}

fn parse_uuid(field: &'static str, value: &str) -> Result<Uuid, PersistenceError> {
    Uuid::parse_str(value).map_err(|_| PersistenceError::InvalidUuid {
        field,
        value: value.to_owned(),
    })
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, PersistenceError> {
    i64::try_from(value).map_err(|_| PersistenceError::InvalidInteger {
        field,
        value: i64::MAX,
    })
}

fn to_i64_usize(value: usize, field: &'static str) -> Result<i64, PersistenceError> {
    i64::try_from(value).map_err(|_| PersistenceError::InvalidInteger {
        field,
        value: i64::MAX,
    })
}

fn to_u32(value: i64, field: &'static str) -> Result<u32, PersistenceError> {
    u32::try_from(value).map_err(|_| PersistenceError::InvalidInteger { field, value })
}

fn to_u64(value: i64, field: &'static str) -> Result<u64, PersistenceError> {
    u64::try_from(value).map_err(|_| PersistenceError::InvalidInteger { field, value })
}

#[cfg(test)]
mod tests;
