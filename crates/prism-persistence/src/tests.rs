use super::{
    AutosaveScheduler, AutosaveSubmit, PersistenceError, initialize, list_autosaves, load,
    load_from_connection, recover_autosave, retain_autosaves, save_atomic, save_autosave,
    save_backup, save_to_connection, upgrade_storage_schema,
};
use prism_commands::{Command, CommandEngine};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_project::{Project, ProxySourceFingerprint, TrackKind, TransitionKind};
use prism_timeline::RationalTime;
use rusqlite::Connection;
use std::time::Duration;

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
            Command::SetSequenceFrameRate {
                sequence_id,
                frame_rate: RationalTime::new(30_000, 1_001)?,
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
#[allow(clippy::too_many_lines)]
fn sqlite_round_trip_preserves_domain_state() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = sample_project()?;
    let sequence_id = *project.sequences.keys().next().ok_or("missing sequence")?;
    let track_id = project.sequences[&sequence_id].tracks[0].id;
    let marker_id = MarkerId::new();
    CommandEngine::execute(
        &mut project,
        Command::AddMarker {
            sequence_id,
            id: marker_id,
            time: RationalTime::new(3, 2)?,
            label: "Beat".to_owned(),
        },
    )?;
    let media_id = *project.media.keys().next().ok_or("missing media")?;
    project
        .media
        .get_mut(&media_id)
        .ok_or("missing media")?
        .proxy_path = Some("/cache/source-proxy.mp4".to_owned());
    project
        .media
        .get_mut(&media_id)
        .ok_or("missing media")?
        .proxy_source_fingerprint = Some(ProxySourceFingerprint {
        sha256: "a".repeat(64),
        byte_length: 42,
    });
    let first_clip_id = project.sequences[&sequence_id].tracks[0].clips[0].id;
    let second_clip_id = ClipId::new();
    CommandEngine::execute(
        &mut project,
        Command::InsertClip {
            sequence_id,
            track_id,
            id: second_clip_id,
            media_id,
            timeline_start: RationalTime::new(151_001, 30_000)?,
            source_start: RationalTime::new(5, 1)?,
            source_duration: RationalTime::new(2, 1)?,
        },
    )?;
    let transition_id = TransitionId::new();
    CommandEngine::execute(
        &mut project,
        Command::AddTransition {
            sequence_id,
            id: transition_id,
            outgoing_clip: first_clip_id,
            incoming_clip: second_clip_id,
            duration: RationalTime::new(1, 2)?,
            alignment: prism_project::TransitionAlignment::Centered,
            kind: TransitionKind::Dissolve,
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::SetTrackVisible {
            sequence_id,
            track_id,
            visible: false,
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::SetTrackMuted {
            sequence_id,
            track_id,
            muted: true,
        },
    )?;
    let mut connection = Connection::open_in_memory()?;
    save_to_connection(&mut connection, &project)?;
    let loaded = load_from_connection(&connection)?;
    assert_eq!(loaded.id, project.id);
    assert_eq!(loaded.name, project.name);
    assert_eq!(loaded.revision(), project.revision());
    assert_eq!(loaded.media.len(), 1);
    assert_eq!(
        loaded.sequence_frame_rate(sequence_id),
        Some(RationalTime::new(30_000, 1_001)?)
    );
    assert_eq!(
        loaded.media[&media_id].proxy_path.as_deref(),
        Some("/cache/source-proxy.mp4")
    );
    assert_eq!(
        loaded.media[&media_id].proxy_source_fingerprint,
        Some(ProxySourceFingerprint {
            sha256: "a".repeat(64),
            byte_length: 42,
        })
    );
    let loaded_sequence = loaded.sequences.values().next().ok_or("missing sequence")?;
    assert!(!loaded_sequence.tracks[0].visible);
    assert!(loaded_sequence.tracks[0].muted);
    assert_eq!(loaded_sequence.markers[0].id, marker_id);
    assert_eq!(loaded_sequence.markers[0].label, "Beat");
    assert_eq!(loaded_sequence.transitions[0].id, transition_id);
    assert_eq!(
        loaded_sequence.transitions[0].alignment,
        prism_project::TransitionAlignment::Centered
    );
    assert_eq!(
        loaded_sequence.transitions[0].kind,
        TransitionKind::Dissolve
    );
    assert_eq!(
        loaded_sequence.tracks[0].clips[0].source_duration,
        RationalTime::new(5, 1)?
    );
    Ok(())
}

#[test]
fn migrates_v1_track_state_with_safe_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
            "CREATE TABLE project(singleton INTEGER PRIMARY KEY, id TEXT NOT NULL, name TEXT NOT NULL, domain_schema INTEGER NOT NULL, revision INTEGER NOT NULL);
             CREATE TABLE media(id TEXT PRIMARY KEY, source_path TEXT NOT NULL);
             CREATE TABLE sequence(id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE track(id TEXT PRIMARY KEY, sequence_id TEXT NOT NULL, position INTEGER NOT NULL, kind TEXT NOT NULL, locked INTEGER NOT NULL CHECK(locked IN (0, 1)), UNIQUE(sequence_id, position));
             CREATE TABLE clip(id TEXT PRIMARY KEY, track_id TEXT NOT NULL, position INTEGER NOT NULL, media_id TEXT NOT NULL, timeline_value INTEGER NOT NULL, timeline_scale INTEGER NOT NULL, source_value INTEGER NOT NULL, source_scale INTEGER NOT NULL, duration_value INTEGER NOT NULL, duration_scale INTEGER NOT NULL);
             PRAGMA user_version = 1;
             INSERT INTO project VALUES(1, '00000000-0000-0000-0000-000000000001', 'Legacy', 1, 0);
             INSERT INTO sequence VALUES('00000000-0000-0000-0000-000000000002', 'Main');
             INSERT INTO track VALUES('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000002', 0, 'video', 0);",
        )?;
    let loaded = load_from_connection(&connection)?;
    let track = &loaded
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?
        .tracks[0];
    assert!(track.visible);
    assert!(!track.muted);
    let sequence_id = *loaded.sequences.keys().next().ok_or("missing sequence")?;
    assert_eq!(
        loaded.sequence_frame_rate(sequence_id),
        Some(RationalTime::from_integer(30))
    );
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, 8);
    Ok(())
}

#[test]
fn migrates_v7_transitions_to_centered_alignment() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(
        "CREATE TABLE transition (
                 id TEXT PRIMARY KEY,
                 sequence_id TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 outgoing_clip_id TEXT NOT NULL,
                 incoming_clip_id TEXT NOT NULL,
                 duration_value INTEGER NOT NULL,
                 duration_scale INTEGER NOT NULL,
                 kind TEXT NOT NULL
             );
             INSERT INTO transition VALUES(
                 '00000000-0000-0000-0000-000000000001',
                 '00000000-0000-0000-0000-000000000002', 0,
                 '00000000-0000-0000-0000-000000000003',
                 '00000000-0000-0000-0000-000000000004', 1, 2, 'dissolve'
             );
             PRAGMA user_version = 7;",
    )?;
    upgrade_storage_schema(&connection)?;
    let alignment: String =
        connection.query_row("SELECT alignment FROM transition", [], |row| row.get(0))?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(alignment, "centered");
    assert_eq!(version, 8);
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

#[test]
fn backup_is_atomic_and_never_replaces_existing_artifact() -> Result<(), Box<dyn std::error::Error>>
{
    let project = sample_project()?;
    let directory =
        std::env::temp_dir().join(format!("prism-backup-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let path = directory.join("revision-1.prism");
    save_backup(&project, &path)?;
    assert_eq!(load(&path)?.revision(), project.revision());
    assert!(matches!(
        save_backup(&project, &path),
        Err(PersistenceError::BackupExists(existing)) if existing == path
    ));
    std::fs::remove_file(path)?;
    std::fs::remove_dir(directory)?;
    Ok(())
}

#[test]
fn autosave_name_is_revision_scoped_and_no_clobber() -> Result<(), Box<dyn std::error::Error>> {
    let project = sample_project()?;
    let directory =
        std::env::temp_dir().join(format!("prism-autosave-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let active = directory.join("edit.prism");
    let autosave = save_autosave(&project, &active)?;
    assert_eq!(
        autosave.file_name().and_then(|name| name.to_str()),
        Some("edit.prism.autosave.1")
    );
    assert_eq!(load(&autosave)?.revision(), project.revision());
    assert!(matches!(
        save_autosave(&project, &active),
        Err(PersistenceError::BackupExists(_))
    ));
    std::fs::remove_file(autosave)?;
    std::fs::remove_dir(directory)?;
    Ok(())
}

#[test]
fn autosave_discovery_lists_newest_valid_candidates_only() -> Result<(), Box<dyn std::error::Error>>
{
    let directory =
        std::env::temp_dir().join(format!("prism-autosave-discovery-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let active = directory.join("edit.prism");
    for revision in [2_u64, 5, 3] {
        std::fs::write(
            directory.join(format!("edit.prism.autosave.{revision}")),
            b"candidate",
        )?;
    }
    std::fs::write(directory.join("edit.prism.autosave.bad"), b"ignore")?;
    std::fs::create_dir(directory.join("edit.prism.autosave.9"))?;
    let candidates = list_autosaves(&active)?;
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.revision)
            .collect::<Vec<_>>(),
        vec![5, 3, 2]
    );
    assert!(candidates.iter().all(|candidate| candidate.path.is_file()));
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn recovery_validates_autosave_and_never_clobbers_destination()
-> Result<(), Box<dyn std::error::Error>> {
    let project = sample_project()?;
    let directory =
        std::env::temp_dir().join(format!("prism-autosave-recovery-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let source = directory.join("edit.prism.autosave.1");
    let destination = directory.join("recovered.prism");
    save_atomic(&project, &source)?;
    assert_eq!(recover_autosave(&source, &destination)?, project.revision());
    assert_eq!(load(&destination)?.id, project.id);
    assert!(matches!(
        recover_autosave(&source, &destination),
        Err(PersistenceError::BackupExists(path)) if path == destination
    ));
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn autosave_scheduler_writes_revisions_off_thread_and_skips_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = sample_project()?;
    let directory =
        std::env::temp_dir().join(format!("prism-autosave-worker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let active = directory.join("edit.prism");
    let scheduler = AutosaveScheduler::spawn(&active, 2)?;
    assert_eq!(scheduler.active_path(), active.as_path());
    assert_eq!(scheduler.try_submit(&project), AutosaveSubmit::Queued);
    assert_eq!(scheduler.try_submit(&project), AutosaveSubmit::Queued);
    let first = scheduler
        .result_timeout(Duration::from_secs(2))
        .ok_or("autosave worker did not report")?;
    assert_eq!(first.revision, project.revision());
    assert!(first.result.is_ok());
    assert!(first.path.as_ref().is_some_and(|path| path.exists()));
    assert!(scheduler.try_result().is_none());

    project.commit_revision()?;
    assert_eq!(scheduler.try_submit(&project), AutosaveSubmit::Queued);
    let second = scheduler
        .result_timeout(Duration::from_secs(2))
        .ok_or("second autosave worker result missing")?;
    assert_eq!(second.revision, project.revision());
    assert!(second.result.is_ok());
    let first_path = directory.join("edit.prism.autosave.1");
    let second_path = directory.join("edit.prism.autosave.2");
    assert!(first_path.exists());
    assert!(second_path.exists());
    drop(scheduler);
    std::fs::remove_file(first_path)?;
    std::fs::remove_file(second_path)?;
    std::fs::remove_dir(directory)?;
    Ok(())
}

#[test]
fn autosave_retention_removes_oldest_matching_revisions_only()
-> Result<(), Box<dyn std::error::Error>> {
    let directory =
        std::env::temp_dir().join(format!("prism-autosave-retention-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let active = directory.join("edit.prism");
    for revision in [1_u64, 2, 3, 4] {
        std::fs::write(
            directory.join(format!("edit.prism.autosave.{revision}")),
            revision.to_string(),
        )?;
    }
    std::fs::write(directory.join("edit.prism.autosave.bad"), b"keep")?;
    std::fs::write(directory.join("unrelated.txt"), b"keep")?;
    let report = retain_autosaves(&active, 2)?;
    assert_eq!(report.removed.len(), 2);
    assert_eq!(report.retained.len(), 2);
    assert!(!directory.join("edit.prism.autosave.1").exists());
    assert!(!directory.join("edit.prism.autosave.2").exists());
    assert!(directory.join("edit.prism.autosave.3").exists());
    assert!(directory.join("edit.prism.autosave.4").exists());
    assert!(directory.join("edit.prism.autosave.bad").exists());
    assert!(directory.join("unrelated.txt").exists());
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
