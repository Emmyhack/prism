# Task 0 baseline

Measured 2026-11-25 on an Apple Silicon Mac running Darwin 27.2.0. Rust was
`rustc 1.94.1`/`cargo 1.94.1`; media tools were Homebrew FFmpeg, FFprobe and
FFplay 9.0.2. Commands were run from the repository root.

## Quality gates

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --workspace --all-features` | Passed: 295 tests; 0 failed; 19 ignored real-media/measurement tests |
| `PRISM_FFMPEG=/opt/homebrew/bin/ffmpeg PRISM_FFPROBE=/opt/homebrew/bin/ffprobe PRISM_FFPLAY=/opt/homebrew/bin/ffplay SDL_AUDIODRIVER=dummy cargo test --workspace --all-features -- --ignored --test-threads=1` | Passed: all 19 ignored tests; 0 failed. The two Task 0 tests return without measuring unless `PRISM_TASK0_LONG_MEDIA` is set; both were also run separately with the generated ten-minute source. |

The ignored-test run covers three CLI media/export tests, four export fixture
tests, two media fixture tests, two playback fixture tests, six Studio
preview/import tests and two Task 0 long-media measurements. This includes
FFplay clock reporting through SDL's dummy audio driver; it does not measure a
physical audio device or display.

## Small benchmark project

The generated project is a 120-second 1080p sequence with ten contiguous video
clips and two audio tracks. It contains 12 clips total and references one
deterministic ten-minute H.264/AAC source. Project construction uses the public
script transaction protocol and therefore the command engine.

Generate the temporary source:

```sh
mkdir -p /tmp/prism-task0-baseline
ffmpeg -hide_banner -y \
  -f lavfi -i 'testsrc2=size=1920x1080:rate=30:duration=600' \
  -f lavfi -i 'sine=frequency=1000:sample_rate=48000:duration=600' \
  -c:v libx264 -preset ultrafast -tune zerolatency -crf 30 -g 30 \
  -pix_fmt yuv420p -c:a aac -b:a 128k -shortest \
  /tmp/prism-task0-baseline/source-10m-1080p.mp4
```

Build and validate the project:

```sh
cargo run -q -p prism-cli -- create-project \
  --project /tmp/prism-task0-baseline/small.prism \
  --name 'Task 0 Small Benchmark'
cargo run -q -p prism-cli -- script-transaction \
  --project /tmp/prism-task0-baseline/small.prism \
  --request benchmarks/task0-small-transaction.json
cargo run -q -p prism-cli -- inspect \
  --project /tmp/prism-task0-baseline/small.prism
cargo run -p prism-studio -- \
  --open-project /tmp/prism-task0-baseline/small.prism
```

Observed validation: revision 1, one sequence, three tracks, 12 clips, and a
120-second exact sequence duration. Native Studio opened the project
successfully. The headless Studio project summary took 1.86 seconds in a warm
debug build; this is not a release-build startup benchmark.

Medium and Large were not created. The repository has no representative media
corpus for those cases, and the Large definition depends on titles/effects that
are not yet implemented end to end. Scaling one synthetic source would produce
an artifact but not useful evidence.

## Playback measurements

The real Studio `PreviewWorker` measured ten paused scrubs at source positions
0, 60, ... 540 seconds. Request-to-decoded-frame samples were 92.657, 92.705,
92.776, 92.887, 94.043, 96.736, 99.452, 103.579, 104.182 and 144.823 ms:
median 96.736 ms and maximum 144.823 ms. This includes worker scheduling,
FFprobe and FFmpeg decode, but not Slint event delivery or display scanout.
Reproduce it with:

```sh
PRISM_TASK0_LONG_MEDIA=/tmp/prism-task0-baseline/source-10m-1080p.mp4 \
PRISM_FFMPEG=/opt/homebrew/bin/ffmpeg \
PRISM_FFPROBE=/opt/homebrew/bin/ffprobe \
cargo test -p prism-studio --bin prism-studio \
  task0_measures_long_media_scrub_latency -- --ignored --nocapture
```

The ten-minute FFplay SDL-dummy session advanced its steady-state audio-master
clock by 599,910 ms over 605,357 ms of monotonic wall time: -5,447 ms drift
(-0.90%, audio-master slower than realtime). Startup and shutdown were excluded
by anchoring the interval at the first clock observation and sampling at 599.9
seconds. Reproduce the real-time run with:

```sh
PRISM_TASK0_LONG_MEDIA=/tmp/prism-task0-baseline/source-10m-1080p.mp4 \
PRISM_FFMPEG=/opt/homebrew/bin/ffmpeg \
PRISM_FFPLAY=/opt/homebrew/bin/ffplay \
cargo test -p prism-studio --bin prism-studio \
  task0_measures_ten_minute_audio_master_drift -- --ignored --nocapture
```

FFprobe reports both generated source streams start at 0.000000 and last
600.000000 seconds, for an encoded-stream end-time delta of 0 ms. During
playback, `native_workspace_renders_and_edits_are_persisted` verifies that the
video playhead is set to the exact audio-master position (120 ms observed gives
an exact origin + 120/1000 playhead). Therefore Prism's scheduled video/audio
timeline drift is 0 ms; the observed master clock itself ran 5,447 ms behind
wall time. Decode/presentation delay and physical speaker/display sync are not
observable with the SDL dummy driver and remain qualification risks.

## Verification of the supplied current-state summary

Verified as stated:

- exact rational time and a typed command engine with atomic transactions;
- snapshot undo/redo capped at 50 edit-session entries;
- autosave/recovery, markers, track controls, proxy/cache foundations and
  waveform strips;
- Python, JavaScript and TypeScript supervised script runtimes with capability
  checks;
- audio mixing, CPU compositing and render-graph foundations;
- export is limited to one video track or one video plus one audio track;
- export rejects timelines whose selected clips participate in transitions;
- preview video uses FFmpeg subprocess decoding and audio uses FFplay rather
  than a realtime native mixed-audio pipeline;
- no persisted linked A/V identity, clipboard, multiselection or title domain
  object; scalar curves are the current keyframe foundation;
- `apps/studio/src/native_ui.rs` is about 8,800 lines; commands, CLI, media and script
  library files are 2,205-3,191 lines.

Corrections to the supplied summary:

- SQLite storage migrations are through v7, not v6
  (`STORAGE_SCHEMA_VERSION` in `prism-persistence`).
- Source insert and overwrite are present in Studio as atomic command batches.
  Slip, roll and slide commands and UI paths also exist. Persisted linked A/V,
  clipboard and multiselection remain absent, and transition-bearing sequences
  are explicitly rejected by insert/overwrite.

## Baseline risks

- The SDL dummy audio master ran 5.447 seconds slow over ten minutes; real-device
  behavior is unknown and must be qualified before playback can be rated ready.
- Scrub measurement excludes UI event delivery and display scanout; physical
  A/V sync remains unmeasured.
- The checked-in real-media fixture is one second at 320x180; it is useful for
  correctness but not long-form performance qualification.
