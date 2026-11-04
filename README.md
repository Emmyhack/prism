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

Inspect a saved project to discover stable object IDs and exact clip timing:

```sh
cargo run -p prism-cli -- inspect --project ./edit.prism
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
  --ffmpeg /path/to/ffmpeg --ffprobe /path/to/ffprobe
```

This path currently emits video only. It does not yet provide audio mixing,
effects, progress, cancellation, overwrite, or a production codec bundle.

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
