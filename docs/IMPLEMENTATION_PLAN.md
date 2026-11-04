# Implementation plan

## Milestone P0.1 — foundation (in development)

Acceptance criteria:

- workspace builds on pinned stable Rust;
- exact time unit and property tests pass;
- typed commands reject invalid state without mutation;
- scripting DTOs cannot bypass command validation;
- all twelve required architecture decisions exist with risks;
- CI runs format, lint, and tests on macOS, Windows, and Linux.

## Milestone P0.2 — project transactions and persistence

Implement typed asset/sequence/track/clip commands, staged multi-command
transactions, inverse-operation undo records, SQLite persistence adapters,
schema migrations, atomic save, and corruption fixtures. Begin Python/JS/TS SDK
schema generation from one protocol description.

## Milestone P1.1 — truthful media ingest

Add a dynamically linked FFmpeg adapter and capability report, then inspect real
fixtures for duration, streams, timebase, codec, pixel/color metadata, channel
layout, rotation, and VFR evidence. Unsupported capabilities remain explicit.

## Milestone P1.2 — smallest editing workflow

Import one real asset, create sequence/tracks, insert/move/trim/split clips,
evaluate frames and audio asynchronously, preview, save/reopen, undo/redo, and
export an MP4 through a temporary file with validation and atomic finalization.

The milestone is complete only when the exported file is independently probed
and its decoded frame/audio timing matches expectations.
