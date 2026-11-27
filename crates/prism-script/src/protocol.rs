use super::{
    API_VERSION, ClipId, Command, Deserialize, Error, MarkerId, MediaAssetId, ProtocolTransaction,
    ProxySourceFingerprint, RationalTime, RecipeError, RecipeManifest, RecipePermission, Request,
    SequenceId, Serialize, TrackId, TrackKind, TransactionRequest, Uuid,
};

/// Recipe invocation received by the script worker boundary.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeRequest {
    /// Declarative recipe metadata.
    pub manifest: RecipeManifest,
    /// User-supplied parameter values.
    pub inputs: serde_json::Map<String, serde_json::Value>,
}

/// Validated recipe execution plan. Construction performs no script execution.
#[derive(Debug)]
pub struct RecipeExecutionPlan {
    /// Validated recipe metadata.
    pub manifest: RecipeManifest,
    /// Validated user inputs, preserving JSON values for the worker adapter.
    pub inputs: serde_json::Map<String, serde_json::Value>,
}

/// Initial protocol operations. Every mutation converts to a typed command.
#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Operation {
    /// Register a source-media reference.
    ImportMedia {
        /// Stable asset identifier.
        id: Uuid,
        /// User-authorized source path.
        source_path: String,
    },
    /// Sets or clears a media proxy relink.
    SetMediaProxy {
        /// Asset to update.
        media_id: Uuid,
        /// New proxy path, or `None` to clear it.
        proxy_path: Option<String>,
        /// Lowercase source SHA-256 recorded for the proxy assignment.
        source_sha256: Option<String>,
        /// Exact source byte length recorded with the digest.
        source_bytes: Option<u64>,
    },
    /// Create an empty sequence.
    CreateSequence {
        /// Stable identifier encoded as a UUID.
        id: Uuid,
        /// User-visible name.
        name: String,
    },
    /// Add a named exact-time sequence marker.
    AddMarker {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Stable marker identifier.
        id: Uuid,
        /// Exact sequence position.
        time: RationalTimeDto,
        /// User-visible label.
        label: String,
    },
    /// Delete a sequence marker.
    DeleteMarker {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Marker identifier.
        marker_id: Uuid,
    },
    /// Add a transition between adjacent clips.
    AddTransition {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Stable transition ID.
        id: Uuid,
        /// Outgoing clip.
        outgoing_clip: Uuid,
        /// Incoming clip.
        incoming_clip: Uuid,
        /// Exact duration.
        duration: RationalTimeDto,
        /// Placement relative to the edit boundary. Older requests default to centered.
        #[serde(default)]
        alignment: TransitionAlignmentDto,
        /// Transition type.
        kind: TransitionKindDto,
    },
    /// Delete a transition.
    DeleteTransition {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Transition ID.
        transition_id: Uuid,
    },
    /// Create an empty track.
    CreateTrack {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Stable track identifier.
        id: Uuid,
        /// Track media kind.
        kind: TrackKindDto,
    },
    /// Locks or unlocks a track for future editing commands.
    SetTrackLocked {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to update.
        track_id: Uuid,
        /// New lock state.
        locked: bool,
    },
    /// Changes whether a track contributes video layers.
    SetTrackVisible {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to update.
        track_id: Uuid,
        /// New visibility state.
        visible: bool,
    },
    /// Changes whether a track contributes audio.
    SetTrackMuted {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to update.
        track_id: Uuid,
        /// New mute state.
        muted: bool,
    },
    /// Reorders a track within its sequence.
    ReorderTrack {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to move.
        track_id: Uuid,
        /// Zero-based destination index.
        destination_index: usize,
    },
    /// Insert a source range on a track.
    InsertClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Destination track.
        track_id: Uuid,
        /// Stable clip identifier.
        id: Uuid,
        /// Referenced media asset.
        media_id: Uuid,
        /// Sequence start.
        timeline_start: RationalTimeDto,
        /// Source start.
        source_start: RationalTimeDto,
        /// Positive source duration.
        source_duration: RationalTimeDto,
    },
    /// Duplicate an existing clip at a new sequence position.
    DuplicateClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Existing clip identifier.
        clip_id: Uuid,
        /// Stable duplicate identifier.
        new_clip_id: Uuid,
        /// New sequence start.
        timeline_start: RationalTimeDto,
    },
    /// Move an existing clip.
    MoveClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
        /// New sequence start.
        timeline_start: RationalTimeDto,
    },
    /// Slip an existing clip's source window without changing its timeline span.
    SlipClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
        /// Exact source-time delta.
        source_delta: RationalTimeDto,
    },
    /// Roll the boundary between two adjacent clips.
    RollEdit {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip before the boundary.
        left_clip_id: Uuid,
        /// Clip after the boundary.
        right_clip_id: Uuid,
        /// Exact boundary delta.
        delta: RationalTimeDto,
    },
    /// Slide a clip while adjusting both adjacent clips.
    SlideEdit {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip before the selected clip.
        previous_clip_id: Uuid,
        /// Selected clip.
        clip_id: Uuid,
        /// Clip after the selected clip.
        next_clip_id: Uuid,
        /// Exact timeline delta.
        delta: RationalTimeDto,
    },
    /// Trim an existing clip.
    TrimClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
        /// New sequence start.
        timeline_start: RationalTimeDto,
        /// New source start.
        source_start: RationalTimeDto,
        /// New source duration.
        source_duration: RationalTimeDto,
    },
    /// Split an existing clip.
    SplitClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Existing left clip identifier.
        clip_id: Uuid,
        /// New right clip identifier.
        right_clip_id: Uuid,
        /// Sequence split position.
        timeline_position: RationalTimeDto,
    },
    /// Delete an existing clip.
    DeleteClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
    },
    /// Remove a clip and close the gap on its track.
    RippleDelete {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
    },
}

/// Exact serialized time; floating-point seconds are intentionally unsupported.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RationalTimeDto {
    /// Signed tick count.
    pub value: i64,
    /// Positive ticks per second.
    pub timescale: u32,
}

/// Serialized track kind.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKindDto {
    /// Visual media.
    Video,
    /// Sampled audio.
    Audio,
}

/// Serialized transition kind.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransitionKindDto {
    /// Linear cross-dissolve.
    Dissolve,
}

/// Serialized transition placement relative to the edit boundary.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransitionAlignmentDto {
    /// Transition begins at the cut.
    StartAtCut,
    /// Transition is split exactly around the cut.
    #[default]
    Centered,
    /// Transition ends at the cut.
    EndAtCut,
}

/// Protocol validation errors returned consistently to every SDK.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    /// Caller requested an unsupported API contract.
    #[error("unsupported API version: {requested}")]
    UnsupportedApiVersion {
        /// Version supplied by the caller.
        requested: String,
    },
    /// Serialized rational time used a zero timescale.
    #[error("invalid rational time: timescale must be greater than zero")]
    InvalidTimebase,
    /// Proxy fingerprint fields were only partially supplied.
    #[error("invalid proxy fingerprint: SHA-256 and byte length must be supplied together")]
    InvalidProxyFingerprint,
    /// Recipe metadata or inputs failed validation.
    #[error("invalid recipe: {0}")]
    InvalidRecipe(RecipeError),
}

impl RecipeRequest {
    /// Validates a recipe manifest and its inputs before worker startup.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidRecipe`] when metadata or parameters
    /// fail the recipe safety contract.
    pub fn into_execution_plan(self) -> Result<RecipeExecutionPlan, ProtocolError> {
        self.manifest
            .validate_inputs(&self.inputs)
            .map_err(ProtocolError::InvalidRecipe)?;
        let mut inputs = self.inputs;
        for (name, schema) in &self.manifest.parameters {
            if !inputs.contains_key(name)
                && let Some(default) = &schema.default
            {
                inputs.insert(name.clone(), default.clone());
            }
        }
        Ok(RecipeExecutionPlan {
            manifest: self.manifest,
            inputs,
        })
    }

    /// Authorizes requested permissions and validates inputs before planning.
    ///
    /// This is the worker-host entry point when a caller has an explicit grant
    /// set. [`Self::into_execution_plan`] remains useful for metadata-only
    /// validation tools that do not activate a recipe.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidRecipe`] when permissions, metadata, or
    /// inputs fail validation.
    pub fn authorize_and_plan(
        self,
        granted: &[RecipePermission],
    ) -> Result<RecipeExecutionPlan, ProtocolError> {
        self.manifest
            .authorize(granted)
            .map_err(ProtocolError::InvalidRecipe)?;
        self.into_execution_plan()
    }
}

impl Request {
    /// Validates the envelope and converts the DTO into a domain command.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnsupportedApiVersion`] when the caller's API
    /// contract is not supported.
    pub fn into_command(self) -> Result<Command, ProtocolError> {
        if self.api_version != API_VERSION {
            return Err(ProtocolError::UnsupportedApiVersion {
                requested: self.api_version,
            });
        }
        self.operation.into_command()
    }
}

impl TransactionRequest {
    /// Validates the envelope and converts every operation to a typed command.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnsupportedApiVersion`] when the caller's API
    /// contract is not supported.
    pub fn into_transaction(self) -> Result<ProtocolTransaction, ProtocolError> {
        if self.api_version != API_VERSION {
            return Err(ProtocolError::UnsupportedApiVersion {
                requested: self.api_version,
            });
        }
        Ok(ProtocolTransaction {
            label: self.label,
            expected_revision: self.expected_revision,
            dry_run: self.dry_run,
            commands: self
                .operations
                .into_iter()
                .map(Operation::into_command)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl Operation {
    #[allow(clippy::too_many_lines)]
    fn into_command(self) -> Result<Command, ProtocolError> {
        Ok(match self {
            Self::ImportMedia { id, source_path } => Command::ImportMedia {
                id: MediaAssetId::from_uuid(id),
                source_path,
            },
            Self::SetMediaProxy {
                media_id,
                proxy_path,
                source_sha256,
                source_bytes,
            } => Command::SetMediaProxy {
                media_id: MediaAssetId::from_uuid(media_id),
                proxy_path,
                source_fingerprint: match (source_sha256, source_bytes) {
                    (Some(sha256), Some(byte_length)) => Some(ProxySourceFingerprint {
                        sha256,
                        byte_length,
                    }),
                    (None, None) => None,
                    _ => {
                        return Err(ProtocolError::InvalidProxyFingerprint);
                    }
                },
            },
            Self::CreateSequence { id, name } => Command::CreateSequence {
                id: SequenceId::from_uuid(id),
                name,
            },
            Self::AddMarker {
                sequence_id,
                id,
                time,
                label,
            } => Command::AddMarker {
                sequence_id: SequenceId::from_uuid(sequence_id),
                id: MarkerId::from_uuid(id),
                time: time.try_into()?,
                label,
            },
            Self::DeleteMarker {
                sequence_id,
                marker_id,
            } => Command::DeleteMarker {
                sequence_id: SequenceId::from_uuid(sequence_id),
                marker_id: MarkerId::from_uuid(marker_id),
            },
            Self::AddTransition {
                sequence_id,
                id,
                outgoing_clip,
                incoming_clip,
                duration,
                alignment,
                kind,
            } => Command::AddTransition {
                sequence_id: SequenceId::from_uuid(sequence_id),
                id: prism_core::TransitionId::from_uuid(id),
                outgoing_clip: ClipId::from_uuid(outgoing_clip),
                incoming_clip: ClipId::from_uuid(incoming_clip),
                duration: duration.try_into()?,
                alignment: alignment.into(),
                kind: kind.into(),
            },
            Self::DeleteTransition {
                sequence_id,
                transition_id,
            } => Command::DeleteTransition {
                sequence_id: SequenceId::from_uuid(sequence_id),
                transition_id: prism_core::TransitionId::from_uuid(transition_id),
            },
            Self::CreateTrack {
                sequence_id,
                id,
                kind,
            } => Command::CreateTrack {
                sequence_id: SequenceId::from_uuid(sequence_id),
                id: TrackId::from_uuid(id),
                kind: kind.into(),
            },
            Self::SetTrackLocked {
                sequence_id,
                track_id,
                locked,
            } => Command::SetTrackLocked {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                locked,
            },
            Self::SetTrackVisible {
                sequence_id,
                track_id,
                visible,
            } => Command::SetTrackVisible {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                visible,
            },
            Self::SetTrackMuted {
                sequence_id,
                track_id,
                muted,
            } => Command::SetTrackMuted {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                muted,
            },
            Self::ReorderTrack {
                sequence_id,
                track_id,
                destination_index,
            } => Command::ReorderTrack {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                destination_index,
            },
            Self::InsertClip {
                sequence_id,
                track_id,
                id,
                media_id,
                timeline_start,
                source_start,
                source_duration,
            } => Command::InsertClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                id: ClipId::from_uuid(id),
                media_id: MediaAssetId::from_uuid(media_id),
                timeline_start: timeline_start.try_into()?,
                source_start: source_start.try_into()?,
                source_duration: source_duration.try_into()?,
            },
            Self::DuplicateClip {
                sequence_id,
                track_id,
                clip_id,
                new_clip_id,
                timeline_start,
            } => Command::DuplicateClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                new_clip_id: ClipId::from_uuid(new_clip_id),
                timeline_start: timeline_start.try_into()?,
            },
            Self::MoveClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
            } => Command::MoveClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                timeline_start: timeline_start.try_into()?,
            },
            Self::SlipClip {
                sequence_id,
                track_id,
                clip_id,
                source_delta,
            } => Command::SlipClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                source_delta: source_delta.try_into()?,
            },
            Self::RollEdit {
                sequence_id,
                track_id,
                left_clip_id,
                right_clip_id,
                delta,
            } => Command::RollEdit {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                left_clip_id: ClipId::from_uuid(left_clip_id),
                right_clip_id: ClipId::from_uuid(right_clip_id),
                delta: delta.try_into()?,
            },
            Self::SlideEdit {
                sequence_id,
                track_id,
                previous_clip_id,
                clip_id,
                next_clip_id,
                delta,
            } => Command::SlideEdit {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                previous_clip_id: ClipId::from_uuid(previous_clip_id),
                clip_id: ClipId::from_uuid(clip_id),
                next_clip_id: ClipId::from_uuid(next_clip_id),
                delta: delta.try_into()?,
            },
            Self::TrimClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
                source_start,
                source_duration,
            } => Command::TrimClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                timeline_start: timeline_start.try_into()?,
                source_start: source_start.try_into()?,
                source_duration: source_duration.try_into()?,
            },
            Self::SplitClip {
                sequence_id,
                track_id,
                clip_id,
                right_clip_id,
                timeline_position,
            } => Command::SplitClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                right_clip_id: ClipId::from_uuid(right_clip_id),
                timeline_position: timeline_position.try_into()?,
            },
            Self::DeleteClip {
                sequence_id,
                track_id,
                clip_id,
            } => Command::DeleteClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
            },
            Self::RippleDelete {
                sequence_id,
                track_id,
                clip_id,
            } => Command::RippleDelete {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
            },
        })
    }
}

impl TryFrom<RationalTimeDto> for RationalTime {
    type Error = ProtocolError;

    fn try_from(value: RationalTimeDto) -> Result<Self, Self::Error> {
        RationalTime::new(value.value, value.timescale).map_err(|_| ProtocolError::InvalidTimebase)
    }
}

impl From<TrackKindDto> for TrackKind {
    fn from(value: TrackKindDto) -> Self {
        match value {
            TrackKindDto::Video => Self::Video,
            TrackKindDto::Audio => Self::Audio,
        }
    }
}

impl From<TransitionKindDto> for prism_project::TransitionKind {
    fn from(value: TransitionKindDto) -> Self {
        match value {
            TransitionKindDto::Dissolve => Self::Dissolve,
        }
    }
}

impl From<TransitionAlignmentDto> for prism_project::TransitionAlignment {
    fn from(value: TransitionAlignmentDto) -> Self {
        match value {
            TransitionAlignmentDto::StartAtCut => Self::StartAtCut,
            TransitionAlignmentDto::Centered => Self::Centered,
            TransitionAlignmentDto::EndAtCut => Self::EndAtCut,
        }
    }
}
