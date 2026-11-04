# Milestone P0.2 persistence slice

Date: 2026-11-04. Status: Implemented and unit tested; not production ready.

## What changed

Added `prism-persistence`, a storage adapter that keeps SQLite rows and migration
concerns outside the domain model. Storage schema v1 persists the project root,
revision, media references, ordered sequences/tracks/clips, track state, typed
IDs, and exact rational time. Writes use SQLite transactions. Loads run SQLite
quick and foreign-key integrity checks, reject unsupported future schemas, parse
typed IDs and integer ranges explicitly, and restore only the current domain
schema.

`save_atomic` creates a sibling temporary database, commits and closes it, syncs
the file, renames it into place, and syncs the parent directory on Unix. Failed
writes attempt to remove only the operation-owned temporary file.

## Verification

Formatting and strict workspace Clippy pass. Twenty-two workspace unit tests
pass. Five persistence tests cover exact project round trip, atomic file save and
load, future-schema load rejection, future-schema write refusal without version
downgrade, and malformed UUID rejection.

## Known limitations

- There is only schema v1, so no historical migration can yet be exercised.
- Atomic replacement of an existing destination needs a native Windows adapter;
  the current path must not be advertised as crash-safe replacement on Windows.
- Autosave, backup rotation, recovery journals, portable packages, and media
  fingerprints are not implemented.
- SQLite integrity checks cannot prove higher-level timeline invariants; a domain
  integrity validator and corruption fixtures are still required.
- The bundled SQLite choice simplifies reproducibility but increases binary size
  and must be included in license/SBOM review.

## Follow-up integrity slice

The project model now validates map-key identity, names and paths, global track
and clip uniqueness, media references, nonnegative starts, positive durations,
chronological ordering, and non-overlap. Restoration and staged command commits
both run this validator. Generated rational-time tests exercise commutativity and
reversibility over randomized values and timebases. The workspace now has 25
passing tests.

## Next slice

Add domain-wide integrity validation and property tests, establish a real v1-to-v2
migration fixture when schema evolution is necessary, then begin the media
capability/metadata adapter and verify it against actual redistributable fixtures.
