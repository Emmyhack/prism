# Technical decisions

Architecture records in `docs/decisions` are provisional until their acceptance
criteria are demonstrated. Current selections: Slint UI, FFmpeg media libraries,
wgpu render backend, rational timeline time, SQLite project storage, supervised
Python workers, supervised `deno_core` JavaScript workers with ahead-of-run
TypeScript compilation, Wasmtime for capability-limited portable plugins, OCIO
for interchange color transforms, a dedicated realtime audio graph, native
platform packaging, and an LGPL-compatible FFmpeg distribution baseline.

These choices optimize Rust ownership and cross-platform portability while
keeping codec, runtime, UI, and persistence adapters replaceable.
