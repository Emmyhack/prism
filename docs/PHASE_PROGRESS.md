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
  project model and command API;
- implement and test slip, slide, and rolling timeline edits with exact-time
  validation and undo records (slip and rolling edits are now implemented in
  the Rust command engine; slip, rolling, and slide protocol builders are now
  complete, while UI parity remains);
- connect background media/cache work to user-visible task state (the Studio
  summary now exposes locked/muted track and marker state);
- expose crash-recovery candidates through the Studio workflow;
- verify the complete import/edit/save/reopen/export workflow with a real media
  fixture on each supported platform.

## Phase 3 — scripting runtimes and SDKs

Current state: in development.

The shared protocol, bounded worker host, SDK builders, and transaction
validation are implemented. Runtime packaging, permissions UI, debugging, and
the Studio scripting workspace remain acceptance gaps.

## Phase 4 — advanced professional processing

Current state: planned/in development by subsystem.

The color, audio, compositing, and render foundations are functional. HDR
display transforms, scopes UI, advanced tracking/retiming, multicamera
workflows, and production codec coverage remain unimplemented.

## Phase 5 — extensibility

Current state: in development.

Versioned manifests, capability authorization, safe catalog discovery, and
policy tests are implemented. Runtime activation, Wasmtime resource limits,
plugin lifecycle/error boundaries, and extension management remain pending.
