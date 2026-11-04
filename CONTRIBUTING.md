# Contributing

All project mutations must use `prism-commands`; neither UI nor scripting code
may mutate `prism-project` directly. Keep serialization DTOs out of domain
crates. Every behavior change requires tests and documentation of limitations.

Before submitting a change, run formatting, Clippy with warnings denied, and all
workspace tests. Record any test that cannot run and its exact blocker.
