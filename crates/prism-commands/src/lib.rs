//! Typed, validated project mutations shared by every interface.

use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind};
use prism_timeline::RationalTime;
use thiserror::Error;

/// Mutation requests accepted by the authoritative command engine.
#[derive(Debug)]
pub enum Command {
    /// Registers a nondestructive reference to source media.
    ImportMedia {
        /// Stable asset ID.
        id: MediaAssetId,
        /// Source path selected by the user.
        source_path: String,
    },
    /// Adds an empty sequence.
    CreateSequence {
        /// Stable ID selected before execution for deterministic replay.
        id: SequenceId,
        /// User-visible sequence name.
        name: String,
    },
    /// Adds a track to an existing sequence.
    CreateTrack {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Stable track ID.
        id: TrackId,
        /// Accepted media kind.
        kind: TrackKind,
    },
    /// Places a source range on a track at normal speed.
    InsertClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Destination track.
        track_id: TrackId,
        /// Stable clip ID.
        id: ClipId,
        /// Referenced media asset.
        media_id: MediaAssetId,
        /// Start in sequence time.
        timeline_start: RationalTime,
        /// Start in source time.
        source_start: RationalTime,
        /// Positive source duration.
        source_duration: RationalTime,
    },
    /// Moves a clip on its current track.
    MoveClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to move.
        clip_id: ClipId,
        /// New start in sequence time.
        timeline_start: RationalTime,
    },
    /// Replaces a clip's sequence placement and source range.
    TrimClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to trim.
        clip_id: ClipId,
        /// New start in sequence time.
        timeline_start: RationalTime,
        /// New start in source time.
        source_start: RationalTime,
        /// New positive source duration.
        source_duration: RationalTime,
    },
    /// Splits one clip into adjacent left and right clips.
    SplitClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Existing clip retained for the left side.
        clip_id: ClipId,
        /// Stable ID assigned to the new right side.
        right_clip_id: ClipId,
        /// Exact split position in sequence time.
        timeline_position: RationalTime,
    },
    /// Removes a clip from a track.
    DeleteClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to remove.
        clip_id: ClipId,
    },
}

/// Event emitted only after a successful state transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// Media was registered.
    MediaImported {
        /// Imported asset.
        id: MediaAssetId,
    },
    /// A sequence was created.
    SequenceCreated {
        /// Created sequence.
        id: SequenceId,
    },
    /// A track was created.
    TrackCreated {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Created track.
        id: TrackId,
    },
    /// A clip was inserted.
    ClipInserted {
        /// Owning track.
        track_id: TrackId,
        /// Inserted clip.
        id: ClipId,
    },
    /// A clip was moved.
    ClipMoved {
        /// Moved clip.
        id: ClipId,
        /// New start time.
        timeline_start: RationalTime,
    },
    /// A clip range was trimmed.
    ClipTrimmed {
        /// Trimmed clip.
        id: ClipId,
    },
    /// One clip was divided into two adjacent clips.
    ClipSplit {
        /// Existing left clip.
        left_id: ClipId,
        /// New right clip.
        right_id: ClipId,
    },
    /// A clip was removed.
    ClipDeleted {
        /// Removed clip.
        id: ClipId,
    },
}

/// Structured command failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CommandError {
    /// User-provided text failed validation.
    #[error("{field} must not be empty")]
    EmptyField {
        /// Invalid field name.
        field: &'static str,
    },
    /// A transaction must contain at least one mutation.
    #[error("transaction must contain at least one command")]
    EmptyTransaction,
    /// Caller based its transaction on stale project state.
    #[error("revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict {
        /// Revision observed by the caller.
        expected: u64,
        /// Current project revision.
        actual: u64,
    },
    /// An ID was not found in current project state.
    #[error("sequence not found")]
    SequenceNotFound,
    /// A track ID was not found.
    #[error("track not found")]
    TrackNotFound,
    /// A clip ID was not found.
    #[error("clip not found")]
    ClipNotFound,
    /// A media asset ID was not found.
    #[error("media asset not found")]
    MediaNotFound,
    /// A locked track rejected a mutation.
    #[error("track is locked")]
    TrackLocked,
    /// A time or duration was outside its valid range.
    #[error("{field} must be {constraint}")]
    InvalidTime {
        /// Invalid field name.
        field: &'static str,
        /// Required range description.
        constraint: &'static str,
    },
    /// Two clips would occupy intersecting ranges on one track.
    #[error("clip overlaps another clip on the track")]
    ClipOverlap,
    /// A stable ID already exists.
    #[error("object identifier already exists")]
    DuplicateId,
    /// A project invariant prevented the commit.
    #[error("project could not commit: {0}")]
    Commit(String),
    /// Staged state failed authoritative domain validation.
    #[error("project invariant failed: {0}")]
    Invariant(String),
}

/// Result of an atomic command batch.
#[derive(Debug)]
pub struct TransactionOutcome {
    /// Events published in command order after commit.
    pub events: Vec<Event>,
    /// Snapshot-based undo record for this initial architecture.
    pub undo: UndoRecord,
}

/// Reversible project-state record.
#[derive(Debug)]
pub struct UndoRecord {
    label: String,
    previous: Project,
    expected_revision: u64,
}

impl UndoRecord {
    /// User-visible transaction label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Restores the state before this transaction and returns a redo record.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::RevisionConflict`] if later edits made this
    /// record stale.
    pub fn apply(self, project: &mut Project) -> Result<Self, CommandError> {
        if project.revision() != self.expected_revision {
            return Err(CommandError::RevisionConflict {
                expected: self.expected_revision,
                actual: project.revision(),
            });
        }
        let current = std::mem::replace(project, self.previous);
        Ok(Self {
            label: self.label,
            previous: current,
            expected_revision: project.revision(),
        })
    }
}

/// Executes commands atomically against project state.
#[derive(Debug, Default)]
pub struct CommandEngine;

impl CommandEngine {
    /// Validates and applies one command.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] when validation fails or the project cannot
    /// commit. Rejected validation does not mutate state or emit events.
    pub fn execute(project: &mut Project, command: Command) -> Result<Vec<Event>, CommandError> {
        Ok(Self::execute_transaction(project, "Edit", vec![command])?.events)
    }

    /// Validates and commits a batch as one revision.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if any command fails. The original project is
    /// unchanged and no events are returned when a batch fails.
    pub fn execute_transaction(
        project: &mut Project,
        label: impl Into<String>,
        commands: Vec<Command>,
    ) -> Result<TransactionOutcome, CommandError> {
        let revision = project.revision();
        Self::execute_transaction_at_revision(project, revision, label, commands)
    }

    /// Commits a batch only if the project still has the expected revision.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::RevisionConflict`] for stale callers, or another
    /// [`CommandError`] when validation fails. No failure mutates the project.
    pub fn execute_transaction_at_revision(
        project: &mut Project,
        expected_revision: u64,
        label: impl Into<String>,
        commands: Vec<Command>,
    ) -> Result<TransactionOutcome, CommandError> {
        if project.revision() != expected_revision {
            return Err(CommandError::RevisionConflict {
                expected: expected_revision,
                actual: project.revision(),
            });
        }
        let label = label.into();
        if label.trim().is_empty() {
            return Err(CommandError::EmptyField { field: "label" });
        }
        if commands.is_empty() {
            return Err(CommandError::EmptyTransaction);
        }
        let mut staged = project.clone();
        staged
            .commit_revision()
            .map_err(|error| CommandError::Commit(error.to_string()))?;
        let mut events = Vec::with_capacity(commands.len());
        for command in commands {
            events.push(Self::apply(&mut staged, command)?);
        }
        staged
            .validate()
            .map_err(|error| CommandError::Invariant(error.to_string()))?;
        let previous = std::mem::replace(project, staged);
        let expected_revision = project.revision();
        Ok(TransactionOutcome {
            events,
            undo: UndoRecord {
                label,
                previous,
                expected_revision,
            },
        })
    }

    /// Validates a batch and returns its planned events without committing.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if any command would fail. The supplied project
    /// is never mutated.
    pub fn validate_transaction(
        project: &Project,
        commands: Vec<Command>,
    ) -> Result<Vec<Event>, CommandError> {
        if commands.is_empty() {
            return Err(CommandError::EmptyTransaction);
        }
        let mut staged = project.clone();
        let events = commands
            .into_iter()
            .map(|command| Self::apply(&mut staged, command))
            .collect::<Result<Vec<_>, _>>()?;
        staged
            .validate()
            .map_err(|error| CommandError::Invariant(error.to_string()))?;
        Ok(events)
    }

    fn apply(project: &mut Project, command: Command) -> Result<Event, CommandError> {
        match command {
            Command::ImportMedia { id, source_path } => import_media(project, id, source_path),
            Command::CreateSequence { id, name } => create_sequence(project, id, name),
            Command::CreateTrack {
                sequence_id,
                id,
                kind,
            } => create_track(project, sequence_id, id, kind),
            Command::InsertClip {
                sequence_id,
                track_id,
                id,
                media_id,
                timeline_start,
                source_start,
                source_duration,
            } => insert_clip(
                project,
                ClipLocation {
                    sequence_id,
                    track_id,
                    id,
                    media_id,
                    timeline_start,
                    source_start,
                    source_duration,
                },
            ),
            Command::MoveClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
            } => move_clip(project, sequence_id, track_id, clip_id, timeline_start),
            Command::TrimClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
                source_start,
                source_duration,
            } => trim_clip(
                project,
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
                source_start,
                source_duration,
            ),
            Command::SplitClip {
                sequence_id,
                track_id,
                clip_id,
                right_clip_id,
                timeline_position,
            } => split_clip(
                project,
                sequence_id,
                track_id,
                clip_id,
                right_clip_id,
                timeline_position,
            ),
            Command::DeleteClip {
                sequence_id,
                track_id,
                clip_id,
            } => delete_clip(project, sequence_id, track_id, clip_id),
        }
    }
}

fn import_media(
    project: &mut Project,
    id: MediaAssetId,
    source_path: String,
) -> Result<Event, CommandError> {
    if source_path.trim().is_empty() {
        return Err(CommandError::EmptyField {
            field: "source_path",
        });
    }
    if project.media.contains_key(&id) {
        return Err(CommandError::DuplicateId);
    }
    project.media.insert(id, MediaAsset { id, source_path });
    Ok(Event::MediaImported { id })
}

fn create_sequence(
    project: &mut Project,
    id: SequenceId,
    name: String,
) -> Result<Event, CommandError> {
    if name.trim().is_empty() {
        return Err(CommandError::EmptyField { field: "name" });
    }
    if project.sequences.contains_key(&id) {
        return Err(CommandError::DuplicateId);
    }
    project.sequences.insert(
        id,
        Sequence {
            id,
            name,
            tracks: Vec::new(),
        },
    );
    Ok(Event::SequenceCreated { id })
}

fn create_track(
    project: &mut Project,
    sequence_id: SequenceId,
    id: TrackId,
    kind: TrackKind,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    if sequence.tracks.iter().any(|track| track.id == id) {
        return Err(CommandError::DuplicateId);
    }
    sequence.tracks.push(Track {
        id,
        kind,
        clips: Vec::new(),
        locked: false,
    });
    Ok(Event::TrackCreated { sequence_id, id })
}

#[derive(Clone, Copy)]
struct ClipLocation {
    sequence_id: SequenceId,
    track_id: TrackId,
    id: ClipId,
    media_id: MediaAssetId,
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
}

fn insert_clip(project: &mut Project, location: ClipLocation) -> Result<Event, CommandError> {
    validate_clip_time(
        location.timeline_start,
        location.source_start,
        location.source_duration,
    )?;
    if !project.media.contains_key(&location.media_id) {
        return Err(CommandError::MediaNotFound);
    }
    let track = find_track_mut(project, location.sequence_id, location.track_id)?;
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    if track.clips.iter().any(|clip| clip.id == location.id) {
        return Err(CommandError::DuplicateId);
    }
    ensure_range_available(
        track,
        location.timeline_start,
        location.source_duration,
        None,
    )?;
    track.clips.push(Clip {
        id: location.id,
        media_id: location.media_id,
        timeline_start: location.timeline_start,
        source_start: location.source_start,
        source_duration: location.source_duration,
    });
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipInserted {
        track_id: location.track_id,
        id: location.id,
    })
}

fn move_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    timeline_start: RationalTime,
) -> Result<Event, CommandError> {
    if !timeline_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "timeline_start",
            constraint: "non-negative",
        });
    }
    let track = find_track_mut(project, sequence_id, track_id)?;
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    let duration = track
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?
        .source_duration;
    ensure_range_available(track, timeline_start, duration, Some(clip_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    clip.timeline_start = timeline_start;
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipMoved {
        id: clip_id,
        timeline_start,
    })
}

#[allow(clippy::too_many_arguments)]
fn trim_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
) -> Result<Event, CommandError> {
    validate_clip_time(timeline_start, source_start, source_duration)?;
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    if !track.clips.iter().any(|clip| clip.id == clip_id) {
        return Err(CommandError::ClipNotFound);
    }
    ensure_range_available(track, timeline_start, source_duration, Some(clip_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    clip.timeline_start = timeline_start;
    clip.source_start = source_start;
    clip.source_duration = source_duration;
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipTrimmed { id: clip_id })
}

fn split_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    right_clip_id: ClipId,
    timeline_position: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    if track.clips.iter().any(|clip| clip.id == right_clip_id) {
        return Err(CommandError::DuplicateId);
    }
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let original = track.clips[index].clone();
    let original_end = original
        .timeline_start
        .checked_add(original.source_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    if timeline_position <= original.timeline_start || timeline_position >= original_end {
        return Err(CommandError::InvalidTime {
            field: "timeline_position",
            constraint: "strictly inside the clip",
        });
    }
    let left_duration = timeline_position
        .checked_sub(original.timeline_start)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    let right_source_start = original
        .source_start
        .checked_add(left_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    let right_duration = original
        .source_duration
        .checked_sub(left_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    track.clips[index].source_duration = left_duration;
    track.clips.insert(
        index + 1,
        Clip {
            id: right_clip_id,
            media_id: original.media_id,
            timeline_start: timeline_position,
            source_start: right_source_start,
            source_duration: right_duration,
        },
    );
    Ok(Event::ClipSplit {
        left_id: clip_id,
        right_id: right_clip_id,
    })
}

fn delete_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    track.clips.remove(index);
    Ok(Event::ClipDeleted { id: clip_id })
}

fn ensure_track_editable(track: &Track) -> Result<(), CommandError> {
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    Ok(())
}

fn find_track_mut(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
) -> Result<&mut Track, CommandError> {
    project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?
        .tracks
        .iter_mut()
        .find(|track| track.id == track_id)
        .ok_or(CommandError::TrackNotFound)
}

fn validate_clip_time(
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
) -> Result<(), CommandError> {
    for (field, value) in [
        ("timeline_start", timeline_start),
        ("source_start", source_start),
    ] {
        if !value.is_non_negative() {
            return Err(CommandError::InvalidTime {
                field,
                constraint: "non-negative",
            });
        }
    }
    if !source_duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "source_duration",
            constraint: "positive",
        });
    }
    Ok(())
}

fn ensure_range_available(
    track: &Track,
    start: RationalTime,
    duration: RationalTime,
    ignored_clip: Option<ClipId>,
) -> Result<(), CommandError> {
    let end = start
        .checked_add(duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    for clip in &track.clips {
        if ignored_clip == Some(clip.id) {
            continue;
        }
        let clip_end = clip
            .timeline_start
            .checked_add(clip.source_duration)
            .map_err(|error| CommandError::Commit(error.to_string()))?;
        if start < clip_end && clip.timeline_start < end {
            return Err(CommandError::ClipOverlap);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Command, CommandEngine, CommandError};
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Project, TrackKind};
    use prism_timeline::{RationalTime, TimeError};

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
                id: TrackId::new(),
                kind: TrackKind::Video,
            },
        )?;
        assert_eq!(project.revision(), 2);
        assert_eq!(project.sequences[&sequence_id].tracks.len(), 1);
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
    fn split_trim_and_delete_preserve_exact_source_ranges() -> Result<(), Box<dyn std::error::Error>>
    {
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
        assert_eq!(clips[1].source_start, two);
        assert_eq!(clips[1].source_duration, two);

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
}
