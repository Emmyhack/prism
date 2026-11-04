# Architecture

Status: Phase 0 baseline, 2026-11-03.

## Dependency direction

```text
Studio UI / CLI / SDK workers
              |
        prism-script DTOs
              |
       prism-commands  ---> events / undo records
              |
        prism-project
          /         \
 prism-core     prism-timeline

media decode -> timeline evaluator -> render graph -> preview/export sinks
```

The authoritative project state lives in `prism-project`. UI, CLI, scripts,
recipes, plugins, and future agents issue the same `prism-commands` requests.
Only the command engine may commit project mutations. Protocol and persistence
DTOs convert explicitly at boundaries and cannot be used as domain objects.

## Initial crate boundaries

| Crate | Responsibility | Must not depend on |
|---|---|---|
| `prism-core` | typed IDs and stable primitives | UI, media, serialization |
| `prism-timeline` | exact time and timeline algorithms | UI, codecs |
| `prism-project` | authoritative aggregate and invariants | UI, persistence formats |
| `prism-persistence` | versioned SQLite schema and domain mapping | UI, language runtimes |
| `prism-media` | capability discovery and typed media metadata | UI, project mutation |
| `prism-commands` | validation, transactions, events, undo | UI, language runtimes |
| `prism-script` | versioned protocol DTOs and conversions | runtime-specific APIs |
| `prism-testing` | reusable fixtures and assertions | production applications |

Future crates are introduced when their first real behavior exists, not merely
to mirror a proposed directory tree.

## Project schema proposal

Projects contain assets, bins, sequences, ordered typed tracks, clips referencing
assets, transitions, effects, keyframes, captions, buses, color state, markers,
and export configurations. Every object receives a typed UUID. Domain schema,
persistence container, and scripting API versions are independent.

The initial `.prism` file is a versioned SQLite database for structured project
state. Domain objects do not derive storage serialization; the persistence crate
maps rows into validated restoration state. A future package may add a manifest,
small generated assets, and recovery metadata.
Original media remains external by default. Large proxies and caches remain in a
separately managed cache. This proposal requires crash and migration tests before
acceptance.

## Time semantics

Authoritative time is a normalized rational `value/timescale` in seconds. Float
seconds are presentation-only. Operations are checked. Conversions that cannot
be exact must name a rounding mode. Source, sequence, sample, and display
timecode are distinct types in the next timeline milestone. NTSC rates use exact
fractions such as 30000/1001. Drop-frame is formatting, not a different rate.

## Command transactions

Commands validate references, permissions, locks, ranges, and invariants before
commit. A transaction stages a sequence of commands against an isolated working
state, then publishes state, undo record, and events together. Events are never
emitted for rejected work. The foundation uses complete project snapshots for
correct undo/redo and rollback semantics. This is intentionally simple but not
acceptable for large projects; later milestones will move to persistent state or
compact inverse records after benchmarked equivalence tests.
