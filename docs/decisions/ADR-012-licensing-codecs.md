# ADR-012: Licensing and codec distribution

Status: Proposed; legal review required before commercial distribution. Project
code is Apache-2.0. Default FFmpeg builds must remain dynamically linked and
LGPL-compatible, excluding GPL and nonfree options. Codec presence is discovered
at runtime and never inferred from container support. Patent/royalty obligations
for H.264, HEVC, AAC, AV1, ProRes, and platform encoders require market-specific
review. CI will inventory dependencies, features, native build flags, licenses,
notices, and shipped binaries.
