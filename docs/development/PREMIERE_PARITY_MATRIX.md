# Premiere familiarity and capability coverage

Baseline: 2026-11-19. Visual reference: the supplied Premiere Pro 2023 Home,
Import, Edit and Export screenshots. Functional reference: Adobe's current public
help. Prism is not yet a Premiere replacement or a production-qualified editor.

This is a capability-family inventory, not an exhaustive enumeration of every
Premiere command, effect, format or preference. Unlisted items are **unverified**,
never implicitly complete. A release-specific command/effect/format catalog and
migration acceptance suite must expand this document before a parity claim.
Adobe service integrations and proprietary project/plugin formats require separate
interoperability work; familiar behavior does not establish binary compatibility.

Status: **Available** = usable Studio path, still subject to release qualification;
**Partial** = subset or engine foundation; **Missing** = no complete native workflow;
**Integration** = separate host/service/format compatibility work. Engine tests alone
do not make a Studio workflow available. See [feature evidence](../FEATURE_STATUS.md)
and [playback acceptance](PLAYBACK_ACCEPTANCE.md).

| Area / capability | Status | Present scope and completion gate |
|---|---|---|
| Home, recent projects, new/open/save/save as | Available | Verify moved media, recovery and long sessions on supported systems. |
| Import/Edit/Export navigation | Available | Familiar workflow shell; does not imply all Premiere panels exist. |
| Project/media panel | Partial | Thumbnails, filtering/sorting and metadata; hierarchical bins, search bins, labels and configurable metadata columns pending. |
| Media Browser, device ingest and spanned clips | Missing | Browse camera packages, preserve metadata and validate partial/offline imports. |
| Source and Program monitors | Partial | Independent source scrub and Program playback; source transport, complete overlay/button editors and monitor context commands pending. |
| Arbitrary panel docking, floating windows and monitor layouts | Missing | Persist tabs, splits, floating geometry and recover inaccessible windows. |
| Panel maximize and panel focus shortcuts | Missing | Focus-specific command routing and accessible navigation required. |
| Editing/Assembly saved layouts | Available | Fixed layouts and panel sizing; not arbitrary workspace parity. |
| Native application menu and complete context menus | Missing | Central command catalog with enabled state, matching shortcut and context. |
| Searchable command launcher | Partial | Curated working commands; not complete command catalog. |
| Premiere-oriented keyboard preset | Partial | Native Edit subset, visible Keys panel, retained Prism preset; no custom keymap editor/import or complete focus-aware bindings. |
| Shortcut persistence | Available | Stored with explicit Save workspace layout; legacy saved views default to Premiere preset. |
| Selection, Razor, Slip, Slide, Rolling tools | Partial | Existing pointer/precision paths; complete modifier, selection and trim-monitor semantics need acceptance tests. |
| Ripple Edit, Rate Stretch, Track Select, Hand, Pen, Type tools | Missing | Familiar keys reserved; must not invoke unrelated tools. |
| Timeline zoom/pan, fit, snap and safe guides | Partial | Basic controls; full magnetic target rules and monitor guide editing pending. |
| Multiple selection, marquee and selection modifiers | Missing | Cross-track selection, ranges and predictable platform modifiers. |
| Clip clipboard: cut/copy/paste/paste attributes | Missing | Preserve relationships, effects, source offsets and destination targeting atomically. |
| Source In/Out and append | Available | Source-only marks and atomic marked-range append; comma is not append in Premiere preset. |
| Insert and Overwrite | Missing | Comma/period reserved; implement source patching, track targeting, sync-lock rules and collision handling before enabling. |
| Sequence In/Out, Lift and Extract | Missing | Distinct monitor focus and range semantics; undo must restore all affected tracks. |
| Track targeting and source patching | Missing | Explicit independent video/audio routing, track targeting and sync-lock. |
| Add Edit | Partial | Selected clip/unique aligned pair; full targeted-track semantics pending. |
| Add Edit to All Tracks | Available | Ctrl/Cmd+Shift+K splits crossing clips on all unlocked tracks in one saved undo transaction, including muted/hidden tracks. |
| Clear and Ripple Delete | Partial | Selected clip/unique aligned pair; full selection, sync-lock and gap semantics pending. |
| Persistent Link/Unlink and grouping | Missing | Current aligned timing inference is not persistent link identity. |
| Track lock, visibility, mute and reorder | Available | More complete header controls, audio solo, targeting and track management remain pending. |
| Track creation/deletion, track height and channel configuration | Partial | Model/command foundations; complete native track configuration workflow pending. |
| J/K/L shuttle, reverse and variable speed | Partial | K stops, Space toggles; J/L and shuttle combinations pending. |
| Frame and edit navigation | Partial | Native frame/edit/start/end commands; targeted-track and full modifier semantics pending. |
| Program Match Frame | Available | F chooses visible video beneath playhead, resolves source offset and rejects gaps. |
| Reverse Match Frame, match across sequences | Missing | Locate all occurrences with clear sequence/instance choice. |
| Precision trim and trim monitor | Partial | Frame-based trim/slip/slide/roll controls; dedicated two-up trim playback/looping pending. |
| Speed/duration, reverse, frame hold and time remapping | Missing | Model, source-time mapping, audio treatment, preview and export must agree. |
| Multiple open sequences, nesting and subsequences | Partial | Project supports sequence records; full native active-sequence/nesting workflow pending. |
| Multicam synchronization and switching | Missing | Sync methods, angle monitor, live cuts and flattening/export. |
| Clip/sequence markers | Partial | Sequence markers and editable list; clip markers, duration/colors and complete navigation pending. |
| Undo/redo and History | Available | Atomic saved edits, bounded session history; cross-session edit history pending. |
| Media relinking and offline state | Available | Single-source repair; bulk relink, metadata matching and offline delivery qualification pending. |
| Proxy generation, assignment and switching | Partial | Background jobs and fingerprint checks; ingest presets and broad camera-format qualification pending. |
| Media cache and background jobs | Partial | Derived cache cleanup and supervised workers; full resource scheduling/preferences pending. |
| Playback reliability and A/V sync | Partial | Continuous decoder and audio clock foundation; device latency, long media and broad format qualification remain open. |
| Scrub audio, native device I/O and external monitoring | Missing | Low-latency playback, output routing and supported-device acceptance. |
| Mixed codecs, VFR, interlace, image sequences and stills | Partial | FFmpeg inspection/decoding foundation; explicit supported-format matrix and edge-case qualification required. |
| Sequence frame rates and timecode | Partial | Rational time and sequence-rate work; DF/NDF, mixed-rate conversion and UI/export agreement need coverage. |
| GPU acceleration and full-resolution rendering | Partial | Engine foundations are not a qualified end-to-end accelerated pipeline. |
| Effects browser and Effect Controls | Missing | Search, apply/reorder/bypass, parameters, reset and effect persistence. |
| Motion, opacity, blend modes and compositing | Partial | Engine foundations; complete native keyframed editing plus preview/export equality pending. |
| Keyframe timeline, interpolation and easing | Partial | Model foundations; full native authoring and evaluated output pending. |
| Masks, feathering, tracking and rotoscoping | Missing | Author/edit/animate/track with export verification. |
| Video/audio transitions | Partial | Dissolve model/UI; export blocks unsupported transitions instead of silently dropping them. |
| Lumetri-style color controls, LUTs and curves | Partial | Color engine foundation; complete grading panels and output integration pending. |
| Color scopes | Partial | Sampled frame histogram/waveform/vectorscope; calibrated full-resolution scopes pending. |
| Color management, HDR and tone mapping | Partial | Explicit source/working/output transforms and reference-image validation required. |
| Titles, text, shapes and graphics templates | Missing | Type/graphics authoring, font handling, animation and rendered output. |
| Captions and subtitle editing | Missing | Tracks, styles, timing, import/export and burn-in/sidecar/embedded outputs. |
| Transcription and text-based editing | Missing | Language/speaker workflow, transcript corrections and reversible timeline edits. |
| Audio clip and track mixers | Partial | DSP/routing foundation; native faders, automation, effects slots and audible multitrack mixing pending. |
| Audio meters and loudness normalization | Partial | Peak/RMS/LUFS engine utilities; realtime calibrated panel and export conformance pending. |
| Audio gain/pan, EQ, dynamics, fades and repair | Partial | DSP primitives do not constitute full native effect workflows. |
| Audio channel mapping, buses and multichannel delivery | Partial | Routing foundations; native configuration and end-to-end channel validation pending. |
| Essential Sound-style workflows | Missing | Dialogue/music/SFX tagging, ducking, repair and editable automation. |
| Export workspace and queue | Partial | Typed 1080p/4K paths, preflight and job retries; complete preset/range/codec/audio/color controls pending. |
| Preview/output agreement | Partial | Must verify every supported effect, transition, transform, track and source-time mapping in exported pixels/audio. |
| Captions, metadata, timecode and multiple deliverables | Missing | Round-trip metadata, embedded/sidecar outputs and batch validation. |
| Smart rendering, preview reuse and background render | Missing | Cache invalidation, frame identity and lossless reuse qualification. |
| Project Manager / collect and consolidate | Missing | Portable media collection, trimmed handles and missing-file reports. |
| XML, AAF, EDL, OMF and project interchange | Integration | Versioned supported subsets and real round-trip fixtures; no .prproj compatibility claim. |
| After Effects / Dynamic Link / template integration | Integration | Separate compatible integration; no proprietary-host compatibility claim. |
| VST/third-party video effects and extension panels | Integration | Manifest foundation exists; actual plugin hosts, lifecycle and compatibility pending. |
| Productions, shared projects and collaboration | Integration | Ownership, locking, conflict/recovery and shared-media workflows required. |
| Autosave, recovery and project integrity | Partial | Atomic persistence/recovery foundations; interruption, disk-full and migration qualification pending. |
| Preferences, accessibility and localization | Partial | Complete settings, screen-reader/keyboard coverage and localization pending. |
| Packaging, updates, crash reports and performance | Partial | Signed distribution, supported systems and measured long-project release gates pending. |

## Execution order and acceptance

1. Stabilize playback and ensure every exposed edit survives save/reopen and undo.
2. Complete familiar edit semantics: selection/clipboard, persistent linking,
   targeting/patching/sync-lock, then Insert/Overwrite, sequence ranges and J/K/L.
3. Establish an evaluated render graph shared by preview and export before exposing
   effects, keyframes, compositing, speed changes and transitions as deliverable.
4. Deliver graphics/captions, mixing/color workflows and export/interchange coverage.
5. Qualify long projects, devices, formats, recovery, accessibility and distribution.

Each capability needs a discoverable working UI, correct disabled states, keyboard
and menu agreement, persistence, undo where applicable, failure handling and real
media acceptance evidence. A panel mockup or reserved key never closes a row.
Preserve Prism's visual identity through restrained violet accents and branding;
keep editing terminology, icon meaning and interaction conventions familiar.

## References

- [Adobe default keyboard shortcuts](https://helpx.adobe.com/premiere/desktop/get-started/keyboard-shortcuts/default-keyboard-shortcuts.html)
- [Tools and options panels](https://helpx.adobe.com/premiere/desktop/get-started/tour-the-workspace/tools-panel-and-options-panel.html)
- [Rate Stretch](https://helpx.adobe.com/premiere/desktop/edit-projects/change-clip-speed/change-clip-speed-and-duration-using-the-rate-stretch-tool.html)
- [Rolling edits](https://helpx.adobe.com/ie/premiere/desktop/edit-projects/trim-clips/perform-rolling-edits.html)
- [Export settings overview](https://helpx.adobe.com/premiere/desktop/render-and-export/export-files/overview-of-export-settings.html)
