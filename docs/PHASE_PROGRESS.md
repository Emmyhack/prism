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
  worker exposes bounded pending-write state; Studio now projects authoritative
  export queue progress, failures, destinations, and valid lifecycle actions,
  and its project summary exposes locked/muted track and marker state. The
  native command bar renders export progress, failures, and state-valid
  cancel/retry/clear controls; cache-task presentation remains pending);
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

Current state: implemented and tested.

The shared protocol, bounded worker host, SDK builders, transaction validation,
Studio script catalog/diagnostics boundary, normalized permission review
boundary, explicit TypeScript source-map compilation, and authorized
Python/JavaScript/TypeScript recipe execution are implemented. Versioned
contained runtime bundles, persisted permission decisions, a combined Studio
workspace model, and a non-mutating step/breakpoint transaction debugger now
close the corresponding host boundaries. The native Studio workspace exposes
the script catalog, runtime health, permission review, diagnostics, debugger,
and run controls. Python, JavaScript, and TypeScript SDK packaging metadata and
parity tests are complete. Phase 3 has no remaining source acceptance gaps;
signed distribution artifacts remain part of the cross-platform release gate.

## Phase 4 — advanced professional processing

Current state: in development.

The color, audio, compositing, and render foundations are functional. D65
Rec.709, Display P3, and Rec.2020 linear-light primaries conversion plus
normalized PQ and HLG transfer functions are implemented and tested. Offline
48 kHz stereo integrated-LUFS measurement now applies BS.1770 K-weighting and
EBU R128 absolute/relative gates. Validated static HDR luminance metadata now
drives a tested PQ Rec.2020 to SDR Rec.709 display transform with smooth
highlight roll-off. Scope accumulation now has normalized backend-neutral
waveform/vectorscope presentation maps. Metadata persistence/codec propagation,
graphical scope overlays, and runtime process wiring/packaged encoder coverage
remain incomplete. Export now has a tested typed container/codec/profile policy
against explicit capabilities, plus runtime-name discovery for known muxers and
encoder families.
Playback now has a tested coordinator that joins exact timeline evaluation to
bounded asynchronous decode scheduling, with seek-generation invalidation and
stale-result suppression. Native decoder surfaces, audio sync, and monitor
presentation remain. Tracking now has a tested bounded integer-pixel CPU point-match
primitive with forward/backward consistency reporting; pyramidal, subpixel,
planar, mask, persistence, and graphical tracking workflows remain.
Exact piecewise retiming supports variable speed, freeze, reverse, and
fractional-frame lookup without drift. Multicamera planning validates exact
angle offsets and deterministic switch boundaries; timeline command,
persistence, playback, and UI integration remain.
CPU compositing now also includes validated normalized coverage masks, alpha
and luma matte extraction, deterministic mask combination, and atomic
straight-alpha application. Shape rasterization, feathering, tracked-mask
binding, render-graph integration, and GPU parity remain.
Waveform and vectorscope accumulators now expose backend-neutral normalized
presentation maps with peak-count metadata, coordinate access, and explicit
linear or logarithmic density scaling. Graticules, legal-range overlays,
color-space labels, and graphical scope rendering remain.

## Phase 5 — extensibility

Current state: in development.

Versioned manifests, capability authorization, safe catalog discovery, and
policy tests are implemented. Runtime activation, Wasmtime resource limits,
plugin lifecycle/error boundaries, and extension management remain pending.
