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

Current result (2026-11-04): forty-six default tests pass, including the first
generated rational-time property test. Strict Clippy and formatting are required;
broader property, integration, fuzz, regression, and media tests remain. Two
media integration tests and two playback integration tests pass against an actual
synthetic MP4/AAC fixture using explicitly supplied local ffprobe and FFmpeg executables. They
verify header metadata, packet-level timing evidence, the fixture's full-file
SHA-256 fingerprint, and pinned RGBA frame hashes at two exact seek timestamps.
The playback test additionally proves that timeline placement and source trim
resolve to the expected pinned source frame. A second playback test sends that
evaluated request through the background scheduler and verifies the decoded
result without performing media work on the calling thread.
Deterministic concurrency tests coordinate the asynchronous decode worker with
channels rather than timing assumptions. They verify successful delivery,
bounded request backpressure, and suppression of queued and in-flight results
after a playback-generation change.
An opt-in export integration encodes an exact source range to a temporary MP4,
independently probes the finalized output, and decodes a frame from it. This is
a local reference-codec test, not a distribution support claim.
Export-plan unit tests verify exact clip trim/placement preservation, explicit
leading and inter-clip gaps, video-track type enforcement, and malformed-layout
rejection before any encoder process starts.
The second export integration renders two trimmed clip ranges with leading and
inter-clip black gaps, verifies duration within one output-frame tolerance, and
decodes both gap and content frames from the finalized video-only MP4.
A third export integration builds a timeline exclusively through validated
commands, atomically saves and reopens the SQLite project, compares the complete
export plan for equality, and renders the reopened state within one frame of its
exact planned duration.
The executable-level CLI integration launches the compiled binary against a
command-built saved project, selects stable typed IDs, and independently probes
the resulting MP4. Parser tests reject missing, duplicate, and malformed options.
The same executable test first inspects the project as versioned JSON and verifies
the reported IDs before export. Unit coverage checks exact rational time fields
remain integer numerator/timescale pairs at the JSON boundary.
An executable-level project-creation test verifies atomic creation, inspection of
the empty saved state, and refusal to replace an existing project. It reinspects
the original after refusal to prove the existing file was not mutated.
The second real-media CLI test creates a project, probes actual media, commits
media/sequence/track/clip creation as one revision, and verifies exact source
start and duration through the inspection JSON contract.
The third executable CLI test appends a second trimmed clip to the existing
video track, moves it through a second command invocation, and verifies the
new exact source and timeline positions.
The local FFmpeg build is a reference tool, not an approved distribution
dependency.
