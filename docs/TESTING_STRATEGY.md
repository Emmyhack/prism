# Testing strategy

Unit tests cover exact time, command validation, invariants, and migrations.
Property tests will generate fractional rates, edit sequences, and command/undo
pairs. Integration tests cross project, commands, persistence, evaluator, and
script protocol boundaries. End-to-end tests use redistributable media fixtures
and independently probe exports. Fuzz targets cover project files, protocol
messages, manifests, and metadata adapters. Golden video/audio tests declare
numeric tolerances, color space, hardware backend, and reference provenance.

CI must execute `fmt`, Clippy with warnings denied, unit/integration tests, SDK
conformance, dependency advisories, and license policy. Platform-native smoke
jobs are required before support claims.

Current result (2026-11-04): thirty-seven default tests pass, including the first
generated rational-time property test. Strict Clippy and formatting are required;
broader property, integration, fuzz, regression, and media tests remain. Two
media integration tests and one playback integration test pass against an actual
synthetic MP4/AAC fixture using explicitly supplied local ffprobe and FFmpeg executables. They
verify header metadata, packet-level timing evidence, the fixture's full-file
SHA-256 fingerprint, and pinned RGBA frame hashes at two exact seek timestamps.
The playback test additionally proves that timeline placement and source trim
resolve to the expected pinned source frame.
Deterministic concurrency tests coordinate the asynchronous decode worker with
channels rather than timing assumptions. They verify successful delivery,
bounded request backpressure, and suppression of queued and in-flight results
after a playback-generation change.
The local FFmpeg build is a reference tool, not an approved distribution
dependency.
