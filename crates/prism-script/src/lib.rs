//! Language-neutral scripting protocol boundary.
//!
//! Protocol DTOs are deliberately separate from domain and command types.
//! Conversion is the validation boundary; serialized input never mutates a
//! project directly.

use prism_commands::Command;
use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_project::TrackKind;
use prism_timeline::RationalTime;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// First supported scripting protocol version.
pub const API_VERSION: &str = "1";

/// A request envelope suitable for JSON-RPC-like transports.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// Correlation identifier supplied by the client.
    pub request_id: String,
    /// Required protocol version.
    pub api_version: String,
    /// Declared operation.
    pub operation: Operation,
}

/// A named atomic script transaction containing one or more operations.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRequest {
    /// Correlation identifier supplied by the client.
    pub request_id: String,
    /// Required protocol version.
    pub api_version: String,
    /// Project revision observed before the script planned its changes.
    pub expected_revision: u64,
    /// User-visible undo label.
    pub label: String,
    /// Validate and report commands without committing when true.
    pub dry_run: bool,
    /// Ordered operations committed as one transaction.
    pub operations: Vec<Operation>,
}

/// Validated protocol transaction ready for the command layer.
#[derive(Debug)]
pub struct ProtocolTransaction {
    /// User-visible undo label.
    pub label: String,
    /// Revision required for commit.
    pub expected_revision: u64,
    /// Whether the caller requested validation without commit.
    pub dry_run: bool,
    /// Strongly typed commands in request order.
    pub commands: Vec<Command>,
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
    /// Create an empty sequence.
    CreateSequence {
        /// Stable identifier encoded as a UUID.
        id: Uuid,
        /// User-visible name.
        name: String,
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
    fn into_command(self) -> Result<Command, ProtocolError> {
        Ok(match self {
            Self::ImportMedia { id, source_path } => Command::ImportMedia {
                id: MediaAssetId::from_uuid(id),
                source_path,
            },
            Self::CreateSequence { id, name } => Command::CreateSequence {
                id: SequenceId::from_uuid(id),
                name,
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

#[cfg(test)]
mod tests {
    use super::{API_VERSION, ProtocolError, Request, TransactionRequest};

    #[test]
    fn rejects_unknown_api_version_before_command_execution() -> Result<(), serde_json::Error> {
        let json = r#"{
            "requestId":"req-1",
            "apiVersion":"999",
            "operation":{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}
        }"#;
        let request: Request = serde_json::from_str(json)?;
        assert!(matches!(
            request.into_command(),
            Err(ProtocolError::UnsupportedApiVersion { requested }) if requested == "999"
        ));
        Ok(())
    }

    #[test]
    fn accepts_current_api_version() -> Result<(), serde_json::Error> {
        let json = format!(
            r#"{{"requestId":"req-1","apiVersion":"{API_VERSION}","operation":{{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}}}}"#
        );
        let request: Request = serde_json::from_str(&json)?;
        assert!(request.into_command().is_ok());
        Ok(())
    }

    #[test]
    fn converts_atomic_batch_and_preserves_dry_run() -> Result<(), Box<dyn std::error::Error>> {
        let json = format!(
            r#"{{
                "requestId":"req-2",
                "apiVersion":"{API_VERSION}",
                "expectedRevision":7,
                "label":"Prepare timeline",
                "dryRun":true,
                "operations":[
                    {{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}},
                    {{"type":"createSequence","id":"00000000-0000-0000-0000-000000000002","name":"Social"}}
                ]
            }}"#
        );
        let request: TransactionRequest = serde_json::from_str(&json)?;
        let transaction = request.into_transaction()?;
        assert!(transaction.dry_run);
        assert_eq!(transaction.expected_revision, 7);
        assert_eq!(transaction.label, "Prepare timeline");
        assert_eq!(transaction.commands.len(), 2);
        Ok(())
    }

    #[test]
    fn rejects_zero_timescale_at_protocol_boundary() -> Result<(), Box<dyn std::error::Error>> {
        let json = format!(
            r#"{{
                "requestId":"req-3",
                "apiVersion":"{API_VERSION}",
                "operation":{{
                    "type":"moveClip",
                    "sequenceId":"00000000-0000-0000-0000-000000000001",
                    "trackId":"00000000-0000-0000-0000-000000000002",
                    "clipId":"00000000-0000-0000-0000-000000000003",
                    "timelineStart":{{"value":1,"timescale":0}}
                }}
            }}"#
        );
        let request: Request = serde_json::from_str(&json)?;
        assert!(matches!(
            request.into_command(),
            Err(ProtocolError::InvalidTimebase)
        ));
        Ok(())
    }
}
