"""Builders for Prism's language-neutral transaction envelope."""

from dataclasses import dataclass
import json
from typing import Any

API_VERSION = "1"


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
class TransactionRequest:
    request_id: str
    expected_revision: int
    label: str
    operations: tuple[MoveClip, ...]
    dry_run: bool = True
    api_version: str = API_VERSION

    def as_dict(self) -> dict[str, Any]:
        if not self.label.strip():
            raise ValueError("label must not be empty")
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
