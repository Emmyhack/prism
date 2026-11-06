# Phase progress gates

This document keeps milestone claims honest. A phase is not complete merely
because part of its engine exists; the listed acceptance gaps must be closed
and covered by tests before the status changes.

## Phase 2 — professional editing foundation

Current state: in development.

Implemented and tested foundations include transitions, scalar keyframes,
audio mixing primitives, proxy generation, thumbnails, waveform generation,
background cache workers, export presets, and crash-safe persistence helpers.

Remaining acceptance work:

- expose proxy assignment, switching, invalidation, and relinking in the
  project model and command API (proxy paths now persist through schema v5,
  `SetMediaProxy` and all three SDKs can set/clear validated relinks, and
  source/proxy freshness is available through the media boundary; automatic
  generation and graphical switching remain);
- implement and test slip, slide, and rolling timeline edits with exact-time
  validation and undo records (slip and rolling edits are now implemented in
  the Rust command engine; slip, rolling, and slide protocol builders are now
  complete, while UI parity remains);
- connect background media/cache work to user-visible task state (the cache
  worker exposes bounded pending-write state and the Studio summary exposes
  locked/muted track and marker state; graphical task presentation remains
  pending);
- expose crash-recovery candidates through the Studio workflow (implemented
  through `--list-autosaves` and the stable `AutosaveSummary` boundary);
- provide a stable default keyboard map for selection, razor, trim, slip,
  slide, rolling, playback, undo, and redo (`--list-shortcuts` exposes the
  map for UI adapters; deterministic event resolution is implemented, while
  graphical event wiring remains pending);
- verify the complete import/edit/save/reopen/export workflow with a real media
  fixture on each supported platform (the complete fixture workflow now passes
  on the development macOS host; Windows and Linux verification remain).

## Phase 3 — scripting runtimes and SDKs

Current state: in development.

The shared protocol, bounded worker host, SDK builders, transaction validation,
Studio script catalog/diagnostics boundary, normalized permission review
boundary, and explicit TypeScript source-map compilation are implemented.
Runtime packaging, graphical permissions UI, interactive debugging, and the
graphical Studio scripting workspace remain acceptance gaps.

## Phase 4 — advanced professional processing

Current state: in development.

The color, audio, compositing, and render foundations are functional. D65
Rec.709, Display P3, and Rec.2020 linear-light primaries conversion plus
normalized PQ and HLG transfer functions are implemented and tested. HDR
display transforms, scopes UI, advanced
tracking/retiming, multicamera workflows, and production codec coverage remain
unimplemented.

## Phase 5 — extensibility

Current state: in development.

Versioned manifests, capability authorization, safe catalog discovery, and
policy tests are implemented. Runtime activation, Wasmtime resource limits,
plugin lifecycle/error boundaries, and extension management remain pending.
