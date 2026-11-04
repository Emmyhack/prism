# Milestone P1.4 headless export CLI

Date: 2026-11-04. Status: In development; saved-project track export tested.

## Outcome

Replaced the CLI stub with a real `export-track` command. It accepts a project
path, stable sequence and track UUIDs, output path, dimensions, rational frame
rate, and explicit `FFmpeg`/ffprobe executable paths. Parsing supports non-UTF-8
filesystem paths while requiring textual IDs and numeric settings to be valid
UTF-8. Unknown, duplicate, missing, and malformed options return structured
errors and a nonzero process status.

The command loads only through the validated persistence adapter and delegates
timing and rendering to `prism-export`. It does not mutate project state or
reimplement timeline semantics.

## Actual workflow verification

The opt-in integration builds media, sequence, track, and clip state through the
command engine, atomically saves the `.prism` database, and launches the compiled
CLI as a separate process. The process selects the saved objects by UUID, exports
the track, reports success, and produces an independently probed video-only MP4.

## Verification

- Formatting passes.
- Strict workspace Clippy passes with warnings denied.
- 44 default tests pass.
- Two media, two playback, three export-library, and one CLI actual-fixture tests
  pass with explicit local `FFmpeg`/ffprobe executables.

## Limitations and next work

The CLI cannot yet create or inspect projects, discover object IDs, edit clips,
report progress, or cancel export. It exposes the existing video-only single-track
renderer and inherits its reference-codec licensing boundary. Next, add truthful
read-only project inspection so users and scripts can discover sequences, tracks,
clips, revisions, and media references without opening SQLite directly.
