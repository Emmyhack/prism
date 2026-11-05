//! Authoritative in-memory project model, independent of persistence formats.

use prism_core::{ClipId, MarkerId, MediaAssetId, ProjectId, SequenceId, TrackId, TransitionId};
use prism_timeline::{RationalTime, TimeError};
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
        let mut marker_ids = BTreeSet::new();
        let mut transition_ids = BTreeSet::new();
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
                &mut marker_ids,
                &mut transition_ids,
            )?;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_lines)]
fn validate_sequence(
    key: SequenceId,
    sequence: &Sequence,
    media: &BTreeMap<MediaAssetId, MediaAsset>,
    track_ids: &mut BTreeSet<TrackId>,
    clip_ids: &mut BTreeSet<ClipId>,
    marker_ids: &mut BTreeSet<MarkerId>,
    transition_ids: &mut BTreeSet<TransitionId>,
) -> Result<(), ProjectError> {
    if key != sequence.id {
        return Err(ProjectValidationError::KeyMismatch { object: "sequence" }.into());
    }
    if sequence.name.trim().is_empty() {
        return Err(ProjectValidationError::EmptyName { object: "sequence" }.into());
    }
    let mut previous_marker_time = None;
    for marker in &sequence.markers {
        if !marker_ids.insert(marker.id) {
            return Err(ProjectValidationError::DuplicateId { object: "marker" }.into());
        }
        if marker.label.trim().is_empty() {
            return Err(ProjectValidationError::EmptyName { object: "marker" }.into());
        }
        if !marker.time.is_non_negative() {
            return Err(ProjectValidationError::NegativeMarkerTime {
                marker_id: marker.id.as_uuid().to_string(),
            }
            .into());
        }
        if previous_marker_time.is_some_and(|previous| marker.time < previous) {
            return Err(ProjectValidationError::MarkerOrder.into());
        }
        previous_marker_time = Some(marker.time);
    }
    for track in &sequence.tracks {
        if !track_ids.insert(track.id) {
            return Err(ProjectValidationError::DuplicateId { object: "track" }.into());
        }
        validate_track(track, media, clip_ids)?;
    }
    let clip_ids_in_sequence = sequence
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(|clip| clip.id))
        .collect::<BTreeSet<_>>();
    for transition in &sequence.transitions {
        if !transition_ids.insert(transition.id) {
            return Err(ProjectValidationError::DuplicateId {
                object: "transition",
            }
            .into());
        }
        if !clip_ids_in_sequence.contains(&transition.outgoing_clip)
            || !clip_ids_in_sequence.contains(&transition.incoming_clip)
        {
            return Err(ProjectValidationError::TransitionClipMissing {
                transition_id: transition.id.as_uuid().to_string(),
            }
            .into());
        }
        let outgoing_track = sequence.tracks.iter().find(|track| {
            track
                .clips
                .iter()
                .any(|clip| clip.id == transition.outgoing_clip)
        });
        let incoming_track = sequence.tracks.iter().find(|track| {
            track
                .clips
                .iter()
                .any(|clip| clip.id == transition.incoming_clip)
        });
        let (Some(outgoing_track), Some(incoming_track)) = (outgoing_track, incoming_track) else {
            return Err(ProjectValidationError::TransitionClipMissing {
                transition_id: transition.id.as_uuid().to_string(),
            }
            .into());
        };
        let outgoing = outgoing_track
            .clips
            .iter()
            .find(|clip| clip.id == transition.outgoing_clip)
            .ok_or(ProjectValidationError::TransitionClipMissing {
                transition_id: transition.id.as_uuid().to_string(),
            })?;
        let incoming = incoming_track
            .clips
            .iter()
            .find(|clip| clip.id == transition.incoming_clip)
            .ok_or(ProjectValidationError::TransitionClipMissing {
                transition_id: transition.id.as_uuid().to_string(),
            })?;
        let adjacent = outgoing_track.id == incoming_track.id
            && outgoing_track.kind == TrackKind::Video
            && outgoing
                .timeline_start
                .checked_add(outgoing.source_duration)?
                == incoming.timeline_start
            && transition.duration <= outgoing.source_duration
            && transition.duration <= incoming.source_duration;
        if !adjacent {
            return Err(ProjectValidationError::InvalidTransition {
                transition_id: transition.id.as_uuid().to_string(),
            }
            .into());
        }
        if !transition.duration.is_positive() {
            return Err(ProjectValidationError::NonPositiveTransitionDuration {
                transition_id: transition.id.as_uuid().to_string(),
            }
            .into());
        }
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
    /// Ordered timeline markers.
    pub markers: Vec<Marker>,
    /// Nondestructive transitions between adjacent clips.
    pub transitions: Vec<Transition>,
    /// Ordered tracks.
    pub tracks: Vec<Track>,
}

impl Sequence {
    /// Returns the exact end time of the latest clip across all tracks.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::Overflow`] if a clip end cannot be represented.
    pub fn duration(&self) -> Result<RationalTime, TimeError> {
        let mut end = RationalTime::ZERO;
        for track in &self.tracks {
            for clip in &track.clips {
                let clip_end = clip.timeline_start.checked_add(clip.source_duration)?;
                if clip_end > end {
                    end = clip_end;
                }
            }
        }
        Ok(end)
    }

    /// Returns the exact end time for one track, or `None` if the track is
    /// absent.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::Overflow`] if a clip end cannot be represented.
    pub fn track_duration(&self, track_id: TrackId) -> Result<Option<RationalTime>, TimeError> {
        let Some(track) = self.tracks.iter().find(|track| track.id == track_id) else {
            return Ok(None);
        };
        let mut end = RationalTime::ZERO;
        for clip in &track.clips {
            let clip_end = clip.timeline_start.checked_add(clip.source_duration)?;
            if clip_end > end {
                end = clip_end;
            }
        }
        Ok(Some(end))
    }
}

/// A named exact-time marker on a sequence timeline.
#[derive(Clone, Debug)]
pub struct Marker {
    /// Stable marker identifier.
    pub id: MarkerId,
    /// Exact sequence position.
    pub time: RationalTime,
    /// User-visible marker label.
    pub label: String,
}

/// Supported timeline transition type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    /// Linear cross-dissolve.
    Dissolve,
}

/// Nondestructive transition between two clips.
#[derive(Clone, Debug)]
pub struct Transition {
    /// Stable transition identifier.
    pub id: TransitionId,
    /// Outgoing clip.
    pub outgoing_clip: ClipId,
    /// Incoming clip.
    pub incoming_clip: ClipId,
    /// Exact overlap duration.
    pub duration: RationalTime,
    /// Transition operation.
    pub kind: TransitionKind,
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
    /// Visible tracks contribute video layers to preview and export.
    pub visible: bool,
    /// Muted tracks contribute no audio during preview and export.
    pub muted: bool,
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
    /// A marker time was negative.
    #[error("marker {marker_id} has a negative time")]
    NegativeMarkerTime {
        /// Marker identifier.
        marker_id: String,
    },
    /// Markers were not stored in nondecreasing time order.
    #[error("sequence markers are not ordered by time")]
    MarkerOrder,
    /// A transition references a clip absent from the sequence.
    #[error("transition {transition_id} references a missing clip")]
    TransitionClipMissing {
        /// Transition identifier.
        transition_id: String,
    },
    /// A transition duration was zero or negative.
    #[error("transition {transition_id} has a non-positive duration")]
    NonPositiveTransitionDuration {
        /// Transition identifier.
        transition_id: String,
    },
    /// Transition clips are not adjacent video clips or duration is too long.
    #[error("transition {transition_id} is invalid for its clips")]
    InvalidTransition {
        /// Transition identifier.
        transition_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        Clip, Marker, MediaAsset, Project, ProjectError, ProjectValidationError, Sequence, Track,
        TrackKind,
    };
    use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId};
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
                markers: Vec::new(),
                transitions: Vec::new(),
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
                    visible: true,
                    muted: false,
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
    fn sequence_duration_uses_latest_exact_clip_end() -> Result<(), TimeError> {
        let track_id = TrackId::new();
        let sequence = Sequence {
            id: SequenceId::new(),
            name: "Main".to_owned(),
            markers: Vec::new(),
            transitions: Vec::new(),
            tracks: vec![Track {
                id: track_id,
                kind: TrackKind::Video,
                clips: vec![Clip {
                    id: ClipId::new(),
                    media_id: MediaAssetId::new(),
                    timeline_start: RationalTime::new(5, 1)?,
                    source_start: RationalTime::ZERO,
                    source_duration: RationalTime::new(2, 1)?,
                }],
                locked: false,
                visible: true,
                muted: false,
            }],
        };
        assert_eq!(sequence.duration()?, RationalTime::new(7, 1)?);
        assert_eq!(
            sequence.track_duration(track_id)?,
            Some(RationalTime::new(7, 1)?)
        );
        assert_eq!(sequence.track_duration(TrackId::new())?, None);
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
                markers: Vec::new(),
                transitions: Vec::new(),
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
                    visible: true,
                    muted: false,
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

    #[test]
    fn validator_rejects_invalid_marker_order_and_labels() -> Result<(), TimeError> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let first = MarkerId::new();
        let second = MarkerId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: vec![
                    Marker {
                        id: first,
                        time: RationalTime::new(3, 1)?,
                        label: "Later".to_owned(),
                    },
                    Marker {
                        id: second,
                        time: RationalTime::new(1, 1)?,
                        label: "Earlier".to_owned(),
                    },
                ],
                transitions: Vec::new(),
                tracks: Vec::new(),
            },
        );
        assert!(matches!(
            project.validate(),
            Err(ProjectError::InvalidState(
                ProjectValidationError::MarkerOrder
            ))
        ));
        if let Some(sequence) = project.sequences.get_mut(&sequence_id) {
            sequence.markers[1].time = RationalTime::new(4, 1)?;
            sequence.markers[1].label = " ".to_owned();
        }
        assert!(matches!(
            project.validate(),
            Err(ProjectError::InvalidState(
                ProjectValidationError::EmptyName { object: "marker" }
            ))
        ));
        Ok(())
    }
}
