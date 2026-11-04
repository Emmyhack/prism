# Feature status

Status vocabulary: Planned, In development, Implemented, Tested, Production
ready, Blocked. "Implemented" never implies the later states.

| Feature | Status | Evidence / limitation |
|---|---|---|
| Rust workspace | Tested | Builds with pinned Rust 1.94.1 |
| Exact rational time | Tested | Three unit tests pass; property tests pending |
| Typed IDs/project model | Tested | Domain validator covers identity, media references, time ranges, ordering, and overlap; object set remains incomplete |
| Typed command API | In development | Atomic batches, revision conflicts, dry-run, undo/redo, and core insert/move/trim/split/delete commands tested |
| Script protocol | In development | Exact-time DTOs for current commands, API/revision checks and dry-run intent; no runtime |
| Persistence | Tested | SQLite v1 round trip, forward-version refusal, SQLite/domain integrity checks and Unix atomic-save path tested; migrations/autosave pending |
| Media inspection | In development | Typed ffprobe reference adapter verified against one synthetic MP4/AAC fixture; decode/seek support not established |
| Editing/playback/export | Planned | No implementation |
| Studio UI | Planned | Toolkit decision provisional; binary is truthful stub |
| Python/JS/TS runtimes | Planned | Architecture only |
