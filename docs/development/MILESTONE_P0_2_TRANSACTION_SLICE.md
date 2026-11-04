# Milestone P0.2 transaction slice

Date: 2026-11-04. Status: Implemented and unit tested; not production ready.

## What changed

- Added comparable exact rational time across different timebases.
- Added atomic command batches with one revision per successful transaction.
- Added dry-run validation returning planned events without mutation.
- Added snapshot undo records that can be applied again as redo records.
- Added media import, clip insertion, and clip movement commands.
- Added exact trim, split, and delete commands.
- Added optimistic revision checks that reject stale transactions and stale undo
  records before either can overwrite newer work.
- Added locked-track, missing-reference, invalid-time, duplicate-ID, and
  half-open clip-overlap validation.
- Added versioned script DTOs for the current command set, exact rational time,
  expected revision, transaction labels, and dry-run intent.

These changes keep CLI, visual UI, and all future language SDKs on one mutation
path. Protocol data still converts into typed Rust commands before validation.

## Verification

Formatting and strict workspace Clippy pass. Seventeen unit tests pass, including
failed-batch rollback, undo/redo, dry-run immutability, exact-time ordering, clip
overlap rejection, split/trim/delete range preservation, stale-revision rejection,
stale-undo rejection, script batch conversion, and malformed rational-time rejection.

## Limitations

Snapshot undo is memory-expensive and will require replacement or structural
sharing before large projects. Media import currently registers a path but does
not inspect or verify the file. Clip duration assumes normal speed. Cross-track
moves, linked A/V, permissions, persistent history, and command coalescing remain
unimplemented. Source-duration validation against probed media is impossible
until the media adapter exists.

## Next slice

Implement SQLite persistence adapters and migration/crash tests, keeping storage
DTOs separate from domain objects. Then add property tests for arbitrary command
sequences and begin truthful media probing.
