# Milestone P1.1 media inspection slice

Date: 2026-11-04. Status: In development; inspection and reference decode subset tested.

## Outcome

Added `prism-media` with typed runtime capability and media metadata models. The
initial adapter invokes an explicit ffprobe executable without a shell and parses
its JSON into exact duration, stream timebase, nominal/average frame rates,
dimensions, pixel format, color identifiers, rotation, timecode, audio sample
rate, channels, and layout. Decimal and fractional time fields never pass through
floating point.

VFR status is deliberately evidence-based: matching nominal/average rates are
`ConstantLikely`, differing rates are `VariableLikely`, and missing information
is `Unknown`. Packet timestamp analysis is still required for a verified VFR
classification.

The follow-up packet pass records distinct positive packet durations and
presentation-timestamp deltas as `ObservedConstant`, `ObservedVariable`, or
`Insufficient`. A bounded-memory full-file SHA-256 fingerprint supports exact
fixture identity and lays the foundation for relinking/change detection.

An explicit `FFmpeg` command-line reference adapter now extracts one bounded,
tightly packed RGBA frame at an exact rational timestamp. Seek conversion uses
integer nanoseconds and a named nearest-ties-away policy. This adapter establishes
behavioral vectors for the future decoder worker; it is not the production
playback architecture and must not run on the UI thread.

## Actual-media verification

Generated `fixtures/media/av-ntsc-48k.mp4` from FFmpeg test sources. It contains
one second of 320×180 MPEG-4 Part 2 video at 30000/1001 fps and mono AAC at
48 kHz. Its SHA-256 and reproduction command are documented beside the fixture.

The opt-in integration test successfully verified container duration, two
streams, dimensions, exact frame rate, header and packet-level constant-rate
evidence, 29 video packets with 1001-tick durations, sample rate, channel count,
byte length, and SHA-256 using `/opt/homebrew/bin/ffprobe` 9.0.2.

The reference decoder also extracted 320×180 RGBA frames at zero and one-half
second. The integration test pins their distinct pixel SHA-256 values, dimensions,
and byte counts using `/opt/homebrew/bin/ffmpeg` 9.0.2.

## Licensing boundary

The installed Homebrew FFmpeg build enables GPL and version-3 components,
including x264 and x265. It is a local development/reference tool only and is
not an approved Prism distribution configuration.

## Verification

- Formatting passes.
- Strict workspace Clippy passes with warnings denied.
- 31 default tests pass.
- Two ignored-by-default actual-media integration tests pass when
  `PRISM_FFPROBE=/opt/homebrew/bin/ffprobe` and
  `PRISM_FFMPEG=/opt/homebrew/bin/ffmpeg` are supplied.

## Limitations and next work

The command-line reference adapter is not interactive decoding or playback
support. Corrupt-input containment, an isolated library-backed decoder worker,
audio/video synchronization, hardware acceleration, and export remain
unimplemented. Next, define the decoder worker protocol and lifecycle, then
validate it against these vectors and an expanded fixture matrix.
