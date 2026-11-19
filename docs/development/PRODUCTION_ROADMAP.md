# Prism production roadmap

Prism is an editor foundation. Passing unit tests does not establish production
readiness or parity with Premiere Pro. This roadmap follows the interface study in
`../research/PREMIERE_INTERFACE_REFERENCE.md` and the actual engine status in
`../FEATURE_STATUS.md`.

## Current increment

- Waveform jobs now use supervised audio decoding and cooperative fingerprint
  cancellation. Project resets advance the worker cancellation generation, runtime
  teardown cancels its active request, and stale results are suppressed; obsolete
  queued jobs cannot publish into the new project. A controlled sleeping decoder
  test verifies process cleanup and that queued work is not restarted.
  Fingerprint/decode stages use 30-second deadlines; already-submitted
  content-addressed cache writes may finish safely in the old cache.

- Metadata probes and reference RGBA still decoding now have bounded output capture,
  deadlines, and per-request cancellation. Preview supersession/project changes and
  worker teardown cancel the old tool operation instead of only ignoring its result.
  The supervisor kills/reaps the direct child on cancellation, timeout or output
  overflow; default APIs use 30 seconds and Studio preview stages use five seconds.
  Controlled-process tests verify cleanup. Broader tool-path cancellation, process-tree
  behavior and blocked-I/O/platform qualification remain open.

- Reference mono-audio decoding and packet-timing inspection now use the same bounded
  supervisor, with 30-second compatibility defaults plus cancellation-aware APIs.
  Audio output is capped to the requested sample bytes, packet JSON to 32 MiB and
  diagnostics to 64 KiB. Studio waveform jobs use the cancellation-aware path.

- Playback audit fixes: repeated/nonadvancing audio-clock reports no longer hide a
  stall, and a missing Program decode result stops the transport after five seconds.
  Completed replies are drained before checking the deadline. Controlled-process
  tests verify that the video deadline stops both audio processes. Stage 1 remains
  open; see [playback acceptance evidence](PLAYBACK_ACCEPTANCE.md) for verified
  behavior, limits and the remaining device/long-media/tool-path qualification work.

- Audio-backed transport now uses the active FFplay session's reported audio position
  to drive the playhead, timecode and video requests. Startup holds until a clock
  report arrives; a missing startup clock or stalled clock progress stops playback.
  Each session owns its observations so old reader threads cannot drive a restarted
  session. Status parsing is bounded and monotonic, with real dummy-device tests.
  These are coarse backend observations, not sample-accurate native device timing;
  hardware latency, drift, long-recording sync and backend-version compatibility
  still require qualification. Video-only transport retains a wall clock.

- Stage 1 now reuses a continuous FFmpeg video decoder for forward Program playback,
  with at most 960×540 RGBA preview frames and bounded catch-up. Source changes,
  backward seeks and larger jumps reopen the decoder. Pause/scrub/project changes and
  shutdown kill its process; paused pictures stay visible while the exact still frame
  loads. Tests verify changing real frames from the same process and process cleanup.
  Preview output remains 30 fps; native sequence timing, sample-accurate device
  timing, native audio-device integration and measured long-media sync are pending.

- Explicit aligned A/V split/delete/ripple-delete, exact-time moves and paired source-range changes edit both uniquely matching clips atomically,
  with locked-track/ambiguity refusal and one-step undo. Pointer move and edge-trim
  gestures now preserve uniquely aligned imported A/V pairs atomically as well, and
  timeline selection highlights both matching clips. Primary split/delete/ripple-delete
  shortcuts also resolve unique pairs automatically. Persistent link-group identity for
  intentionally linked or unlinked clips remains pending.

- Save As creates a separate project file, changes the active destination only after
  a successful save, preserves undo/redo, and refreshes project-scoped autosave/cache
  services. Existing destinations are rejected. Workspace layout stays visible and
  can be saved for the new copy. Media is referenced, not copied into a package.
- Stage 1 audio preview now uses one FFmpeg decoder piped to one FFplay device
  process for the remaining active audio track. The session covers trims, cuts,
  silence gaps and trailing sequence silence without restarting at each clip.
  A signal-based fixture verifies exact sample count, audible segments and silence.
  Pause, seek, shutdown and edits/history changes to future audio stop both processes.
- This session is limited to one unmuted audio track and 128 remaining segments;
  extra tracks or larger sessions produce an explicit error. Full mixing, a native
  audio-device engine and sample-accurate shared device timing remain pending. PCM
  normalization is 48 kHz stereo; per-segment codec tail padding preserves duration.

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
