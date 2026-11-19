# Prism production roadmap

Prism is an editor foundation. Passing unit tests does not establish production
readiness or parity with Premiere Pro. This roadmap follows the interface study in
`../research/PREMIERE_INTERFACE_REFERENCE.md` and the actual engine status in
`../FEATURE_STATUS.md`.

## Current increment

- Explicit aligned A/V split/delete/ripple-delete, exact-time moves and paired source-range changes edit both uniquely matching clips atomically,
  with locked-track/ambiguity refusal and one-step undo. Pointer move and edge-trim
  gestures now preserve uniquely aligned imported A/V pairs atomically as well.
  Persistent link groups and grouped selection remain pending.

- Save As creates a separate project file, changes the active destination only after
  a successful save, preserves undo/redo, and refreshes project-scoped autosave/cache
  services. Existing destinations are rejected. Workspace layout stays visible and
  can be saved for the new copy. Media is referenced, not copied into a package.
- Timeline audio preview follows clip boundaries, exact source trims and silence
  gaps. FFplay receives a bounded remaining duration, and the transport changes the
  active process when it reaches another clip. Pause stops audio and picture.
- Audio preview still chooses one active unmuted clip. This is not a multitrack mixer.
  Per-clip player startup can introduce latency at cuts. A shared audio/video clock
  and persistent decode/device pipeline remain necessary.

## Delivery stages and acceptance evidence

1. **Reliable playback and cutting.** Persistent decoding and audio device I/O,
   common presentation clock, native sequence frame rate, linked A/V selection and
   atomic move/trim/split/ripple operations. Verify long real recordings, variable
   frame rate and multiple codecs; measure sync and dropped frames. No stranded
   playback process after pause, seeking, project switches or shutdown.
2. **Complete media/project workflow.** Named bins, rich metadata, offline-media
   management, track creation/routing, sequence settings and multiple sequences,
   portable media collection. Verify stable IDs, undo, reopen and recovery for every
   project mutation, plus missing media and destination failure handling.
3. **Matching preview and delivery.** Render all supported visible video layers and
   audible tracks, including real transitions. Validate whole-sequence output against
   preview and reject unsupported output instead of silently omitting content. Add
   configurable codecs, frame rate, size, quality and bounded export ranges.
4. **Picture and sound controls.** Persist transform, crop, opacity, gain, pan and
   effect curves; expose them only when preview and export both evaluate them.
   Add actual meters, mixing, fades and loudness tools with signal-based tests.
5. **Color, text and captions.** Connect existing color/scopes foundations to real
   decoded frames, then implement grading, titles and timed caption authoring with
   deterministic output and font/color handling.
6. **Release qualification.** Supported-platform codec packaging, install/update,
   performance budgets, memory/worker shutdown audits, accessibility and keyboard
   coverage, crash recovery and representative project testing. Publish a tested
   capability matrix and known limitations before calling a release production ready.

Each stage needs usable controls, persisted behavior, undo where applicable,
preview/export agreement, and representative media checks. Panel shells alone do
not satisfy a stage.
