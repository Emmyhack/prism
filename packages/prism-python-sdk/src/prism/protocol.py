"""Builders for Prism's language-neutral transaction envelope."""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Any

API_VERSION = "1"


@dataclass(frozen=True)
class RecipeRequest:
    """Language-neutral recipe invocation envelope.

    Manifest validation, permission authorization, and execution remain host
    responsibilities; this builder only emits the shared JSON shape.
    """

    manifest: dict[str, Any]
    inputs: dict[str, Any]

    def as_dict(self) -> dict[str, Any]:
        if not isinstance(self.manifest, dict):
            raise TypeError("manifest must be a mapping")
        if not isinstance(self.inputs, dict):
            raise TypeError("inputs must be a mapping")
        return {"manifest": self.manifest, "inputs": self.inputs}

    def to_json(self) -> str:
        return json.dumps(self.as_dict(), indent=2, sort_keys=True) + "\n"


@dataclass(frozen=True)
class ImportMedia:
    id: str
    source_path: str

    def as_dict(self) -> dict[str, Any]:
        return {"type": "importMedia", "id": self.id, "sourcePath": self.source_path}


@dataclass(frozen=True)
class SetMediaProxy:
    media_id: str
    proxy_path: str | None

    def as_dict(self) -> dict[str, Any]:
        if self.proxy_path is not None and not self.proxy_path.strip():
            raise ValueError("proxy_path must not be empty")
        return {"type": "setMediaProxy", "mediaId": self.media_id, "proxyPath": self.proxy_path}


@dataclass(frozen=True)
class CreateSequence:
    id: str
    name: str

    def as_dict(self) -> dict[str, Any]:
        return {"type": "createSequence", "id": self.id, "name": self.name}


@dataclass(frozen=True)
class CreateTrack:
    sequence_id: str
    id: str
    kind: str

    def as_dict(self) -> dict[str, Any]:
        if self.kind not in {"video", "audio"}:
            raise ValueError("kind must be video or audio")
        return {"type": "createTrack", "sequenceId": self.sequence_id, "id": self.id, "kind": self.kind}


@dataclass(frozen=True)
class SetTrackLocked:
    sequence_id: str
    track_id: str
    locked: bool

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "setTrackLocked",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "locked": self.locked,
        }


@dataclass(frozen=True)
class SetTrackVisible:
    sequence_id: str
    track_id: str
    visible: bool

    def as_dict(self) -> dict[str, Any]:
        if not isinstance(self.visible, bool):
            raise TypeError("visible must be a boolean")
        return {
            "type": "setTrackVisible",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "visible": self.visible,
        }


@dataclass(frozen=True)
class SetTrackMuted:
    sequence_id: str
    track_id: str
    muted: bool

    def as_dict(self) -> dict[str, Any]:
        if not isinstance(self.muted, bool):
            raise TypeError("muted must be a boolean")
        return {
            "type": "setTrackMuted",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "muted": self.muted,
        }


@dataclass(frozen=True)
class ReorderTrack:
    sequence_id: str
    track_id: str
    destination_index: int

    def as_dict(self) -> dict[str, Any]:
        if self.destination_index < 0:
            raise ValueError("destination_index must be nonnegative")
        return {
            "type": "reorderTrack",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "destinationIndex": self.destination_index,
        }


@dataclass(frozen=True)
class AddMarker:
    sequence_id: str
    id: str
    time: RationalTime
    label: str

    def as_dict(self) -> dict[str, Any]:
        if not self.label.strip():
            raise ValueError("label must not be empty")
        return {"type": "addMarker", "sequenceId": self.sequence_id, "id": self.id, "time": self.time.as_dict(), "label": self.label}


@dataclass(frozen=True)
class DeleteMarker:
    sequence_id: str
    marker_id: str

    def as_dict(self) -> dict[str, Any]:
        return {"type": "deleteMarker", "sequenceId": self.sequence_id, "markerId": self.marker_id}


@dataclass(frozen=True)
class DuplicateClip:
    sequence_id: str
    track_id: str
    clip_id: str
    new_clip_id: str
    timeline_start: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "duplicateClip", "sequenceId": self.sequence_id, "trackId": self.track_id,
            "clipId": self.clip_id, "newClipId": self.new_clip_id,
            "timelineStart": self.timeline_start.as_dict(),
        }


@dataclass(frozen=True)
class RippleDelete:
    sequence_id: str
    track_id: str
    clip_id: str

    def as_dict(self) -> dict[str, Any]:
        return {"type": "rippleDelete", "sequenceId": self.sequence_id, "trackId": self.track_id, "clipId": self.clip_id}


@dataclass(frozen=True)
class AddTransition:
    sequence_id: str
    id: str
    outgoing_clip: str
    incoming_clip: str
    duration: RationalTime
    kind: str = "dissolve"

    def as_dict(self) -> dict[str, Any]:
        if self.kind != "dissolve":
            raise ValueError("kind must be dissolve")
        return {"type": "addTransition", "sequenceId": self.sequence_id, "id": self.id, "outgoingClip": self.outgoing_clip, "incomingClip": self.incoming_clip, "duration": self.duration.as_dict(), "kind": self.kind}


@dataclass(frozen=True)
class DeleteTransition:
    sequence_id: str
    transition_id: str

    def as_dict(self) -> dict[str, Any]:
        return {"type": "deleteTransition", "sequenceId": self.sequence_id, "transitionId": self.transition_id}


@dataclass(frozen=True)
class InsertClip:
    sequence_id: str
    track_id: str
    id: str
    media_id: str
    timeline_start: RationalTime
    source_start: RationalTime
    source_duration: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "insertClip", "sequenceId": self.sequence_id, "trackId": self.track_id,
            "id": self.id, "mediaId": self.media_id,
            "timelineStart": self.timeline_start.as_dict(),
            "sourceStart": self.source_start.as_dict(),
            "sourceDuration": self.source_duration.as_dict(),
        }


@dataclass(frozen=True)
class RationalTime:
    value: int
    timescale: int

    def __post_init__(self) -> None:
        if self.timescale <= 0:
            raise ValueError("timescale must be greater than zero")

    def as_dict(self) -> dict[str, int]:
        return {"value": self.value, "timescale": self.timescale}


@dataclass(frozen=True)
class MoveClip:
    sequence_id: str
    track_id: str
    clip_id: str
    timeline_start: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "moveClip",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "clipId": self.clip_id,
            "timelineStart": self.timeline_start.as_dict(),
        }


@dataclass(frozen=True)
class SlipClip:
    sequence_id: str
    track_id: str
    clip_id: str
    source_delta: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "slipClip",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "clipId": self.clip_id,
            "sourceDelta": self.source_delta.as_dict(),
        }


@dataclass(frozen=True)
class RollEdit:
    sequence_id: str
    track_id: str
    left_clip_id: str
    right_clip_id: str
    delta: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "rollEdit",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "leftClipId": self.left_clip_id,
            "rightClipId": self.right_clip_id,
            "delta": self.delta.as_dict(),
        }


@dataclass(frozen=True)
class SlideEdit:
    sequence_id: str
    track_id: str
    previous_clip_id: str
    clip_id: str
    next_clip_id: str
    delta: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "slideEdit",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "previousClipId": self.previous_clip_id,
            "clipId": self.clip_id,
            "nextClipId": self.next_clip_id,
            "delta": self.delta.as_dict(),
        }


@dataclass(frozen=True)
class TrimClip:
    sequence_id: str
    track_id: str
    clip_id: str
    timeline_start: RationalTime
    source_start: RationalTime
    source_duration: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "trimClip",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "clipId": self.clip_id,
            "timelineStart": self.timeline_start.as_dict(),
            "sourceStart": self.source_start.as_dict(),
            "sourceDuration": self.source_duration.as_dict(),
        }


@dataclass(frozen=True)
class SplitClip:
    sequence_id: str
    track_id: str
    clip_id: str
    right_clip_id: str
    timeline_position: RationalTime

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "splitClip",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "clipId": self.clip_id,
            "rightClipId": self.right_clip_id,
            "timelinePosition": self.timeline_position.as_dict(),
        }


@dataclass(frozen=True)
class DeleteClip:
    sequence_id: str
    track_id: str
    clip_id: str

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": "deleteClip",
            "sequenceId": self.sequence_id,
            "trackId": self.track_id,
            "clipId": self.clip_id,
        }


@dataclass(frozen=True)
class TransactionRequest:
    request_id: str
    expected_revision: int
    label: str
    operations: tuple[Any, ...]
    dry_run: bool = True
    api_version: str = API_VERSION

    def as_dict(self) -> dict[str, Any]:
        if not self.label.strip():
            raise ValueError("label must not be empty")
        if not isinstance(self.expected_revision, int) or self.expected_revision < 0:
            raise ValueError("expected_revision must be a nonnegative integer")
        if not isinstance(self.operations, tuple):
            raise TypeError("operations must be a tuple")
        if not isinstance(self.dry_run, bool):
            raise TypeError("dry_run must be a boolean")
        return {
            "requestId": self.request_id,
            "apiVersion": self.api_version,
            "expectedRevision": self.expected_revision,
            "label": self.label,
            "dryRun": self.dry_run,
            "operations": [operation.as_dict() for operation in self.operations],
        }

    def to_json(self) -> str:
        return json.dumps(self.as_dict(), indent=2, sort_keys=True) + "\n"
