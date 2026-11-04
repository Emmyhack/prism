# Prism

Prism is a professional, cross-platform, programmable nonlinear video editor
implemented in Rust. The project is in **Phase 0: architecture and foundation**.
It is not yet capable of editing or exporting media.

## Current foundation

- exact rational timeline time with checked arithmetic;
- typed project-object identifiers and a UI-independent domain model;
- a typed command boundary used by all future interfaces;
- a versioned, language-neutral scripting protocol boundary;
- architecture, security, testing, and milestone documentation.

## Prerequisites

Install Rust 1.94.1 with rustfmt and Clippy. Media development will additionally
require an approved FFmpeg build; its distribution configuration is not yet
locked because codec patent and license review remains open.

```sh
rustup toolchain install 1.94.1 --profile minimal --component rustfmt,clippy
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

See [the implementation plan](docs/IMPLEMENTATION_PLAN.md) and
[feature status](docs/FEATURE_STATUS.md). No current feature is production ready.
