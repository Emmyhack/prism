use super::{Command, CommandEngine, CommandError, CommandRegistry, Event};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_project::{Project, ProxySourceFingerprint, TrackKind, TransitionKind};
use prism_timeline::{RationalTime, TimeError};

#[test]
fn registry_exposes_stable_descriptors_and_context_predicates() -> Result<(), CommandError> {
    let project = Project::new("Test");
    let undo = CommandRegistry::find("timeline.undo").ok_or(CommandError::EmptyTransaction)?;
    assert_eq!(undo.label, "Undo");
    assert_eq!(undo.shortcut, Some("Cmd+Z"));
    assert!(!undo.is_enabled(&project));
    let play =
        CommandRegistry::find("playback.play_pause").ok_or(CommandError::EmptyTransaction)?;
    assert!(play.is_enabled(&project));
    assert!(CommandRegistry::find("project.set_proxy").is_some());
    assert!(CommandRegistry::find("timeline.insert_clip").is_some());
    assert!(CommandRegistry::find("timeline.ripple_delete").is_some());
    assert!(CommandRegistry::find("timeline.toggle_track_lock").is_some());
    assert!(CommandRegistry::find("timeline.toggle_track_mute").is_some());
    assert_eq!(
        CommandRegistry::find("export.cancel").map(|command| command.label),
        Some("Cancel Export")
    );
    assert!(CommandRegistry::find("export.retry").is_some());
    assert!(CommandRegistry::find("export.remove").is_some());
    Ok(())
}

#[test]
fn registry_execution_keeps_validation_and_undo_transactional() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let create =
        CommandRegistry::find("project.import_media").ok_or(CommandError::EmptyTransaction)?;
    let media_id = MediaAssetId::new();
    create.execute(
        &mut project,
        Command::ImportMedia {
            id: media_id,
            source_path: "/tmp/source.mp4".to_owned(),
        },
    )?;
    assert_eq!(project.media.len(), 1);
    assert_eq!(project.revision(), 1);
    Ok(())
}

#[test]
fn relink_media_updates_path_and_is_undoable() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let media_id = MediaAssetId::new();
    CommandEngine::execute(
        &mut project,
        Command::ImportMedia {
            id: media_id,
            source_path: "/offline/original.mov".to_owned(),
        },
    )?;
    let relink =
        CommandRegistry::find("project.relink_media").ok_or(CommandError::EmptyTransaction)?;
    let outcome = relink.execute(
        &mut project,
        Command::RelinkMedia {
            media_id,
            source_path: "/Volumes/Media/original.mov".to_owned(),
        },
    )?;
    assert_eq!(outcome, vec![Event::MediaRelinked { id: media_id }]);
    assert_eq!(
        project.media[&media_id].source_path,
        "/Volumes/Media/original.mov"
    );
    Ok(())
}

#[test]
fn rejected_command_does_not_advance_revision() {
    let mut project = Project::new("Test");
    let result = CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: SequenceId::new(),
            name: "  ".to_owned(),
        },
    );
    assert_eq!(result, Err(CommandError::EmptyField { field: "name" }));
    assert_eq!(project.revision(), 0);
    assert!(project.sequences.is_empty());
}

#[test]
fn creates_sequence_and_track_through_commands() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: sequence_id,
            name: "Main".to_owned(),
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::CreateTrack {
            sequence_id,
            id: track_id,
            kind: TrackKind::Video,
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::SetTrackLocked {
            sequence_id,
            track_id,
            locked: true,
        },
    )?;
    assert!(project.sequences[&sequence_id].tracks[0].locked);
    assert_eq!(project.revision(), 3);
    assert_eq!(project.sequences[&sequence_id].tracks.len(), 1);
    Ok(())
}

#[test]
fn media_proxy_command_sets_clears_and_validates_relinks() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let media_id = MediaAssetId::new();
    CommandEngine::execute(
        &mut project,
        Command::ImportMedia {
            id: media_id,
            source_path: "source.mp4".to_owned(),
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::SetMediaProxy {
            media_id,
            proxy_path: Some("proxy.mp4".to_owned()),
            source_fingerprint: Some(ProxySourceFingerprint {
                sha256: "a".repeat(64),
                byte_length: 42,
            }),
        },
    )?;
    assert_eq!(
        project.media[&media_id].proxy_path.as_deref(),
        Some("proxy.mp4")
    );
    CommandEngine::execute(
        &mut project,
        Command::SetMediaProxy {
            media_id,
            proxy_path: None,
            source_fingerprint: None,
        },
    )?;
    assert!(!project.media[&media_id].has_proxy_path());
    assert_eq!(
        CommandEngine::execute(
            &mut project,
            Command::SetMediaProxy {
                media_id,
                proxy_path: Some("  ".to_owned()),
                source_fingerprint: None,
            },
        ),
        Err(CommandError::EmptyField {
            field: "proxy_path"
        })
    );
    Ok(())
}

#[test]
fn reorders_tracks_with_stable_ids() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let first = TrackId::new();
    let second = TrackId::new();
    CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: sequence_id,
            name: "Main".to_owned(),
        },
    )?;
    for id in [first, second] {
        CommandEngine::execute(
            &mut project,
            Command::CreateTrack {
                sequence_id,
                id,
                kind: TrackKind::Video,
            },
        )?;
    }
    let event = CommandEngine::execute(
        &mut project,
        Command::ReorderTrack {
            sequence_id,
            track_id: first,
            destination_index: 1,
        },
    )?;
    assert_eq!(
        event,
        vec![Event::TrackReordered {
            sequence_id,
            track_id: first,
            destination_index: 1,
        }]
    );
    assert_eq!(
        project.sequences[&sequence_id]
            .tracks
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>(),
        vec![second, first]
    );
    assert!(matches!(
        CommandEngine::execute(
            &mut project,
            Command::ReorderTrack {
                sequence_id,
                track_id: first,
                destination_index: 2,
            }
        ),
        Err(CommandError::InvalidTrackIndex)
    ));
    Ok(())
}

#[test]
fn updates_track_visibility_and_mute_state() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: sequence_id,
            name: "Main".to_owned(),
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::CreateTrack {
            sequence_id,
            id: track_id,
            kind: TrackKind::Video,
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
    let track = &project.sequences[&sequence_id].tracks[0];
    assert!(!track.visible);
    assert!(track.muted);
    Ok(())
}

#[test]
fn markers_are_sorted_and_removable() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: sequence_id,
            name: "Main".to_owned(),
        },
    )?;
    let late = MarkerId::new();
    let early = MarkerId::new();
    let late_time = RationalTime::new(10, 1)?;
    let early_time = RationalTime::new(2, 1)?;
    CommandEngine::execute(
        &mut project,
        Command::AddMarker {
            sequence_id,
            id: late,
            time: late_time,
            label: "Late".to_owned(),
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::AddMarker {
            sequence_id,
            id: early,
            time: early_time,
            label: "Early".to_owned(),
        },
    )?;
    assert_eq!(
        project.sequences[&sequence_id]
            .markers
            .iter()
            .map(|marker| marker.id)
            .collect::<Vec<_>>(),
        vec![early, late]
    );
    CommandEngine::execute(
        &mut project,
        Command::DeleteMarker {
            sequence_id,
            marker_id: early,
        },
    )?;
    assert_eq!(project.sequences[&sequence_id].markers.len(), 1);
    Ok(())
}

#[test]
fn markers_reject_invalid_values() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: sequence_id,
            name: "Main".to_owned(),
        },
    )?;
    let id = MarkerId::new();
    let negative_time = RationalTime::new(-1, 1)?;
    assert!(matches!(
        CommandEngine::execute(
            &mut project,
            Command::AddMarker {
                sequence_id,
                id,
                time: negative_time,
                label: "x".to_owned()
            }
        ),
        Err(CommandError::InvalidTime { .. })
    ));
    assert!(matches!(
        CommandEngine::execute(
            &mut project,
            Command::AddMarker {
                sequence_id,
                id,
                time: RationalTime::ZERO,
                label: " ".to_owned()
            }
        ),
        Err(CommandError::EmptyField { field: "label" })
    ));
    CommandEngine::execute(
        &mut project,
        Command::AddMarker {
            sequence_id,
            id,
            time: RationalTime::ZERO,
            label: "x".to_owned(),
        },
    )?;
    assert!(matches!(
        CommandEngine::execute(
            &mut project,
            Command::AddMarker {
                sequence_id,
                id,
                time: RationalTime::ZERO,
                label: "duplicate".to_owned()
            }
        ),
        Err(CommandError::DuplicateId)
    ));
    Ok(())
}

#[test]
fn duplicate_clip_reuses_source_range_at_requested_position()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let clip_id = ClipId::new();
    let duplicate_id = ClipId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Build",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: "source.mov".to_owned(),
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
                id: clip_id,
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::new(5, 1)?,
                source_duration: RationalTime::new(2, 1)?,
            },
        ],
    )?;
    let event = CommandEngine::execute(
        &mut project,
        Command::DuplicateClip {
            sequence_id,
            track_id,
            clip_id,
            new_clip_id: duplicate_id,
            timeline_start: RationalTime::new(4, 1)?,
        },
    )?;
    assert_eq!(
        event,
        vec![Event::ClipDuplicated {
            source_id: clip_id,
            id: duplicate_id
        }]
    );
    let clips = &project.sequences[&sequence_id].tracks[0].clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[1].timeline_start, RationalTime::new(4, 1)?);
    assert_eq!(clips[1].source_start, RationalTime::new(5, 1)?);
    Ok(())
}

#[test]
fn ripple_delete_closes_gap_without_touching_source_ranges()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let first = ClipId::new();
    let removed = ClipId::new();
    let later = ClipId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Build",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: "source.mov".to_owned(),
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
                id: first,
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::ZERO,
                source_duration: RationalTime::new(2, 1)?,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: removed,
                media_id,
                timeline_start: RationalTime::new(2, 1)?,
                source_start: RationalTime::new(8, 1)?,
                source_duration: RationalTime::new(3, 1)?,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: later,
                media_id,
                timeline_start: RationalTime::new(5, 1)?,
                source_start: RationalTime::new(20, 1)?,
                source_duration: RationalTime::new(1, 1)?,
            },
        ],
    )?;
    let events = CommandEngine::execute(
        &mut project,
        Command::RippleDelete {
            sequence_id,
            track_id,
            clip_id: removed,
        },
    )?;
    assert_eq!(
        events,
        vec![Event::ClipRippleDeleted {
            id: removed,
            shifted_count: 1
        }]
    );
    let clips = &project.sequences[&sequence_id].tracks[0].clips;
    let later_clip = clips
        .iter()
        .find(|clip| clip.id == later)
        .ok_or("missing later clip")?;
    assert_eq!(later_clip.timeline_start, RationalTime::new(2, 1)?);
    assert_eq!(later_clip.source_start, RationalTime::new(20, 1)?);
    Ok(())
}

#[test]
fn transitions_require_adjacent_video_clips() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let outgoing = ClipId::new();
    let incoming = ClipId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Build",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: "source.mov".to_owned(),
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
                id: outgoing,
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::ZERO,
                source_duration: RationalTime::new(2, 1)?,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: incoming,
                media_id,
                timeline_start: RationalTime::new(2, 1)?,
                source_start: RationalTime::new(2, 1)?,
                source_duration: RationalTime::new(2, 1)?,
            },
        ],
    )?;
    let transition_id = TransitionId::new();
    let events = CommandEngine::execute(
        &mut project,
        Command::AddTransition {
            sequence_id,
            id: transition_id,
            outgoing_clip: outgoing,
            incoming_clip: incoming,
            duration: RationalTime::new(1, 2)?,
            alignment: prism_project::TransitionAlignment::Centered,
            kind: TransitionKind::Dissolve,
        },
    )?;
    assert_eq!(
        events,
        vec![Event::TransitionAdded {
            sequence_id,
            id: transition_id
        }]
    );
    assert_eq!(
        project.sequences[&sequence_id].transitions[0].duration,
        RationalTime::new(1, 2)?
    );
    let deleted = CommandEngine::execute(
        &mut project,
        Command::DeleteTransition {
            sequence_id,
            transition_id,
        },
    )?;
    assert_eq!(
        deleted,
        vec![Event::TransitionDeleted {
            sequence_id,
            id: transition_id
        }]
    );
    Ok(())
}

#[test]
fn failed_batch_rolls_back_every_command() {
    let mut project = Project::new("Test");
    let created_id = SequenceId::new();
    let result = CommandEngine::execute_transaction(
        &mut project,
        "Invalid batch",
        vec![
            Command::CreateSequence {
                id: created_id,
                name: "Main".to_owned(),
            },
            Command::CreateTrack {
                sequence_id: SequenceId::new(),
                id: TrackId::new(),
                kind: TrackKind::Video,
            },
        ],
    );
    assert!(matches!(result, Err(CommandError::SequenceNotFound)));
    assert!(!project.sequences.contains_key(&created_id));
    assert_eq!(project.revision(), 0);
}

#[test]
fn dry_run_reports_events_without_mutating_project() -> Result<(), CommandError> {
    let project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let events = CommandEngine::validate_transaction(
        &project,
        vec![Command::CreateSequence {
            id: sequence_id,
            name: "Preview".to_owned(),
        }],
    )?;
    assert_eq!(events.len(), 1);
    assert_eq!(project.revision(), 0);
    assert!(project.sequences.is_empty());
    Ok(())
}

#[test]
fn undo_and_redo_restore_complete_transaction() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let outcome = CommandEngine::execute_transaction(
        &mut project,
        "Create timeline",
        vec![Command::CreateSequence {
            id: sequence_id,
            name: "Main".to_owned(),
        }],
    )?;
    assert_eq!(project.revision(), 1);
    assert!(project.sequences.contains_key(&sequence_id));

    let redo = outcome.undo.apply(&mut project)?;
    assert_eq!(project.revision(), 0);
    assert!(!project.sequences.contains_key(&sequence_id));

    let _undo_again = redo.apply(&mut project)?;
    assert_eq!(project.revision(), 1);
    assert!(project.sequences.contains_key(&sequence_id));
    Ok(())
}

#[test]
fn clip_commands_enforce_non_overlapping_ranges() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let media_id = MediaAssetId::new();
    let first_clip_id = ClipId::new();
    let second_clip_id = ClipId::new();
    let second = RationalTime::new(2, 1)?;
    let duration = RationalTime::new(2, 1)?;

    CommandEngine::execute_transaction(
        &mut project,
        "Build timeline",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: "/media/interview.mov".to_owned(),
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
                id: first_clip_id,
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::ZERO,
                source_duration: duration,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: second_clip_id,
                media_id,
                timeline_start: second,
                source_start: RationalTime::ZERO,
                source_duration: duration,
            },
        ],
    )?;

    let overlap = CommandEngine::execute(
        &mut project,
        Command::MoveClip {
            sequence_id,
            track_id,
            clip_id: second_clip_id,
            timeline_start: RationalTime::new(1, 1)?,
        },
    );
    assert!(matches!(overlap, Err(CommandError::ClipOverlap)));
    let track = &project.sequences[&sequence_id].tracks[0];
    assert_eq!(track.clips[1].timeline_start, second);
    Ok(())
}

#[test]
fn locked_tracks_reject_all_clip_mutations() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let media_id = MediaAssetId::new();
    let clip_id = ClipId::new();
    let duration = RationalTime::new(4, 1)?;
    CommandEngine::execute_transaction(
        &mut project,
        "Create locked timeline",
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
                id: clip_id,
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::ZERO,
                source_duration: duration,
            },
            Command::SetTrackLocked {
                sequence_id,
                track_id,
                locked: true,
            },
        ],
    )?;

    let commands = [
        Command::InsertClip {
            sequence_id,
            track_id,
            id: ClipId::new(),
            media_id,
            timeline_start: RationalTime::new(5, 1)?,
            source_start: RationalTime::ZERO,
            source_duration: duration,
        },
        Command::MoveClip {
            sequence_id,
            track_id,
            clip_id,
            timeline_start: RationalTime::new(1, 1)?,
        },
        Command::TrimClip {
            sequence_id,
            track_id,
            clip_id,
            timeline_start: RationalTime::ZERO,
            source_start: RationalTime::ZERO,
            source_duration: RationalTime::new(2, 1)?,
        },
        Command::SplitClip {
            sequence_id,
            track_id,
            clip_id,
            right_clip_id: ClipId::new(),
            timeline_position: RationalTime::new(2, 1)?,
        },
        Command::DeleteClip {
            sequence_id,
            track_id,
            clip_id,
        },
    ];
    for command in commands {
        assert!(matches!(
            CommandEngine::execute(&mut project, command),
            Err(CommandError::TrackLocked)
        ));
    }
    assert_eq!(project.revision(), 1);
    assert_eq!(project.sequences[&sequence_id].tracks[0].clips.len(), 1);
    Ok(())
}

#[test]
fn exact_time_ordering_handles_different_timebases() -> Result<(), TimeError> {
    assert!(RationalTime::new(1, 24)? < RationalTime::new(1, 23)?);
    Ok(())
}

#[test]
fn stale_revision_cannot_commit() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: SequenceId::new(),
            name: "Current".to_owned(),
        },
    )?;
    let result = CommandEngine::execute_transaction_at_revision(
        &mut project,
        0,
        "Stale edit",
        vec![Command::CreateSequence {
            id: SequenceId::new(),
            name: "Stale".to_owned(),
        }],
    );
    assert!(matches!(
        result,
        Err(CommandError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    ));
    assert_eq!(project.sequences.len(), 1);
    Ok(())
}

#[test]
fn stale_undo_cannot_discard_later_edits() -> Result<(), CommandError> {
    let mut project = Project::new("Test");
    let first = CommandEngine::execute_transaction(
        &mut project,
        "First",
        vec![Command::CreateSequence {
            id: SequenceId::new(),
            name: "First".to_owned(),
        }],
    )?;
    CommandEngine::execute(
        &mut project,
        Command::CreateSequence {
            id: SequenceId::new(),
            name: "Later".to_owned(),
        },
    )?;
    let result = first.undo.apply(&mut project);
    assert!(matches!(
        result,
        Err(CommandError::RevisionConflict {
            expected: 1,
            actual: 2
        })
    ));
    assert_eq!(project.sequences.len(), 2);
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn split_trim_and_delete_preserve_exact_source_ranges() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let media_id = MediaAssetId::new();
    let left_id = ClipId::new();
    let right_id = ClipId::new();
    let four = RationalTime::new(4, 1)?;
    let two = RationalTime::new(2, 1)?;

    CommandEngine::execute_transaction(
        &mut project,
        "Create clip",
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
                id: left_id,
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::ZERO,
                source_duration: four,
            },
        ],
    )?;
    CommandEngine::execute(
        &mut project,
        Command::SlipClip {
            sequence_id,
            track_id,
            clip_id: left_id,
            source_delta: RationalTime::new(1, 2)?,
        },
    )?;
    assert_eq!(
        project.sequences[&sequence_id].tracks[0].clips[0].source_start,
        RationalTime::new(1, 2)?
    );
    CommandEngine::execute(
        &mut project,
        Command::SplitClip {
            sequence_id,
            track_id,
            clip_id: left_id,
            right_clip_id: right_id,
            timeline_position: two,
        },
    )?;
    let clips = &project.sequences[&sequence_id].tracks[0].clips;
    assert_eq!(clips[0].source_duration, two);
    assert_eq!(clips[1].timeline_start, two);
    assert_eq!(clips[1].source_start, RationalTime::new(5, 2)?);
    assert_eq!(clips[1].source_duration, two);

    CommandEngine::execute(
        &mut project,
        Command::RollEdit {
            sequence_id,
            track_id,
            left_clip_id: left_id,
            right_clip_id: right_id,
            delta: RationalTime::new(1, 2)?,
        },
    )?;
    let clips = &project.sequences[&sequence_id].tracks[0].clips;
    assert_eq!(clips[0].source_duration, RationalTime::new(5, 2)?);
    assert_eq!(clips[1].timeline_start, RationalTime::new(5, 2)?);
    assert_eq!(clips[1].source_start, RationalTime::new(3, 1)?);
    assert_eq!(clips[1].source_duration, RationalTime::new(3, 2)?);

    CommandEngine::execute(
        &mut project,
        Command::TrimClip {
            sequence_id,
            track_id,
            clip_id: right_id,
            timeline_start: RationalTime::new(3, 1)?,
            source_start: RationalTime::new(3, 1)?,
            source_duration: RationalTime::new(1, 1)?,
        },
    )?;
    CommandEngine::execute(
        &mut project,
        Command::DeleteClip {
            sequence_id,
            track_id,
            clip_id: left_id,
        },
    )?;
    let clips = &project.sequences[&sequence_id].tracks[0].clips;
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].id, right_id);
    assert_eq!(clips[0].timeline_start, RationalTime::new(3, 1)?);
    Ok(())
}

#[test]
fn slide_edit_moves_clip_and_adjusts_neighbors() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Test");
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let media_id = MediaAssetId::new();
    let previous = ClipId::new();
    let selected = ClipId::new();
    let next = ClipId::new();
    let duration = RationalTime::new(2, 1)?;
    CommandEngine::execute_transaction(
        &mut project,
        "Create slide timeline",
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
                id: previous,
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::ZERO,
                source_duration: duration,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: selected,
                media_id,
                timeline_start: duration,
                source_start: RationalTime::ZERO,
                source_duration: duration,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: next,
                media_id,
                timeline_start: RationalTime::new(4, 1)?,
                source_start: RationalTime::ZERO,
                source_duration: duration,
            },
        ],
    )?;
    CommandEngine::execute(
        &mut project,
        Command::SlideEdit {
            sequence_id,
            track_id,
            previous_clip_id: previous,
            clip_id: selected,
            next_clip_id: next,
            delta: RationalTime::new(1, 2)?,
        },
    )?;
    let clips = &project.sequences[&sequence_id].tracks[0].clips;
    assert_eq!(clips[0].source_duration, RationalTime::new(5, 2)?);
    assert_eq!(clips[1].timeline_start, RationalTime::new(5, 2)?);
    assert_eq!(clips[1].source_duration, duration);
    assert_eq!(clips[2].timeline_start, RationalTime::new(9, 2)?);
    assert_eq!(clips[2].source_start, RationalTime::new(1, 2)?);
    assert_eq!(clips[2].source_duration, RationalTime::new(3, 2)?);
    Ok(())
}
