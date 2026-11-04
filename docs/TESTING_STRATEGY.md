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

Current result (2026-11-04): twenty-seven default tests pass, including the first generated
rational-time property test. Strict Clippy and formatting
are required; property, integration, fuzz, regression, and media tests remain.
One additional opt-in integration test passes against an actual synthetic
30000/1001 fps video plus 48 kHz audio fixture using ffprobe 9.0.2.
