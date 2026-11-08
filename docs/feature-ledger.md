# Prism Feature Ledger

This ledger records verified implementation status. It is intentionally more
conservative than a visual resemblance checklist: a control is only marked
implemented when it updates authoritative state and has a regression test.

| Area | Status | Evidence / boundary |
| --- | --- | --- |
| Project and media command engine | implemented, tested | Typed mutations, validation, atomic transactions, and snapshot undo/redo in `prism-commands`. |
| Media catalog and probing | partial | Real-path cataloging now probes with `ffprobe`, fingerprints readable files, retains per-item offline/diagnostic state, sorts deterministically, supports case-insensitive search/type filtering, exposes typed content-addressed keys for thumbnails/waveforms/preview frames, and is exposed through Studio `--list-media`; bounded asynchronous thumbnail, mono, and stereo peak/RMS waveform artifact writes plus typed cache lookup/read APIs are implemented and catalog JSON exposes those keys. Project bins, relink UI, and native browser binding remain. Explicit source relinking is now a validated undoable command. |
| Shared command descriptors | implemented, tested | Stable IDs, categories, shortcuts, enabled predicates, and undo strategy in `CommandRegistry`; UI adapter wiring remains next. |
| Exact timeline math | implemented, tested | Rational time, retiming, multicam planning, trim/ripple/roll/slip/slide command coverage. |
| Persistence and recovery | implemented, tested | SQLite schema migrations, atomic saves, backups, autosaves, integrity checks, and recovery candidate discovery. |
| Native Studio shell | implemented, tested | Slint desktop shell with responsive browser/monitor/inspector/timeline regions and native launch. |
| Studio command wiring | partial | Workspace, independent Source/Program transport state, frame-step command descriptors, tool, monitor focus, script review/run, and export-status controls have local state; authoritative command dispatch and undo history are not yet connected to every control. |
| Source/Program playback | partial | Exact timeline evaluation now feeds a bounded `PlaybackEngine`; a typed `ReferenceVideoPlayback` adapter connects inspected media sources to the existing off-thread FFmpeg RGBA decoder with seek generations and stale-result suppression. Native decoder surfaces, audio sync, frame stepping, and monitor presentation remain. |
| Color and scopes | partial | CPU color/HDR/scope foundations and COLOR workspace presentation affordances are implemented; backend-fed overlays and GPU parity remain. |
| Audio | partial | Offline BS.1770/EBU R128 LUFS foundation and stateful stereo peak/RMS meter with peak-hold/clipping reset are tested; realtime mixer, devices, and sync remain. |
| Export | partial | Typed capabilities, runtime-name discovery, deterministic FFmpeg argument mapping, and a validated queued-job lifecycle (progress, cancellation, terminal failure) are tested; request-level policy selection, worker integration, and packaged encoder execution remain. |
| Scripting | implemented, tested | Permissioned Python/JavaScript/TypeScript protocol, runtime manifests, debugger, SDK parity, and package tests. |

## Audit boundary

The current repository has a strong Rust domain foundation but does not yet
contain a dock-tree manager, media decoder surface, realtime playback engine,
or a UI-to-command bridge for every visible control. Those are implementation
items, not claims of Premiere parity. Future slices must move rows from
`partial`/`deferred` only after model, UI, persistence/export consequences, and
tests are present.
