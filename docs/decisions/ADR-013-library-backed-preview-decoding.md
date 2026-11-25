# ADR-013: Library-backed preview decoding

Status: Accepted for Task 8 design; experimental implementation remains feature
gated and is not on the Studio path.

## Context

Studio preview currently uses one `FFmpeg` subprocess per paused seek and
`FFplay` for audio. Task 0 measured ten one-minute-interval seeks through the
real Studio worker at 96.736 ms median and 144.823 ms maximum. Its SDL-dummy
audio master advanced 5,447 ms less than monotonic time over ten minutes. Video
scheduling follows the reported audio-master position exactly, but physical
speaker/display sync is not instrumented.

Task 1 tested whether a reusable in-process demuxer, video decoder, scaler,
bounded RGBA frame cache and forward prefetch should replace subprocess decoding
in the future playback design. The spike uses `ffmpeg-next` 9.1.0 behind the
non-default `library-decoder-spike` feature. Studio does not enable or call it.

## Fixture and method

Hardware and tool versions match `docs/BASELINE.md`. The source is its ten-minute
1920x1080 30 fps H.264/AAC fixture. The proxy was generated as 960x540 H.264 with
the encoder's default GOP:

```sh
mkdir -p /tmp/prism-task1-spike
ffmpeg -hide_banner -loglevel error -y \
  -i /tmp/prism-task0-baseline/source-10m-1080p.mp4 \
  -vf scale=960:540 -c:v libx264 -preset veryfast -crf 23 -an \
  /tmp/prism-task1-spike/proxy-10m-540p.mp4
PRISM_TASK1_MEDIA=/tmp/prism-task0-baseline/source-10m-1080p.mp4 \
PRISM_TASK1_PROXY=/tmp/prism-task1-spike/proxy-10m-540p.mp4 \
  cargo test -p prism-playback --features library-decoder-spike \
  benchmarks_preview_modes_on_long_media -- --ignored --nocapture
```

Each mode performs ten cold random seeks at one-minute intervals, then 60
consecutive 30 fps requests. A request taking more than 33.333 ms counts as a
dropped-frame deadline. The cache limit is 96 MiB and prefetch is four frames.
These are debug-build decoder timings; they exclude UI delivery and scanout.

| Mode | Seek median | Seek max | Sequential p95 | Missed 30 fps deadlines | Retained RGBA cache |
|---|---:|---:|---:|---:|---:|
| Full 1920x1080 | 14.617 ms | 19.134 ms | 40.089 ms | 5/60 | 99,532,800 bytes |
| Half 960x540 | 17.423 ms | 17.955 ms | 45.144 ms | 6/60 | 99,532,800 bytes |
| Quarter 480x270 | 12.339 ms | 12.454 ms | 41.064 ms | 6/60 | 54,432,000 bytes |
| Proxy 960x540 | 63.967 ms | 120.813 ms | 26.067 ms | 2/60 | 99,532,800 bytes |

Full, Half and Quarter random seeks materially beat the Task 0 subprocess
baseline. Scaling is not the dominant cost in this debug run. The proxy has the
best sequential p95 but poor random seeks because its default long GOP requires
decoding many frames after a seek (1,435 total decoded frames versus about 255
for the source modes). Proxy generation therefore needs an explicit short-GOP
preview profile before proxy latency can be treated as representative.

This video-only, off-path spike cannot produce a new honest A/V drift number.
Its scheduling drift relative to the existing audio-master timestamp is 0 ms by
construction, as in Task 0; device drift remains unqualified. Claiming the Task
0 dummy-backend wall-clock result as a decoder result would conflate independent
systems.

## Decision

Go: Task 8 will use persistent library-backed decoded buffers and an audio-master
timeline scheduler, with bounded caches, prefetch, generation-based stale-result
rejection and cooperative cancellation. Preview and export must consume the same
decoded-buffer and mixing semantics. Subprocess decoding remains the fallback
until the production implementation passes codec packaging, cancellation,
malformed-media, physical-device sync and release-build playback gates.

Do not promote this spike directly. Before Studio integration, add timestamped
decoded frames, codec send/receive error classification, seek preroll policy,
EOF draining, cancellation between packets/frames, cache keys that include source
identity and output format, and a short-GOP proxy preset. Measure process RSS in
addition to retained cache bytes and qualify speaker/display sync on approved
hardware.

## Consequences

Task 8 has a concrete decoder/scheduler direction without changing the current
main path. `ffmpeg-next` stays optional, so default builds and packaging remain
unchanged. The result supports library-backed decoding, but does not yet select
a stable wrapper/API or make a codec distribution claim.
