# ADR-002: Media decoding and encoding

Status: Provisional. Use FFmpeg libraries through a narrow Rust adapter, with
platform hardware acceleration selected by measured capability and software
fallback. Prefer dynamic LGPL-compatible distribution; GPL/nonfree components
are excluded from default builds. Each packaged codec requires decode, seek,
sync, metadata, and export tests. Apple VideoToolbox integration may supplement
FFmpeg where zero-copy and licensing justify it. Native parsers remain untrusted
FFI and should be isolated where latency permits.
