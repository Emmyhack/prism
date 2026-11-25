# Performance baselines

## Task 0: 2026-11-25

Apple Silicon Mac, Darwin 27.2.0, debug build, Rust 1.94.1, Homebrew FFmpeg
9.0.2. The generated source is ten minutes of 1920x1080 30 fps H.264 plus 48 kHz
AAC. See `docs/BASELINE.md` for exact generation and reproduction commands.

| Operation | Cache/sample conditions | Result |
|---|---|---:|
| Studio paused scrub, request to decoded frame | 10 seeks at one-minute intervals; subprocess path; warm build | median 96.736 ms, max 144.823 ms |
| FFplay SDL-dummy audio-master vs monotonic wall time | 599.910 seconds of clock progress | -5,447 ms (-0.90%) |
| Encoded A/V stream end delta | FFprobe, 600-second synthetic source | 0 ms |
| Headless Studio project summary | Small project, warm debug build, one run | 1.86 s |

Startup, import/probe, dropped frames, cache hit rate, memory, proxy generation,
waveform generation, export speed and autosave latency remain unmeasured. The
scrub result excludes UI event delivery and display scanout. The drift result is
from a dummy audio backend, not physical hardware.

## Initial budgets

These are hypotheses, not achieved results:

| Operation | Phase 1 target |
|---|---:|
| In-memory single clip command p95 | < 2 ms |
| Timeline viewport input response p95 | < 16 ms |
| Warm cached 1080p frame seek p95 | < 50 ms |
| Script bridge overhead per batched command p95 | < 1 ms |

Representative fixtures will include short 1080p, long interview, mixed-rate,
4K multitrack, large library, effects-heavy, and long export projects.

## Task 1 playback spike: 2026-11-25

The feature-gated in-process decoder used the Task 0 ten-minute source, a 96 MiB
RGBA cache and four-frame prefetch. Full/Half/Quarter seek medians were 14.617,
17.423 and 12.339 ms; maxima were 19.134, 17.955 and 12.454 ms. A default-GOP
960x540 proxy measured 63.967 ms median and 120.813 ms maximum. Sequential 30 fps
p95 was 40.089/45.144/41.064/26.067 ms with 5/6/6/2 of 60 deadlines missed.
Retained cache bytes were bounded at 99,532,800 except Quarter at 54,432,000.
See `docs/decisions/ADR-013-library-backed-preview-decoding.md` for method,
limitations and the go recommendation.
