# Milestone P1.3 basic export slice

Date: 2026-11-04. Status: In development; single-source MP4 subset tested.

## Outcome

Added `prism-export` with a real, narrowly scoped MP4 workflow. It accepts one
exact nonnegative source start and positive duration, invokes explicitly selected
`FFmpeg` and ffprobe executables without a shell, and produces MPEG-4 Part 2 video
with optional AAC audio.

The adapter refuses missing sources, unavailable output parents, invalid ranges,
and existing destination files. Encoding occurs in a uniquely named temporary
file beside the destination. The temporary file is independently probed for a
positive duration and video stream and checked for nonzero length before
same-filesystem no-clobber hard-link finalization. This prevents replacement even
if a destination appears during encoding. Failure paths remove the temporary file.

The crate also builds a deterministic plan for one authoritative video track.
It preserves clip IDs, asset IDs, source trims, sequence placement, and exact
durations while inserting explicit leading and inter-clip gap segments. It does
not invent a trailing gap because the current sequence model has no independent
duration. Invalid or non-video track layouts fail before encoding.

## Actual-media verification

The opt-in integration exports the 1/4-through-3/4-second range of the synthetic
MP4/AAC fixture. It verifies the finalized file is nonempty and contains video
and audio, independently probes 320×180 video, and decodes a complete RGBA frame
from the result. The test removes its explicit temporary directory afterward.

## Verification

- Formatting passes.
- Strict workspace Clippy passes with warnings denied.
- 41 default tests pass.
- Two media, two playback, and one export actual-fixture tests pass with explicit
  local `FFmpeg`/ffprobe executables.

## Limitations and next work

This slice exports a source range, not a fully evaluated multitrack sequence.
It has no progress reporting, cancellation, hardware encoding, presets, effects,
color transforms, or codec capability negotiation. The installed Homebrew build
remains a local GPL-enabled reference tool and is not approved for distribution.
Next, render this plan into one MP4 using deterministic black gap frames and
source clip ranges, then independently compare its timing with preview plans.
