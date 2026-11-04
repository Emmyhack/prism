//! Authoritative in-memory project model, independent of persistence formats.

use prism_core::{ClipId, MediaAssetId, ProjectId, SequenceId, TrackId};
use prism_timeline::RationalTime;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Current authoritative project schema version.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// A Prism project aggregate.
#[derive(Clone, Debug)]
pub struct Project {
    /// Stable identifier.
    pub id: ProjectId,
    /// User-visible name.
    pub name: String,
    /// Domain schema version, not a file-format version.
    pub schema_version: u32,
    /// Referenced media assets.
    pub media: BTreeMap<MediaAssetId, MediaAsset>,
    /// Editable sequences.
    pub sequences: BTreeMap<SequenceId, Sequence>,
    revision: u64,
}

impl Project {
    /// Creates an empty project.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: ProjectId::new(),
            name: name.into(),
            schema_version: CURRENT_SCHEMA_VERSION,
            media: BTreeMap::new(),
            sequences: BTreeMap::new(),
            revision: 0,
        }
    }

    /// Restores a project after a persistence adapter has validated its data.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::UnsupportedSchemaVersion`] when the adapter did
    /// not migrate the state to the current domain schema.
    pub fn restore(state: RestoredProject) -> Result<Self, ProjectError> {
        if state.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ProjectError::UnsupportedSchemaVersion {
                found: state.schema_version,
                current: CURRENT_SCHEMA_VERSION,
            });
        }
        let project = Self {
            id: state.id,
            name: state.name,
            schema_version: state.schema_version,
            media: state.media,
            sequences: state.sequences,
            revision: state.revision,
        };
        project.validate()?;
        Ok(project)
    }

    /// Monotonic state revision, incremented after successful commands.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Marks one successfully committed mutation.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::RevisionOverflow`] if no later revision exists.
    pub fn commit_revision(&mut self) -> Result<(), ProjectError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ProjectError::RevisionOverflow)?;
        Ok(())
    }

    /// Validates invariants required by commands, rendering, and persistence.
    ///
    /// # Errors
    ///
    /// Returns a structured [`ProjectValidationError`] wrapped in
    /// [`ProjectError::InvalidState`] when any authoritative invariant fails.
    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.name.trim().is_empty() {
            return Err(ProjectValidationError::EmptyName { object: "project" }.into());
        }
        let mut track_ids = BTreeSet::new();
        let mut clip_ids = BTreeSet::new();
        for (asset_key, asset) in &self.media {
            if *asset_key != asset.id {
                return Err(ProjectValidationError::KeyMismatch { object: "media" }.into());
            }
            if asset.source_path.trim().is_empty() {
                return Err(ProjectValidationError::EmptySourcePath {
                    media_id: asset.id.as_uuid().to_string(),
                }
                .into());
            }
        }
        for (sequence_key, sequence) in &self.sequences {
            validate_sequence(
                *sequence_key,
                sequence,
                &self.media,
                &mut track_ids,
                &mut clip_ids,
            )?;
        }
        Ok(())
    }
}

fn validate_sequence(
    key: SequenceId,
    sequence: &Sequence,
    media: &BTreeMap<MediaAssetId, MediaAsset>,
    track_ids: &mut BTreeSet<TrackId>,
    clip_ids: &mut BTreeSet<ClipId>,
) -> Result<(), ProjectError> {
    if key != sequence.id {
        return Err(ProjectValidationError::KeyMismatch { object: "sequence" }.into());
    }
    if sequence.name.trim().is_empty() {
        return Err(ProjectValidationError::EmptyName { object: "sequence" }.into());
    }
    for track in &sequence.tracks {
        if !track_ids.insert(track.id) {
            return Err(ProjectValidationError::DuplicateId { object: "track" }.into());
        }
        validate_track(track, media, clip_ids)?;
    }
    Ok(())
}

fn validate_track(
    track: &Track,
    media: &BTreeMap<MediaAssetId, MediaAsset>,
    clip_ids: &mut BTreeSet<ClipId>,
) -> Result<(), ProjectError> {
    let mut previous_end = None;
    for clip in &track.clips {
        if !clip_ids.insert(clip.id) {
            return Err(ProjectValidationError::DuplicateId { object: "clip" }.into());
        }
        if !media.contains_key(&clip.media_id) {
            return Err(ProjectValidationError::MissingMedia {
                clip_id: clip.id.as_uuid().to_string(),
            }
            .into());
        }
        if !clip.timeline_start.is_non_negative() || !clip.source_start.is_non_negative() {
            return Err(ProjectValidationError::NegativeClipTime {
                clip_id: clip.id.as_uuid().to_string(),
            }
            .into());
        }
        if !clip.source_duration.is_positive() {
            return Err(ProjectValidationError::NonPositiveDuration {
                clip_id: clip.id.as_uuid().to_string(),
            }
            .into());
        }
        if previous_end.is_some_and(|end| clip.timeline_start < end) {
            return Err(ProjectValidationError::ClipOrderOrOverlap {
                track_id: track.id.as_uuid().to_string(),
            }
            .into());
        }
        previous_end = Some(clip.timeline_start.checked_add(clip.source_duration)?);
    }
    Ok(())
}

/// Validated domain state supplied by a persistence adapter.
#[derive(Debug)]
pub struct RestoredProject {
    /// Stable project identifier.
    pub id: ProjectId,
    /// User-visible name.
    pub name: String,
    /// Domain schema version after migration.
    pub schema_version: u32,
    /// Last committed project revision.
    pub revision: u64,
    /// Referenced media assets.
    pub media: BTreeMap<MediaAssetId, MediaAsset>,
    /// Editable sequences.
    pub sequences: BTreeMap<SequenceId, Sequence>,
}

/// A nondestructive reference to source media.
#[derive(Clone, Debug)]
pub struct MediaAsset {
    /// Stable identifier.
    pub id: MediaAssetId,
    /// Original path as selected or relinked by the user.
    pub source_path: String,
}

/// A timeline sequence.
#[derive(Clone, Debug)]
pub struct Sequence {
    /// Stable identifier.
    pub id: SequenceId,
    /// User-visible name.
    pub name: String,
    /// Ordered tracks.
    pub tracks: Vec<Track>,
}

/// Track media kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackKind {
    /// Visual media.
    Video,
    /// Sampled audio.
    Audio,
}

/// A sequence track.
#[derive(Clone, Debug)]
pub struct Track {
    /// Stable identifier.
    pub id: TrackId,
    /// Media kind accepted by this track.
    pub kind: TrackKind,
    /// Ordered clips. Overlap policy is command-defined.
    pub clips: Vec<Clip>,
    /// Locked tracks reject editing mutations.
    pub locked: bool,
}

/// A nondestructive source range placed on a sequence.
#[derive(Clone, Debug)]
pub struct Clip {
    /// Stable identifier.
    pub id: ClipId,
    /// Referenced source asset.
    pub media_id: MediaAssetId,
    /// Start in sequence time.
    pub timeline_start: RationalTime,
    /// Start in source media time.
    pub source_start: RationalTime,
    /// Duration in source time before retiming.
    pub source_duration: RationalTime,
}

/// Project invariant failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProjectError {
    /// Revision counter exhausted.
    #[error("project revision counter overflow")]
    RevisionOverflow,
    /// Persistence state was not migrated to the current schema.
    #[error("unsupported project schema {found}; current schema is {current}")]
    UnsupportedSchemaVersion {
        /// Schema supplied by the adapter.
        found: u32,
        /// Schema required by this build.
        current: u32,
    },
    /// Restored or staged state violated a domain invariant.
    #[error("invalid project state: {0}")]
    InvalidState(#[from] ProjectValidationError),
    /// Exact time arithmetic failed during validation.
    #[error("invalid timeline arithmetic: {0}")]
    Time(#[from] prism_timeline::TimeError),
}

/// Structured authoritative project invariant failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProjectValidationError {
    /// A user-visible object name is blank.
    #[error("{object} name must not be empty")]
    EmptyName {
        /// Invalid object kind.
        object: &'static str,
    },
    /// A map key does not equal the contained object's typed ID.
    #[error("{object} map key does not match its object ID")]
    KeyMismatch {
        /// Invalid object kind.
        object: &'static str,
    },
    /// A source media path is blank.
    #[error("media {media_id} has an empty source path")]
    EmptySourcePath {
        /// Invalid media identifier.
        media_id: String,
    },
    /// An ID required to be globally unique was repeated.
    #[error("duplicate {object} identifier")]
    DuplicateId {
        /// Duplicated object kind.
        object: &'static str,
    },
    /// A clip references a media asset absent from the project.
    #[error("clip {clip_id} references missing media")]
    MissingMedia {
        /// Invalid clip identifier.
        clip_id: String,
    },
    /// A clip start was negative.
    #[error("clip {clip_id} has a negative timeline or source start")]
    NegativeClipTime {
        /// Invalid clip identifier.
        clip_id: String,
    },
    /// A clip duration was zero or negative.
    #[error("clip {clip_id} has a non-positive duration")]
    NonPositiveDuration {
        /// Invalid clip identifier.
        clip_id: String,
    },
    /// Track clips were not chronological or occupied intersecting ranges.
    #[error("track {track_id} clips are out of order or overlap")]
    ClipOrderOrOverlap {
        /// Invalid track identifier.
        track_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        Clip, MediaAsset, Project, ProjectError, ProjectValidationError, Sequence, Track, TrackKind,
    };
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_timeline::{RationalTime, TimeError};

    #[test]
    fn validator_rejects_missing_media_reference() -> Result<(), TimeError> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                tracks: vec![Track {
                    id: TrackId::new(),
                    kind: TrackKind::Video,
                    clips: vec![Clip {
                        id: ClipId::new(),
                        media_id: MediaAssetId::new(),
                        timeline_start: RationalTime::ZERO,
                        source_start: RationalTime::ZERO,
                        source_duration: RationalTime::new(1, 1)?,
                    }],
                    locked: false,
                }],
            },
        );
        assert!(matches!(
            project.validate(),
            Err(ProjectError::InvalidState(
                ProjectValidationError::MissingMedia { .. }
            ))
        ));
        Ok(())
    }

    #[test]
    fn validator_rejects_overlapping_clips() -> Result<(), TimeError> {
        let mut project = Project::new("Test");
        let media_id = MediaAssetId::new();
        project.media.insert(
            media_id,
            MediaAsset {
                id: media_id,
                source_path: "/media/source.mov".to_owned(),
            },
        );
        let sequence_id = SequenceId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                tracks: vec![Track {
                    id: TrackId::new(),
                    kind: TrackKind::Video,
                    clips: vec![
                        Clip {
                            id: ClipId::new(),
                            media_id,
                            timeline_start: RationalTime::ZERO,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(2, 1)?,
                        },
                        Clip {
                            id: ClipId::new(),
                            media_id,
                            timeline_start: RationalTime::new(1, 1)?,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(2, 1)?,
                        },
                    ],
                    locked: false,
                }],
            },
        );
        assert!(matches!(
            project.validate(),
            Err(ProjectError::InvalidState(
                ProjectValidationError::ClipOrderOrOverlap { .. }
            ))
        ));
        Ok(())
    }
}
