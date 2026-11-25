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
