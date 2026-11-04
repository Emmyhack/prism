# Prism

Prism is a professional, cross-platform, programmable nonlinear video editor
implemented in Rust. The project is in **Phase 1: functional editing foundation**.
It has tested core editing, persistence, reference decoding, and constrained
single-video-track MP4 export paths, but it is not yet a usable desktop editor.

## Current foundation

- exact rational timeline time with checked arithmetic;
- typed project-object identifiers and a UI-independent domain model;
- a typed command boundary used by all future interfaces;
- a versioned, language-neutral scripting protocol boundary;
- atomic SQLite project save/reopen;
- exact timeline evaluation with bounded background decode scheduling;
- validated reference MP4 export for one video track with explicit black gaps;
- a non-interactive saved-project track export command;
- architecture, security, testing, and milestone documentation.

## Headless track export

Create an empty validated project without overwriting an existing file:

```sh
cargo run -p prism-cli -- create-project \
  --project ./edit.prism --name "My edit"
```

Inspect real media and create the first video sequence through one validated
command transaction:

```sh
cargo run -p prism-cli -- create-video-sequence \
  --project ./edit.prism --media ./source.mp4 --sequence-name Main \
  --source-start 0/1 --duration 10/1 --ffprobe /path/to/ffprobe
```

Inspect a saved project to discover stable object IDs and exact clip timing:

```sh
cargo run -p prism-cli -- inspect --project ./edit.prism
```

Append another trimmed source range to an existing video track:

```sh
cargo run -p prism-cli -- add-video-clip \
  --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --media ./source.mp4 \
  --source-start 10/1 --timeline-start 10/1 --duration 5/1 \
  --ffprobe /path/to/ffprobe
```

Move an existing video clip without changing its source range:

```sh
cargo run -p prism-cli -- move-video-clip \
  --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --clip 33333333-3333-4333-8333-333333333333 \
  --timeline-start 15/1
```

Trim and reposition an existing video clip:

```sh
cargo run -p prism-cli -- trim-video-clip \
  --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --clip 33333333-3333-4333-8333-333333333333 \
  --timeline-start 15/1 --source-start 2/1 --duration 5/1
```

Split an existing clip at an exact timeline position:

```sh
cargo run -p prism-cli -- split-video-clip \
  --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --clip 33333333-3333-4333-8333-333333333333 \
  --timeline-position 13/16
```

Delete an existing video clip by stable IDs:

```sh
cargo run -p prism-cli -- delete-video-clip \
  --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --clip 33333333-3333-4333-8333-333333333333
```

Validate a language-neutral script transaction without changing the project:

```sh
cargo run -p prism-cli -- script-transaction \
  --project ./edit.prism --request ./transaction.json
```

Set `dryRun` to `false` in the versioned JSON request to commit through the
same validated command engine.

See [the example transaction](examples/script-transaction-dry-run.json) for
the complete wire shape used by future Python, JavaScript, and TypeScript SDKs.

Run a trusted Python authoring script through the supervised host. The script
must print one JSON transaction envelope to stdout; its mutations still pass
through the same protocol validation, revision checks, and command engine:

```sh
cargo run -p prism-cli -- run-python-script \
  --project ./edit.prism --script ./scripts/organize.py --python python3 \
  --permissions ./scripts/organize.permissions.json
```

The host injects `PRISM_PROJECT_REVISION`, bounds wall-clock execution and
stdout/stderr size, and terminates cancelled or timed-out workers. This is a
trusted-process boundary, not an OS-level sandbox; JavaScript and TypeScript
runtime adapters remain separate milestones.

JavaScript files can use the equivalent Node boundary and produce the same
transaction envelope:

```sh
cargo run -p prism-cli -- run-javascript-script \
  --project ./edit.prism --script ./scripts/organize.mjs --node node \
  --permissions ./scripts/organize.permissions.json
```

TypeScript uses an explicit compiler followed by the same Node worker. Compiler
output is isolated in a temporary directory and removed after execution:

```sh
cargo run -p prism-cli -- run-typescript-script \
  --project ./edit.prism --script ./scripts/organize.ts \
  --tsc /path/to/tsc --node node \
  --permissions ./scripts/organize.permissions.json
```

The Studio binary currently provides a real, UI-independent shell. Start it
without a project or inspect a validated project through the persistence layer:

```sh
cargo run -p prism-studio
cargo run -p prism-studio -- --project ./edit.prism
```

Lock or unlock a track through the same validated command engine:

```sh
cargo run -p prism-cli -- set-track-locked \
  --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --locked true
```

Track lock state is persisted in the project and prevents future timeline
mutations that target the locked track.

Reorder tracks by stable ID through the same command path:

```sh
cargo run -p prism-cli -- reorder-track \
  --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --index 0
```

Track visibility and mute state are also persisted through validated commands:

```sh
cargo run -p prism-cli -- set-track-visible --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 --visible false
cargo run -p prism-cli -- set-track-muted --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 --muted true
```

Add and remove exact-time sequence markers through the same command engine:

```sh
cargo run -p prism-cli -- add-marker --project ./edit.prism \
  --sequence 11111111-1111-4111-8111-111111111111 \
  --marker 33333333-3333-4333-8333-333333333333 \
  --time 3/2 --label "Beat"
cargo run -p prism-cli -- delete-marker --project ./edit.prism \
  --sequence 11111111-1111-4111-8111-111111111111 \
  --marker 33333333-3333-4333-8333-333333333333
```

Duplicate a video clip at an exact timeline position without altering source media:

```sh
cargo run -p prism-cli -- duplicate-video-clip --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --clip 44444444-4444-4444-8444-444444444444 \
  --new-clip 55555555-5555-4555-8555-555555555555 \
  --timeline-start 12/1
```

Ripple-delete a clip and close the gap on its track:

```sh
cargo run -p prism-cli -- ripple-delete-video-clip --project ./edit.prism \
  --track 22222222-2222-4222-8222-222222222222 \
  --clip 44444444-4444-4444-8444-444444444444
```

Audio clips use the same exact-time command path and require an inspected audio
stream before the transaction is committed:

```sh
cargo run -p prism-cli -- add-audio-clip --project ./edit.prism \
  --track 77777777-7777-4777-8777-777777777777 \
  --media ./interview.wav --source-start 0/1 \
  --timeline-start 0/1 --duration 30/1 --ffprobe /path/to/ffprobe
```

Transitions are persisted as validated sequence objects. The current CLI
supports exact-duration dissolves between adjacent video clips:

```sh
cargo run -p prism-cli -- add-transition --project ./edit.prism \
  --sequence 11111111-1111-4111-8111-111111111111 \
  --transition 66666666-6666-4666-8666-666666666666 \
  --outgoing-clip 44444444-4444-4444-8444-444444444444 \
  --incoming-clip 55555555-5555-4555-8555-555555555555 \
  --duration 1/2 --kind dissolve
```

Validate a recipe manifest and its inputs without starting a script worker:

```sh
cargo run -p prism-cli -- validate-recipe \
  --manifest ./recipe.json --inputs ./recipe-inputs.json
```

Recipe validation checks the declared language, capabilities, parameter types,
defaults, and numeric bounds before an execution plan can be created.

Generate a validated proxy through an explicitly selected FFmpeg executable. The
source is never modified and the destination must not already exist:

```sh
cargo run -p prism-cli -- generate-proxy \
  --source ./camera.mp4 --output ./cache/camera-proxy.mp4 \
  --width 1280 --height 720 --video-codec libx264 \
  --audio-codec aac --ffmpeg /path/to/ffmpeg
```

The command uses the same cancellable proxy generator and profile validation as
the media library; packaging, background scheduling, and automatic proxy
management remain separate milestones.

Create a no-clobber recovery backup of a saved project:

```sh
cargo run -p prism-cli -- backup-project \
  --project ./edit.prism --output ./backups/edit-revision-1.prism
```

Create the deterministic autosave for the current project revision:

```sh
cargo run -p prism-cli -- autosave-project --project ./edit.prism
```

List revision-scoped autosaves and recover a selected candidate into a new
project without replacing an existing destination:

```sh
cargo run -p prism-cli -- list-autosaves --project ./edit.prism
cargo run -p prism-cli -- recover-autosave \
  --source ./edit.prism.autosave.7 --output ./recovered.prism
```

The command emits deterministic, versioned JSON. Use the reported sequence and
track IDs with the export command:

The current CLI exports one video track from a validated saved project. IDs are
the stable UUIDs stored in the project. Encoder and probe paths are explicit
because the distributable codec configuration is not yet approved.

```sh
cargo run -p prism-cli -- export-track \
  --project ./edit.prism \
  --sequence 11111111-1111-4111-8111-111111111111 \
  --track 22222222-2222-4222-8222-222222222222 \
  --output ./finish.mp4 \
  --width 1920 --height 1080 --fps 30000/1001 \
  --video-bitrate-kbps 8000 \
  --ffmpeg /path/to/ffmpeg --ffprobe /path/to/ffprobe
```

The synchronized `export-video-audio` command accepts the same video bitrate
option plus `--audio-bitrate-kbps` for explicit AAC bitrate control.

This path currently emits video only. It does not yet provide audio mixing,
effects, progress, cancellation, overwrite, or a production codec bundle.
Audio-track timing plans are validated separately (including trims and explicit
silence gaps). The export library also provides a synchronized single-video /
single-audio AAC muxing path with duration validation; multitrack mixing,
effects, progress, cancellation, overwrite, and a production codec bundle
remain later milestones. The CLI command above intentionally remains the
video-only compatibility path until its audio options are finalized.

For a synchronized single video track and audio track, use the combined CLI
command (both tracks must have the same validated timeline duration):

```sh
cargo run -p prism-cli -- export-video-audio \
  --project ./edit.prism \
  --sequence 11111111-1111-4111-8111-111111111111 \
  --video-track 22222222-2222-4222-8222-222222222222 \
  --audio-track 33333333-3333-4333-8333-333333333333 \
  --output ./finish-with-audio.mp4 \
  --width 1920 --height 1080 --fps 30000/1001 \
  --ffmpeg /path/to/ffmpeg --ffprobe /path/to/ffprobe
```

## Prerequisites

Install Rust 1.94.1 with rustfmt and Clippy. Media development will additionally
require an approved FFmpeg build; its distribution configuration is not yet
locked because codec patent and license review remains open.

```sh
rustup toolchain install 1.94.1 --profile minimal --component rustfmt,clippy
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

See [the implementation plan](docs/IMPLEMENTATION_PLAN.md) and
[feature status](docs/FEATURE_STATUS.md). No current feature is production ready.
Sequence timeline markers are supported by the core command API, SQLite project format, headless inspection, and the Python, JavaScript, and TypeScript protocol SDKs.
Validated proxy profiles, content-addressed proxy cache identities, and an
explicit cancellable FFmpeg proxy generator are now available. The cache also
has a bounded background write worker; eviction policy integration and
distributable codec packaging remain separate milestones.
