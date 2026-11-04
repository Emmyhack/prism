# Feature status

Status vocabulary: Planned, In development, Implemented, Tested, Production
ready, Blocked. "Implemented" never implies the later states.

| Feature | Status | Evidence / limitation |
|---|---|---|
| Rust workspace | Tested | Builds with pinned Rust 1.94.1 |
| Exact rational time | Tested | Exact arithmetic, explicit tick rounding, and a commutativity/reversibility property test pass |
| Typed IDs/project model | Tested | Domain validator covers identity, media references, time ranges, ordering, and overlap; object set remains incomplete |
| Typed command API | In development | Atomic batches, revision conflicts, dry-run, undo/redo, and core insert/move/trim/split/delete commands tested |
| Script protocol | In development | Exact-time DTOs for current commands, API/revision checks and dry-run intent; no runtime |
| Persistence | Tested | SQLite v1 round trip, forward-version refusal, SQLite/domain integrity checks and Unix atomic-save path tested; migrations/autosave pending |
| Media inspection and reference decode | In development | Typed ffprobe inspection, packet timing, fingerprinting, and exact-seek RGBA reference vectors verified against one synthetic MP4/AAC fixture; production decoder worker pending |
| Timeline playback evaluation | In development | Exact source planning drives a bounded background decoder and reproduces a pinned real frame; queue backpressure and stale-generation suppression tested; presentation pending |
| Export | In development | Validated MP4 output supports one source range or one video track with exact trims and black gaps; command-built saved/reopened projects preserve export plans and timing; audio mixing, cancellation, presets, effects, and production codec packaging pending |
| Headless CLI | In development | No-clobber `create-project`, versioned JSON `inspect`, and typed `export-track` are tested; media/sequence/track/clip editing commands pending |
| Studio UI | Planned | Toolkit decision provisional; binary is truthful stub |
| Python/JS/TS runtimes | Planned | Architecture only |
