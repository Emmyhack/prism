# Open questions

## GitHub Actions billing lock

Can the repository owner clear the GitHub Actions billing lock so the Linux,
Windows and macOS CI matrix can run?

Task 1 PR #2 received no CI execution: GitHub attached the exact annotation
`The job was not started because your account is locked due to a billing issue.`
The jobs contain zero steps and no logs. The required format, Clippy, full
workspace test and ignored real-media gates passed locally on the documented
macOS environment, but cross-platform CI could not run for this external reason.

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
