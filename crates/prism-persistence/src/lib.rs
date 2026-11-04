//! `SQLite` persistence adapter for the UI-independent Prism domain model.

use prism_core::{ClipId, MediaAssetId, ProjectId, SequenceId, TrackId};
use prism_project::{
    Clip, MediaAsset, Project, ProjectError, RestoredProject, Sequence, Track, TrackKind,
};
use prism_timeline::{RationalTime, TimeError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};
use thiserror::Error;
use uuid::Uuid;

const STORAGE_SCHEMA_VERSION: u32 = 1;

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
    check_storage_version(connection)?;
    validate_database(connection)?;
    let root = read_root(connection)?;
    let media = read_media(connection)?;
    let mut sequences = read_sequences(connection)?;
    read_tracks(connection, &mut sequences)?;
    read_clips(connection, &mut sequences, &media)?;
    Project::restore(RestoredProject {
        id: root.id,
        name: root.name,
        schema_version: root.domain_schema,
        revision: root.revision,
        media,
        sequences,
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
             source_path TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS sequence (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS track (
             id TEXT PRIMARY KEY,
             sequence_id TEXT NOT NULL REFERENCES sequence(id) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('video', 'audio')),
             locked INTEGER NOT NULL CHECK(locked IN (0, 1)),
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
         PRAGMA user_version = 1;",
    )?;
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
        "DELETE FROM clip;
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
            "INSERT INTO media(id, source_path) VALUES(?1, ?2)",
            params![asset.id.as_uuid().to_string(), asset.source_path],
        )?;
    }
    for sequence in project.sequences.values() {
        transaction.execute(
            "INSERT INTO sequence(id, name) VALUES(?1, ?2)",
            params![sequence.id.as_uuid().to_string(), sequence.name],
        )?;
        write_tracks(transaction, sequence)?;
    }
    Ok(())
}

fn write_tracks(
    transaction: &Transaction<'_>,
    sequence: &Sequence,
) -> Result<(), PersistenceError> {
    for (track_position, track) in sequence.tracks.iter().enumerate() {
        transaction.execute(
            "INSERT INTO track(id, sequence_id, position, kind, locked)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                track.id.as_uuid().to_string(),
                sequence.id.as_uuid().to_string(),
                to_i64_usize(track_position, "track.position")?,
                match track.kind {
                    TrackKind::Video => "video",
                    TrackKind::Audio => "audio",
                },
                i64::from(track.locked)
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
    let mut statement = connection.prepare("SELECT id, source_path FROM media ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut media = BTreeMap::new();
    for row in rows {
        let (raw_id, source_path) = row?;
        let id = MediaAssetId::from_uuid(parse_uuid("media.id", &raw_id)?);
        media.insert(id, MediaAsset { id, source_path });
    }
    Ok(media)
}

fn read_sequences(
    connection: &Connection,
) -> Result<BTreeMap<SequenceId, Sequence>, PersistenceError> {
    let mut statement = connection.prepare("SELECT id, name FROM sequence ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut sequences = BTreeMap::new();
    for row in rows {
        let (raw_id, name) = row?;
        let id = SequenceId::from_uuid(parse_uuid("sequence.id", &raw_id)?);
        sequences.insert(
            id,
            Sequence {
                id,
                name,
                tracks: Vec::new(),
            },
        );
    }
    Ok(sequences)
}

fn read_tracks(
    connection: &Connection,
    sequences: &mut BTreeMap<SequenceId, Sequence>,
) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT id, sequence_id, kind, locked FROM track ORDER BY sequence_id, position",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
        ))
    })?;
    for row in rows {
        let (raw_id, raw_sequence_id, raw_kind, locked) = row?;
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
mod tests {
    use super::{
        PersistenceError, initialize, load, load_from_connection, save_atomic, save_to_connection,
    };
    use prism_commands::{Command, CommandEngine};
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Project, TrackKind};
    use prism_timeline::RationalTime;
    use rusqlite::Connection;

    fn sample_project() -> Result<Project, Box<dyn std::error::Error>> {
        let mut project = Project::new("Round trip");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        CommandEngine::execute_transaction(
            &mut project,
            "Fixture",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "/media/source.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: ClipId::new(),
                    media_id,
                    timeline_start: RationalTime::new(1_001, 30_000)?,
                    source_start: RationalTime::ZERO,
                    source_duration: RationalTime::new(5, 1)?,
                },
            ],
        )?;
        Ok(project)
    }

    #[test]
    fn sqlite_round_trip_preserves_domain_state() -> Result<(), Box<dyn std::error::Error>> {
        let project = sample_project()?;
        let mut connection = Connection::open_in_memory()?;
        save_to_connection(&mut connection, &project)?;
        let loaded = load_from_connection(&connection)?;
        assert_eq!(loaded.id, project.id);
        assert_eq!(loaded.name, project.name);
        assert_eq!(loaded.revision(), project.revision());
        assert_eq!(loaded.media.len(), 1);
        let loaded_sequence = loaded.sequences.values().next().ok_or("missing sequence")?;
        assert_eq!(
            loaded_sequence.tracks[0].clips[0].source_duration,
            RationalTime::new(5, 1)?
        );
        Ok(())
    }

    #[test]
    fn rejects_newer_storage_schema() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA user_version = 999;")?;
        assert!(matches!(
            load_from_connection(&connection),
            Err(PersistenceError::UnsupportedStorageSchema { found: 999, .. })
        ));
        Ok(())
    }

    #[test]
    fn save_does_not_downgrade_newer_storage() -> Result<(), Box<dyn std::error::Error>> {
        let project = sample_project()?;
        let mut connection = Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA user_version = 999;")?;
        assert!(matches!(
            save_to_connection(&mut connection, &project),
            Err(PersistenceError::UnsupportedStorageSchema { found: 999, .. })
        ));
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, 999);
        Ok(())
    }

    #[test]
    fn malformed_identifier_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        initialize(&connection)?;
        connection.execute(
            "INSERT INTO project(singleton, id, name, domain_schema, revision)
             VALUES(1, 'not-a-uuid', 'Broken', 1, 0)",
            [],
        )?;
        assert!(matches!(
            load_from_connection(&connection),
            Err(PersistenceError::InvalidUuid {
                field: "project.id",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn atomic_file_save_loads_complete_project() -> Result<(), Box<dyn std::error::Error>> {
        let project = sample_project()?;
        let directory = std::env::temp_dir().join(format!("prism-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let path = directory.join("project.prism");
        save_atomic(&project, &path)?;
        let loaded = load(&path)?;
        assert_eq!(loaded.id, project.id);
        std::fs::remove_file(path)?;
        std::fs::remove_dir(directory)?;
        Ok(())
    }
}
