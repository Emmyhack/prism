# ADR-008: Plugin sandboxing

Status: Proposed. Use Wasmtime components for portable, capability-based plugins
that do not require direct realtime GPU/audio access. WASI capabilities default
to none; fuel, epoch deadlines, memory, and output limits are mandatory. Trusted
native plugins require a separate signed, opt-in, process-isolated architecture.
WebAssembly is not assumed suitable for every effect or realtime callback.
