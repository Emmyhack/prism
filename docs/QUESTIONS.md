# Open questions

## Task 6 clip-transform semantics

Which coordinate and evaluation model should be authoritative for persisted clip
transforms and shared preview/export rendering?

Task 6 requires position, scale, rotation, anchor point, opacity and crop, but the
brief and current architecture do not define the following compatibility-critical
semantics:

- whether position and anchor use output pixels, source pixels or normalized
  coordinates, and where their origins lie;
- whether scale is uniform or two-axis, whether `1` or `100` is identity, and
  whether aspect ratio is locked by default;
- whether positive rotation is clockwise or counterclockwise and which coordinate
  system it uses;
- whether crop values use pixels, percentages or normalized edges, and whether crop
  occurs before or after anchor/scale/rotation;
- the complete transform order and sampling/border policy; and
- whether an untouched clip defaults to fit, fill or source-size presentation when
  source and sequence dimensions differ.

These choices must be fixed before adding the domain object or schema migration:
changing them later would silently alter saved projects and could make preview and
export disagree. A proposed default for approval is output-pixel position with a
top-left origin; normalized source-space anchor and crop edges; two-axis scale with
`1.0` identity and aspect lock in the UI only; clockwise degrees; crop, anchor,
scale, rotation, then position; transparent borders with bilinear sampling; and the
existing fit-with-letterbox behavior for untouched clips.

## GitHub Actions billing lock

Can the repository owner clear the GitHub Actions billing lock so the Linux,
Windows and macOS CI matrix can run?

Task 1 PR #2 received no CI execution: GitHub attached the exact annotation
`The job was not started because your account is locked due to a billing issue.`
The jobs contain zero steps and no logs. The required format, Clippy, full
workspace test and ignored real-media gates passed locally on the documented
macOS environment, but cross-platform CI could not run for this external reason.

Task 3 PR #4 repeated the billing lock for Ubuntu and Windows. Its macOS job
also did not start and reported the exact independent annotation
`The configuration 'macos-14-arm64-us-default' is not supported`. Can the
repository owner select a supported macOS runner configuration while clearing
the billing lock? The Task 3 format, warnings-denied Clippy, full workspace,
SDK, and all ignored real-media gates passed locally.

## Playback hardware qualification

What approved measurement method and hardware should define end-to-end Studio
scrub latency and ten-minute physical A/V drift?

The current automated acceptance gate uses FFplay with SDL's dummy audio driver.
It verifies monotonic clock behavior and exact decoded sample counts but cannot
observe speaker/display output. The Studio preview path also has no timestamp
probe spanning pointer input, scheduling, decode completion and presentation.
Task 0 now records request-to-decoded-frame latency through Studio's real preview
worker and a ten-minute FFplay audio-master drift measurement. It also verifies
that video timeline scheduling uses that exact audio-master position. These are
reproducible software-path baselines, but they do not replace speaker/display
measurement. This question is not a Task 0 blocker; it is a required playback
qualification decision for the Task 1 ADR and Task 8 design.
