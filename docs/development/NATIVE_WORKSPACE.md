# Native editing workspace

Launch with `cargo run -p prism-studio`, or pass `-- --open-project ./edit.prism`.
FFmpeg and FFprobe must be on PATH for video import, previews, and export.

The interface interprets the supplied Premiere Pro references through four workspaces:
Home for project entry, Import for footage selection, Edit for paired monitors and
an editable timeline, and Export for destination settings and job feedback. Prism
retains its supplied ribbon logo, with graphite surfaces and violet selection cues.
The desktop minimum is 1100 × 720; drag the divider below the monitors to resize.
Editing and Assembly presets change the source-monitor/media-bin allocation.
Below 1250px, Source moves into the inspector and Program takes the available
monitor width. The right rail opens Properties, History or Jobs; Inspector collapses
that region, and Reset layout restores the session defaults.

## Supported interactions

- Create/open a local project; save with the header button or Ctrl/Cmd+S.
- When revision-scoped autosaves exist beside the current project, Home shows the
  newest discoverable candidate and its revision. Recovery validates it and writes to a new
  user-selected project path and never replaces the active project or autosave.
- Every successful edit, undo, and redo queues a validated snapshot on a bounded
  background autosave worker. Bursts coalesce to the newest pending revision, the Jobs
  panel reports status, and retention keeps the five newest project autosaves.
- Import one or more videos with native file dialogs (Ctrl/Cmd+I). Imports append
  clips to the first unlocked video track, creating a sequence/track if required.
  New projects prompt for a save destination. Cancellation leaves the project intact.
- Browse real background-generated thumbnails in Import and the project media bin.
  Filter filenames without changing source identities. Failed previews retain an
  explicit unavailable state. Jobs reports authoritative queued/running/completed and
  unavailable counts for the decoder queue. The Media/Info tabs separate assets from
  project details.
- Assign or clear a proxy for selected media from Properties, and switch monitor
  decoding between the original and persisted proxy without changing media identity.
- Relink Source repairs moved/offline media without changing its stable project identity.
  Replacements must contain video and cover every source range already used by clips;
  successful relinks persist atomically, refresh previews, re-evaluate proxies, autosave,
  and support undo/redo.
- Generate a cancellable 960×540 H.264/AAC proxy in the background. Successful
  no-clobber completion persists and activates the proxy; failure leaves the original
  media assignment unchanged. Assigned proxies are classified as available, missing,
  or stale from source/proxy modification times. Missing and stale proxies can be
  regenerated to a new versioned sibling without deleting the prior assignment.
  Proxy generation state is also visible in Jobs beside media preparation and export.
- Select footage to inspect it independently of the sequence playhead. Scrub the
  Source bar, mark In/Out, and append the marked range to an unlocked video track.
  Mark Out includes the current 30 fps frame, clamped to the source duration.
  Compact windows expose these same controls inside Properties.
- Click/drag the timeline or a clip to scrub. Left/right arrows and monitor buttons
  step at a fixed 30 fps preview/edit grid. Time labels use non-drop 30 fps display;
  source timestamps and stored edits remain rational. The export preset retains its
  independent 30000/1001 fps setting.
- Use one-frame Slip, Roll In/Out, and Slide controls in Properties for exact
  three-point and boundary edits. Invalid edge operations report the missing neighbor;
  successful edits use the same atomic save and undo history as other timeline edits.
- The T/Y/U/R timeline tools also perform trim, slip, slide, and rolling edits by
  dragging a clip; T and R choose the nearest edge from the drag start. Gestures honor
  frame snapping, use finer 1/300-second resolution when snapping is off, and remain
  command-validated and undoable.
- The native keyboard map is active in Edit: V/C/T/Y/U/R select editing tools,
  Space plays, arrows step frames, comma inserts the marked source range, Backspace
  ripple-deletes, S toggles snapping, M adds a marker, and the platform primary
  modifier drives undo/redo. The timeline header shows the active tool and snap state.
- Sequence markers render as amber flags in the ruler. Clicking a flag selects it and
  seeks the playhead; ×M deletes the selected marker through the same persisted,
  undoable command path.
- Select a video clip with an adjacent clip after it to add a dissolve at the cut.
  Studio chooses a validated duration up to 15 frames, renders the transition over the
  edit boundary, and uses the same control to remove it. Add/remove are persisted,
  autosaved, and undoable.
- Duplicate to track end copies the selected clip's exact media identity and source
  range after the last clip on its current track. The placement is overlap-safe,
  persisted, autosaved, and undoable.
- Track ↑/↓ in Properties reorders the selected clip's track while preserving clip
  selection by stable identity. Boundary actions disable automatically; successful
  reorders persist, autosave, and support undo/redo.
- Play/Pause (or Spacebar) advances the sequence clock and requests current video
  frames through the bounded preview worker. Newer requests supersede queued stale
  frames. When an unmuted audio clip is active, Prism starts `ffplay` at the matching
  source offset and terminates it on pause or seek.
- Select a clip and split at the playhead (Ctrl/Cmd+K), or delete (Delete key).
  The validated command engine rejects splits at clip edges and edits to locked tracks.
- Use Properties to change sequence start, source In, and duration in exact seconds.
  Values accept decimals (up to six decimal places) or rational fractions. Invalid,
  overlapping, locked-track, and out-of-source edits are rejected before persistence.
- Move and trim a selected clip one frame at a time from Properties. These operations
  use the same validated command engine, atomic save, and undo/redo history as typed
  timing edits.
- Drag a clip body to move it, or drag its left/right edge handles to trim. Pointer
  distances are quantized to the 30 fps edit grid before the validated command is
  committed; rejected overlaps or source ranges leave the project unchanged.
- Undo with Ctrl/Cmd+Z, redo with Ctrl/Cmd+Shift+Z. History shows up to 50 session
  transactions. New edits clear redo; project changes/import reset the history boundary.
- Zoom the timeline from 1× through 8× and pan using its bottom navigation bar.
  V selects/scrubs; C activates a Razor that splits at the clicked frame.
- Toggle inset framing guides in Program with Safe. These are panel-relative guides,
  rather than a calibrated title-safe overlay on the media aspect rectangle.
- Lock/unlock, show/hide, or mute/unmute the actual selected track row. Track headers
  and clips scroll together. Video preview follows the first visible video track.
- Choose an output MP4 destination, export, cancel, retry failures, and clear a
  terminal job. Export uses the existing YouTube 1080p single-video-track worker.

Timeline/track edits save a candidate project atomically before publishing the
new state to the interface. Original media stays untouched. Undo/redo also persists before
publishing the restored state, and a failed save restores the history entry. Frame requests run in background workers with bounded queues;
new scrubs replace pending work and stale replies cannot overwrite newer frames.
Decoder failures appear in the feedback bar instead of leaving a fake preview.

## Current limits

This is a functional desktop foundation, not a completed production editor.
Low-latency library-backed audiovisual playback, audio mixing, effects controls/rendering,
arbitrary docking layouts, saved custom workspace layouts, and export preset selection are not provided
by this interface. The UI deliberately omits simulated waveforms, meters, and
unsupported workspace controls. Export uses synchronized video/audio when an
unmuted audio track is present, falls back to video-only otherwise, and refuses
existing output files through the existing export implementation.

## Verification

`cargo test -p prism-studio --all-targets` covers the shell and native adapter,
including locked-track rejection, saved split/delete/timing edits, undo/redo, failed
history-save recovery, marked-source append, independent source/sequence playheads,
exact timing parsing, filtered media identities, and software rendering at 1440 × 900
and 1100 × 720. Rendering covers all four workflow screens plus History, Jobs,
Assembly, collapsed-inspector, and zoomed variants.
Set `PRISM_TEST_FFMPEG=1` to additionally assert real source/program fixture decoding and thumbnail generation.
Set `PRISM_UI_SNAPSHOTS=/tmp/prism-ui-snapshots` to export PPM render artifacts for
visual inspection. This does not require a live desktop or screen capture permission.
