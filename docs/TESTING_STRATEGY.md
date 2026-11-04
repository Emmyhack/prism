# Testing strategy

Unit tests cover exact time, command validation, invariants, and migrations.
Property tests will generate fractional rates, edit sequences, and command/undo
pairs. Integration tests cross project, commands, persistence, evaluator, and
script protocol boundaries. End-to-end tests use redistributable media fixtures
and independently probe exports. Fuzz targets cover project files, protocol
messages, manifests, and metadata adapters. Golden video/audio tests declare
numeric tolerances, color space, hardware backend, and reference provenance.

CI must execute `fmt`, Clippy with warnings denied, unit/integration tests, SDK
conformance (Python and JavaScript currently run in the SDK job), dependency advisories, and license policy. Platform-native smoke
jobs are required before support claims.

Current result (2026-11-04): forty-seven default tests pass, including the first
generated rational-time property test. Strict Clippy and formatting are required;
broader property, integration, fuzz, regression, and media tests remain. Two
media integration tests and two playback integration tests pass against an actual
synthetic MP4/AAC fixture using explicitly supplied local ffprobe and FFmpeg executables. They
verify header metadata, packet-level timing evidence, the fixture's full-file
SHA-256 fingerprint, and pinned RGBA frame hashes at two exact seek timestamps.
Timeline unit tests additionally cover nearest-grid snapping, negative times,
and ties-away-from-zero at fractional-frame boundaries.
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
Preset tests verify stable YouTube 1080p/4K identifiers, exact dimensions and
frame rates, request construction, and unknown-preset rejection; they do not
claim bitrate or codec-policy coverage.
Audio export-plan tests additionally verify exact silence gaps, source trims,
duration, muted-track rejection, and stable labeled FFmpeg audio-filter output
without launching an encoder. Combined video/audio export preflight verifies
that mismatched track durations are rejected before an encoder process starts;
argument coverage verifies that the combined encoder path maps both filtered
streams and selects AAC; an opt-in local FFmpeg fixture now encodes the
combined path and independently probes the finalized audio stream.
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
video track, moves, trims, splits, and deletes it through subsequent command
invocations, and verifies the resulting exact source and timeline positions.
Audio clip insertion shares the same executable command workflow and rejects
media without an audio stream before any project mutation.
Script-host tests exercise bounded stdout capture, cancellation, and the
trusted Python executable path; the CLI workflow is also shared by the Node
JavaScript path and the explicit compiler-to-Node TypeScript path. The
resulting transaction is inspected after commit to prove it crossed the same
command and persistence boundary.
The same executable workflow submits a versioned JSON transaction in dry-run
mode and verifies that validation succeeds without an additional project save,
then commits a second transaction and verifies the persisted revision and exact
timeline position after reopening.
The stale dry-run request is then rejected after the committed revision
advances.
The local FFmpeg build is a reference tool, not an approved distribution
dependency.
Persistence tests also verify that a recovery backup loads correctly and never
replaces an existing backup artifact.
The CLI project workflow also exercises backup creation and refusal to replace
the same backup destination.
Autosave tests verify deterministic revision naming and no-clobber behavior.
Autosave scheduler tests additionally verify bounded background writes,
revision-scoped artifacts, duplicate-revision suppression, result reporting,
and worker shutdown using temporary project directories. Discovery tests verify
newest-first candidate ordering and isolation from malformed or directory
entries. Retention tests verify
oldest-first removal, newest-count preservation, and isolation from unrelated
or malformed filenames.
The executable CLI workflow also creates and refuses to replace a revision-scoped
autosave.
The CLI autosave-list workflow serializes recoverable candidates newest-first
through the same persistence discovery boundary.
Persistence and CLI recovery coverage validates a selected autosave through the
normal SQLite integrity/schema loader, restores it with no-clobber semantics,
and rejects a second recovery to the same destination.
CLI parser and executable coverage also verifies preset-driven video export
resolves the typed YouTube 1080p/4K settings before invoking the export engine.
The same CLI integration invokes a fake explicit FFmpeg executable through the
proxy-generation command and verifies the finalized artifact bytes, while the
media unit tests cover cancellation and no-clobber behavior.
Export unit coverage verifies pre-cancelled source-range and single-track
requests never launch an encoder, and a Unix process-level test terminates a
running fake encoder while removing temporary output. UI progress reporting and
portable process-signal integration remain future work.

Audio unit coverage verifies allocation-free stereo gain/pan processing,
multitrack and indexed-bus accumulation, fade/crossfade curves, stateful
one-pole filter cutoff validation and block/reset behavior, validation of malformed buffers and parameters, and
peak/RMS and dBFS analysis. Peak normalization tests also cover silence, finite-input
validation, and deterministic target scaling; this is not a claim of LUFS
compliance. Dynamics tests cover attack/release detector behavior, reset,
peak reduction, limiter ceilings, and non-finite input rejection. Resampler
tests verify explicit rate validation, interpolation,
caller-owned output, non-finite input rejection, and chunked streaming phase
continuity. Stereo delay tests verify parameter bounds, delayed signal timing,
reset behavior, and non-finite input rejection.

Plugin foundation tests round-trip a versioned manifest through JSON and reject
path-like identifiers, duplicate capabilities, and unsupported future schema
versions before runtime loading is possible.
Authorization tests also reject requested capabilities that are absent from the
host grant set. Catalog tests additionally verify deterministic directory
ordering, duplicate IDs, missing entries, and entry-path containment before
any runtime activation.
Cache-worker tests verify nonblocking bounded submission, background atomic
finalization, result delivery, and no-clobber behavior without running cache I/O
on the caller thread.

Recipe tests round-trip manifests and reject unknown, missing, incorrectly typed,
and out-of-range parameters before a script worker can start.
Script integration tests also verify valid recipe requests become execution
plans and invalid inputs are rejected before planning.
Recipe authorization tests reject missing host permissions before a worker plan
can be activated.

Script-worker tests verify payload and operation budgets, cancellation before
mutation, structured execution results, and identical dry-run versus commit
validation paths.

Command and SDK conformance coverage includes track-lock mutations, marker
operations, clip duplication, ripple deletion, and transitions so visual, CLI,
Python, JavaScript, and TypeScript interfaces can share the same state changes.

Playback and export tests verify hidden video tracks produce no video source
requests and muted audio tracks produce no audio source requests; export rejects
hidden selected tracks before invoking an encoder.

Audio tests verify routed mixing skips muted inputs while still rejecting malformed
buffer shapes. Filter tests verify low/high-pass state continuity and reset
behavior. Waveform tests verify deterministic bucket extrema, RMS values,
partial final buckets, zero-window rejection, and non-finite sample rejection.

Render-graph CPU tests verify parameterized color-gain, opacity, and blend-mode
operations, including normalized output, missing-operation rejection, and
preservation of the existing unsupported-node behavior for unimplemented effects.

Cache tests verify deterministic content-derived keys, operation/parameter
separation, fixed digest formatting, sharding, and rejection of path-like
extensions.
Atomic cache tests also verify finalized bytes are preserved and existing
artifacts are never replaced; finalization uses a same-filesystem atomic
create-if-absent link. Eviction tests verify explicit byte/age policy behavior
and ensure temporary files are not treated as finalized artifacts. Background
cache tests verify bounded write and eviction scheduling, result reporting, and
application of explicit eviction policies.
Media tests also verify fingerprints partition thumbnail and waveform cache
operations deterministically. RGBA thumbnail tests verify nearest-neighbor
sampling, exact output sizes, and malformed-frame rejection.
Proxy profile tests verify nonzero dimensions, filename-safe codec/extension
identifiers, canonical cache parameters, and distinct cache keys for changed
profiles. Proxy worker tests use an explicit fake encoder to verify temporary
output finalization, no-clobber behavior, cancellation, and cleanup. Cache
lookup tests distinguish finalized entries from misses.

Color tests verify normalized-channel validation, endpoint behavior, numerical
round trips for the implemented sRGB and Rec.709 transfer curves, and bounded
linear-RGB grading controls with deterministic clamping, 1D LUT tests verify
interpolation and channel application, and 3D LUT tests verify cube validation
and trilinear identity interpolation; they do not claim HDR or
primaries-conversion coverage. Histogram tests verify exact RGB and Rec.709
luma bin counts and invalid-sample rejection; waveform tests verify column
preservation, row dimensions, and invalid-sample rejection; vectorscope tests
verify deterministic U/V bins and invalid-sample rejection.

Compositor tests verify alpha-weighted source-over equations, opaque
Multiply/Screen/Additive/Overlay results, transparent-layer behavior, invalid-channel rejection, and
bounded cross-dissolve progress/interpolation.

Render-graph tests verify typed node metadata, kind-specific input arity,
deterministic dependency-first ordering, missing and duplicate input rejection,
explicit cycle detection, and CPU source/blend/transition/output evaluation;
unsupported resource nodes are rejected rather than silently passed through.

Automation-curve tests verify exact-time insertion ordering, endpoint clamping,
linear interpolation, and rejection of duplicate, negative, or non-finite
keyframes.

Marker coverage verifies exact-time sorting, duplicate-ID and invalid-label
rejection, command deletion, SQLite round trips and migrations, CLI inspection,
and equivalent Python, JavaScript, TypeScript, and Rust protocol payloads.
