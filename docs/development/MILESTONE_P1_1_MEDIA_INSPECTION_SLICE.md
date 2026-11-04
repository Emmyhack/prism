# Milestone P1.1 media inspection slice

Date: 2026-11-04. Status: In development; metadata subset tested.

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

## Actual-media verification

Generated `fixtures/media/av-ntsc-48k.mp4` from FFmpeg test sources. It contains
one second of 320×180 MPEG-4 Part 2 video at 30000/1001 fps and mono AAC at
48 kHz. Its SHA-256 and reproduction command are documented beside the fixture.

The opt-in integration test successfully verified container duration, two
streams, dimensions, exact frame rate, constant-rate evidence, sample rate, and
channel count using `/opt/homebrew/bin/ffprobe` 9.0.2.

## Licensing boundary

The installed Homebrew FFmpeg build enables GPL and version-3 components,
including x264 and x265. It is a local development/reference tool only and is
not an approved Prism distribution configuration.

## Verification

- Formatting passes.
- Strict workspace Clippy passes with warnings denied.
- 27 default tests pass.
- One ignored-by-default actual-media integration test passes when
  `PRISM_FFPROBE=/opt/homebrew/bin/ffprobe` is supplied.

## Limitations and next work

Metadata inspection is not decoding support. Seeking, packet timestamp analysis,
corrupt-input containment, audio/video synchronization, hardware acceleration,
and export remain unimplemented. Next, add packet-level VFR evidence and media
fingerprinting, then introduce an isolated library-backed decode worker and
validate seek/frame behavior against an expanded fixture matrix.
