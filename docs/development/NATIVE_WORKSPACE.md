# Native editing workspace

Launch with `cargo run -p prism-studio`, or pass `-- --open-project ./edit.prism`.
The configured or packaged FFmpeg runtime is required for media import,
previews, audio playback, and export.

The interface interprets the supplied Premiere Pro references through four workspaces:
Home for project entry, Import for footage selection, Edit for paired monitors and
an editable timeline, and Export for destination settings and job feedback. Prism
retains its supplied ribbon logo, with graphite surfaces and violet selection cues.
The desktop minimum is 1100 × 720; drag the divider below the monitors to resize.
Editing and Assembly presets change the source-monitor/media-bin allocation.
Below 1250px, Source moves into the inspector and Program takes the available
monitor width. The labeled right rail opens Properties, History or Jobs; Inspector collapses
that region. Save layout remembers the preset, monitor height, panel tabs/visibility,
snapping, safe guides and media sort order for the current project in a versioned
`.workspace.json` sidecar. Opening the project restores it, with monitor height clamped
to the current window. Reset layout returns to defaults; Save layout commits that reset.
Invalid or future-version preferences fall back safely without overwriting the sidecar.
Workspace changes do not enter project edit history.

The media browser cycles between project order and ascending/descending file names.
Filtering and sorting preserve source identity. Cards identify missing originals and
show file size; Source properties report original-file availability and decoded preview
size, with direct relinking when a preview cannot be loaded.
Audio clips queue cache-first background waveform generation for their referenced source
ranges. Jobs reports this work, every audio clip lane projects bounded real peak data,
and selecting an audio clip renders a larger peak strip in Properties without blocking
the interface.

## Supported interactions

- Edit aligned A/V in Clip properties provides Split A/V, Delete A/V, Ripple delete A/V and an exact-time Move A/V together control. It requires
  exactly one opposite-kind companion with the same media ID, sequence start, source
  In and duration, and both tracks unlocked. Each edit is one atomic saved transaction
  and one undo step. Splitting uses the playhead; deleting leaves both gaps. This is
  explicit paired editing, not persisted Link/Unlink or automatic grouped gestures.
  Moving validates both destination ranges atomically, preserves source trims, and
  keeps the selected clip by its stable ID when track ordering changes.
  Paired Source In and Duration fields apply the same source range to both clips
  while keeping their sequence start fixed. The original media duration is inspected
  before commit, and overlap on either track rejects the entire edit.

- Save as… (Ctrl/Cmd+Shift+S) writes a new project file and makes it the active
  edit destination, preserving undo/redo. Existing files are rejected. Media remains
  referenced and project-scoped cache/autosave services restart for the copy.
- Timeline audio preview follows source trims and clip boundaries, stops in gaps,
  and starts the next audible clip. Preview currently plays one active audio clip;
  full multitrack mixing and sample-accurate cut playback remain pending.

- Match frame in Source (Shift+F) maps the selected timeline clip at the playhead
  to its original source time, respecting trimmed source In. When the playhead is
  outside that clip, it opens the clip’s source In instead. Locked clips can still
  be inspected. Source scrubbing respects the selected original/proxy mode.

- Up/Down jump to previous/next edit boundaries across visible video and unmuted audio
  tracks, including gap edges. Home/End jump to sequence bounds. Jumps and playback
  reveal the playhead when it leaves a zoomed view. Fit (F) restores the full timeline.

- Source monitors show a highlighted In/Out range with boundary ticks and a distinct
  playhead. Range duration is visible in wide and compact layouts; Full / Use full clip
  clears source marks without changing timeline clips or project edit history. Audio-only
  assets open as marked, appendable sources without requesting an invalid video frame.
  Append writes every stream the source owns as one undoable operation, keeping paired
  video/audio clips aligned at the complete sequence end.

- Commands (Ctrl/Cmd+Shift+P) searches editing actions, panel navigation and tools.
  Use Up/Down and Return, click a result, or press Escape to close. Unavailable actions
  explain their requirements and eligibility is rechecked before execution.
- Properties separates Clip and Source work. Clip timing and primary actions remain
  visible; advanced precision controls expand on demand. Source puts range marking
  and Append first, with relinking/proxies inside an expandable Media & proxies group.
- Hover toolbar controls for contextual help in the status bar. Text fields retain
  normal editing focus; closing Commands restores editor keyboard focus.
- The Markers tab lists sequence markers with timecodes, click-to-seek, and rename/
  delete controls. Renaming preserves marker identity and timing as one undoable edit.
- Pointer move and edge-trim gestures keep matching imported video/audio clips aligned
  and commit both changes in one undo step; locked or ambiguous pairs are refused.

- Create/open a local project; save with the header button or Ctrl/Cmd+S.
- When revision-scoped autosaves exist beside the current project, Home shows the
  newest discoverable candidate and its revision. Recovery validates it and writes to a new
  user-selected project path and never replaces the active project or autosave.
- Every successful edit, undo, and redo queues a validated snapshot on a bounded
  background autosave worker. Bursts coalesce to the newest pending revision, the Jobs
  panel reports status. Jobs can enable/disable autosave and cycle retention through
  1, 5, 10, or 20 revisions; these preferences persist atomically beside the project.
- Import one or more video or audio files with native file dialogs (Ctrl/Cmd+I).
  Video sources append synchronized video/audio clips when sound is present;
  audio-only sources append to the first unlocked audio track. A sequence and
  appropriate tracks are created when required. New projects prompt for a save
  destination. Cancellation leaves the project intact.
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
- Jobs reports the saved project's derived-cache artifact count and size. Clean
  derived cache runs a traversal-safe eviction pass in the background and reports
  completion or failure without blocking editing.
- Successfully decoded browser thumbnails are fingerprinted off the UI thread and
  written through the bounded project cache worker. Jobs includes pending cache writes,
  and completed artifacts immediately contribute to cache inventory. Thumbnail cache
  entries carry a versioned width/height header and validated RGBA payload so later
  cache-hit presentation never has to guess image geometry.
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
- Choose an output MP4 destination and a typed 1080p/4K UHD preset. Export shows
  selected-track video/audio summaries and blocks unequal audio/video end times.
  Jobs can be cancelled, retried, or cleared. Retry retains the original project
  snapshot and preset rather than silently applying new settings to a prior job.
  Settings scroll in compact windows while Export stays anchored at the bottom.

Timeline/track edits save a candidate project atomically before publishing the
new state to the interface. Original media stays untouched. Undo/redo also persists before
publishing the restored state, and a failed save restores the history entry. Frame requests run in background workers with bounded queues;
new scrubs replace pending work and stale replies cannot overwrite newer frames.
Decoder failures appear in the feedback bar instead of leaving a fake preview.
Playback waits for each pending frame decode before requesting another and retains
the last completed picture between frames. Explicit scrubs still supersede stale
requests. This prevents timer ticks from starving video while independent audio plays;
preview frame rate remains limited by the current per-frame FFmpeg decoder.

## Current limits

This is a functional desktop foundation, not a completed production editor.
Low-latency library-backed audiovisual playback, audio mixing, effects controls/rendering,
arbitrary docking layouts are not provided
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
Assembly, collapsed-inspector, zoomed, Source, precision, Markers, Commands (with/without results), and 4K Export variants.
Set `PRISM_TEST_FFMPEG=1` to additionally assert real source/program fixture decoding and thumbnail generation.
Set `PRISM_UI_SNAPSHOTS=/tmp/prism-ui-snapshots` to export PPM render artifacts for
visual inspection. This does not require a live desktop or screen capture permission.

See [the production roadmap](PRODUCTION_ROADMAP.md) for delivery stages and acceptance evidence.
