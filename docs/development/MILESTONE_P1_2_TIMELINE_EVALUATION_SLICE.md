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

The same crate now provides a codec-independent asynchronous decode scheduler.
Both request and completion queues are bounded, submission never blocks the
caller, and playback generations suppress queued or in-flight stale results.
Decoder calls execute on a dedicated worker thread. A saturated completion queue
drops newer frames so shutdown and UI coordination cannot deadlock.

## Actual-media verification

The opt-in integration constructs a real project referencing the generated
MP4/AAC fixture. It places a clip at sequence time 1/4 with source start 1/4,
evaluates sequence time 1/2, and verifies the resulting source request is exactly
1/2. The reference decoder then produces the already pinned RGBA frame hash for
that source timestamp.

A second integration routes an evaluated 1/2-second source request through the
bounded asynchronous scheduler. The dedicated worker invokes the reference
decoder and returns the same pinned RGBA hash, proving the current project-to-
background-decode path without claiming interactive playback or process-level
isolation.

## Verification

- Formatting passes.
- Strict workspace Clippy passes with warnings denied.
- 37 default tests pass.
- Two media and two playback actual-fixture tests pass with explicitly supplied
  local `FFmpeg`/ffprobe executables.

## Limitations and next work

The planner currently models unretimed video clips only because speed, track
visibility, effects, transitions, and compositing are not yet authoritative
project fields. The scheduler does not forcibly interrupt an in-flight native
decoder call, evaluate audio, pace realtime playback, or present frames. Next,
add an adapter that resolves planned assets into the isolated decoder boundary,
with process-level cancellation and crash containment.
