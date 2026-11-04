# Milestone P1.2 timeline evaluation slice

Date: 2026-11-04. Status: In development; video source planning subset tested.

## Outcome

Added `prism-playback`, a UI- and codec-independent evaluator that maps an exact
sequence timestamp to at most one active clip per video track and computes its
exact source timestamp. Clip ranges are start-inclusive and end-exclusive.
Gaps produce no source request, audio tracks are excluded from the video plan,
and stored track order is preserved without prematurely defining compositor
layer semantics.

This keeps authoritative edit interpretation outside decoders, preview widgets,
and exporters. Those consumers will share the same plan rather than independently
reimplementing clip timing.

## Actual-media verification

The opt-in integration constructs a real project referencing the generated
MP4/AAC fixture. It places a clip at sequence time 1/4 with source start 1/4,
evaluates sequence time 1/2, and verifies the resulting source request is exactly
1/2. The reference decoder then produces the already pinned RGBA frame hash for
that source timestamp.

## Verification

- Formatting passes.
- Strict workspace Clippy passes with warnings denied.
- 35 default tests pass.
- Two media and one playback actual-fixture tests pass with explicitly supplied
  local `FFmpeg`/ffprobe executables.

## Limitations and next work

The planner currently models unretimed video clips only because speed, track
visibility, effects, transitions, and compositing are not yet authoritative
project fields. It does not decode asynchronously, schedule playback, evaluate
audio, or present frames. Next, add a bounded asynchronous decode worker boundary
with stale-request cancellation, then connect it to this plan without moving
timeline semantics into the worker.
