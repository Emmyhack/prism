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
| `prism-persistence` | versioned SQLite schema and domain mapping, atomic saves/backups, autosave scheduling, retention, and candidate discovery | UI, language runtimes |
| `prism-media` | capability discovery, metadata, decode reference vectors, deterministic thumbnails, and proxy profile/cache identity | UI, project mutation |
| `prism-playback` | exact sequence-time evaluation into ordered source requests | UI, codecs, project mutation |
| `prism-export` | validated encoding, typed geometry/frame-rate presets, temporary outputs, and atomic finalization | UI, bitrate/codec policy, project mutation |
| `prism-audio` | allocation-free routed track/bus mixing, fades/crossfades, peak/RMS/dBFS analysis, normalization, dynamics compression, limiting, stateful one-pole filtering and stereo delay, block resampling, and cache-ready waveform primitives | UI, device ownership, project mutation |
| `prism-cache` | content-addressed derived-artifact keys, atomic writes, finalized lookup, bounded eviction, and bounded background write/eviction scheduling | UI, project mutation |
| `prism-color` | validated SDR/HDR transfer and display-transform primitives, CPU grading/LUTs, exact histogram/waveform/vectorscope accumulation, and backend-neutral normalized scope-presentation maps | UI, GPU handles, graphical scope overlays |
| `prism-compositor` | backend-independent linear RGBA blend/transition equations plus validated coverage-mask, matte extraction, combination, and straight-alpha application primitives | UI, GPU handles, project mutation, mask shape authoring |
| `prism-effects` | exact-time scalar automation curves and deterministic bounded CPU point tracking with forward/backward consistency | UI, persistence formats, tracking workflow ownership |
| `prism-render` | deterministic render-graph dependency planning and explicit CPU pixel execution for source/blend/transition/output plus parameterized color gain, opacity, and blend modes | UI, GPU handles, codec I/O |
| `prism-commands` | validation, transactions, events, undo | UI, language runtimes |
| `prism-script` | versioned protocol DTOs, bounded/cancellable command execution, supervised external worker process boundary, and capability authorization | runtime-specific APIs |
| `prism-plugin` | versioned plugin manifests, capability validation, and safe deterministic local catalog discovery | runtime loading, UI |
| `prism-recipe` | versioned recipe manifests and input validation | script execution, UI |
| `prism-testing` | reusable fixtures and assertions | production applications |

`apps/studio` currently provides a UI-independent application shell. It owns
startup argument parsing and project-open summaries while depending only on the
persistence boundary; a future graphical frontend can consume the same shell
contracts without moving domain logic into toolkit callbacks.

Future crates are introduced when their first real behavior exists, not merely
to mirror a proposed directory tree.

`prism-timeline` owns exact rational time, explicit rounding policies, checked
arithmetic, and nearest-grid snapping. Snapping ties resolve away from zero;
UI and codec layers cannot introduce floating-point timeline authority.

## Project schema proposal

Projects contain assets, bins, sequences, ordered typed tracks, clips referencing
assets, transitions, effects, keyframes, captions, buses, color state, markers,
and export configurations. Every object receives a typed UUID. Domain schema,
persistence container, and scripting API versions are independent.

Persistence exposes separate atomic active-save and no-clobber recovery-backup
paths. Backup artifacts are versioned by callers and never replace the active
project or an existing backup.

The persistence crate also provides a bounded background autosave scheduler and
a deterministic retention helper. The scheduler clones validated project
snapshots, skips revisions already written by its worker, and reports
completion; retention removes only matching revision artifacts and preserves
the newest requested count. Neither owns domain mutation or media bytes.

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
