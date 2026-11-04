# ADR-008: Plugin sandboxing

Status: Proposed for runtime; manifest and safe local catalog boundaries implemented. Use Wasmtime components for portable, capability-based plugins
that do not require direct realtime GPU/audio access. WASI capabilities default
to none; fuel, epoch deadlines, memory, and output limits are mandatory. Trusted
native plugins require a separate signed, opt-in, process-isolated architecture.
WebAssembly is not assumed suitable for every effect or realtime callback.
Catalog discovery validates manifests and resolves entries only within each
plugin package; discovery never loads or executes plugin code. Project, media,
export, script, filesystem, and network capabilities are explicit manifest
requests and require matching host grants; filesystem and network access remain
denied by default.
