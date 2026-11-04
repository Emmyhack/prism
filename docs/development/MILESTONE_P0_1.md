# Milestone P0.1 continuity record

Date: 2026-11-03. Status: In development.

## What changed and why

Initialized a Rust 2024 workspace with domain, timeline, project, command,
scripting-protocol, testing, CLI, and Studio boundaries. Added normalized exact
time, typed UUIDs, initial sequence/track commands, and an API-versioned protocol
DTO so scripting shapes the engine from its first milestone. Added twelve ADRs,
security/testing/performance plans, CI, roadmap, feature status, and risk register.

## Verification

`cargo fmt --all --check`, strict workspace Clippy, and all workspace tests are
the required commands. Seven tests currently exercise time, command rejection
and creation, and protocol version validation.

## Known limitations

The command prototype does not yet stage multi-command rollback or implement
undo. Revision exhaustion is theoretically fallible after mutation and must be
eliminated when staged commit lands. Project persistence, media, rendering,
audio, export, runtime workers, and functional UI do not exist. FFmpeg,
ffprobe, and pkg-config are absent on the host. UI selection is provisional.

## Next milestone

Implement a staged command transaction with create/import/insert/move/trim/split
operations, exact interval invariants, inverse undo records, and language-neutral
batch/dry-run protocol messages. Then add persistence adapters and migration tests.
