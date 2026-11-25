"""Build a trimmed talking-head sequence and request a Prism sequence export."""

from decimal import Decimal
from fractions import Fraction
import json
import os
from pathlib import Path
import subprocess
import uuid


def exact(value):
    fraction = Fraction(value)
    return {"value": fraction.numerator, "timescale": fraction.denominator}


def probe(path):
    executable = os.environ.get("PRISM_FFPROBE")
    if not executable:
        raise RuntimeError("PRISM_FFPROBE is not set")
    result = subprocess.run(
        [executable, "-v", "error", "-show_entries", "format=duration:stream=codec_type", "-of", "json", str(path)],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    metadata = json.loads(result.stdout, parse_float=Decimal)
    kinds = {stream.get("codec_type") for stream in metadata.get("streams", [])}
    if not {"video", "audio"}.issubset(kinds):
        raise ValueError(f"clip must contain video and audio: {path.name}")
    return Fraction(Decimal(metadata["format"]["duration"]))


inputs = json.loads(os.environ["PRISM_RECIPE_INPUTS"], parse_float=Decimal)
folder = Path(inputs["folder"]).resolve()
config_path = Path(inputs["config"]).resolve()
output_path = Path(inputs["output"]).resolve()
config = json.loads(config_path.read_text(encoding="utf-8"), parse_float=Decimal)
names = list(config.get("order", []))
intro = config.get("intro")
if intro:
    names.insert(0, intro)
if not names:
    raise ValueError("config order must contain at least one clip")

transition = Fraction(config.get("transitionSeconds", 0))
if transition < 0:
    raise ValueError("transitionSeconds must not be negative")
half = transition / 2
trims = config.get("trims", {})
sequence_id = str(uuid.uuid4())
video_track_id = str(uuid.uuid4())
audio_track_id = str(uuid.uuid4())
operations = [
    {"type": "createSequence", "id": sequence_id, "name": config.get("name", "Podcast edit")},
    {"type": "createTrack", "sequenceId": sequence_id, "id": video_track_id, "kind": "video"},
    {"type": "createTrack", "sequenceId": sequence_id, "id": audio_track_id, "kind": "audio"},
]
timeline = Fraction(0)
video_clips = []

for index, name in enumerate(names):
    path = (folder / name).resolve()
    if path.parent != folder or not path.is_file():
        raise ValueError(f"clip is outside the folder or missing: {name}")
    media_duration = probe(path)
    trim = trims.get(name, {})
    default_start = half if index > 0 else Fraction(0)
    default_end = media_duration - half if index + 1 < len(names) else media_duration
    source_start = Fraction(trim.get("start", default_start))
    source_end = Fraction(trim.get("end", default_end))
    if source_start < (half if index > 0 else 0):
        raise ValueError(f"incoming transition handle is unavailable: {name}")
    if source_end + (half if index + 1 < len(names) else 0) > media_duration:
        raise ValueError(f"outgoing transition handle is unavailable: {name}")
    duration = source_end - source_start
    if duration <= 0:
        raise ValueError(f"trim is empty: {name}")
    media_id = str(uuid.uuid4())
    video_clip_id = str(uuid.uuid4())
    audio_clip_id = str(uuid.uuid4())
    operations.append({"type": "importMedia", "id": media_id, "sourcePath": str(path)})
    for track_id, clip_id in ((video_track_id, video_clip_id), (audio_track_id, audio_clip_id)):
        operations.append({
            "type": "insertClip", "sequenceId": sequence_id, "trackId": track_id,
            "id": clip_id, "mediaId": media_id, "timelineStart": exact(timeline),
            "sourceStart": exact(source_start), "sourceDuration": exact(duration),
        })
    video_clips.append(video_clip_id)
    timeline += duration

if transition > 0:
    for outgoing, incoming in zip(video_clips, video_clips[1:]):
        operations.append({
            "type": "addTransition", "sequenceId": sequence_id, "id": str(uuid.uuid4()),
            "outgoingClip": outgoing, "incomingClip": incoming,
            "duration": exact(transition), "alignment": "centered", "kind": "dissolve",
        })

transaction = {
    "requestId": str(uuid.uuid4()), "apiVersion": "1",
    "expectedRevision": int(os.environ["PRISM_PROJECT_REVISION"]),
    "label": "Podcast folder recipe", "dryRun": False, "operations": operations,
}
print(json.dumps({
    "transaction": transaction,
    "export": {"sequenceId": sequence_id, "output": str(output_path), "preset": inputs["preset"]},
}, separators=(",", ":")))
