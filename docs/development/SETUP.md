# Development setup

Prism pins Rust 1.94.1. Install it through rustup with rustfmt and Clippy, then
run the commands in the root README. Xcode command-line tools are required on
macOS. Phase 1 media work will add a reproducible FFmpeg configuration after the
license matrix is approved; installing an arbitrary FFmpeg package is not proof
that Prism can distribute its enabled codecs.

Environment discovery on 2026-11-03 found macOS 26.6.2 on arm64 with Apple
Clang 21, CMake 4.4.3, Node 26.6.0, and Python 3.14.7. Rust, pkg-config, FFmpeg,
and ffprobe were absent.

Studio resolves media tools in this order: explicit `PRISM_FFMPEG`,
`PRISM_FFPROBE`, and `PRISM_FFPLAY` paths; audited tools beside the executable
in portable Linux/Windows packages; audited tools embedded under the macOS
bundle's `Contents/Resources/bin`; then the process `PATH`. Finder-launched
builds should not rely on a developer shell's Homebrew path. Packaging accepts
an audited tool directory through `PRISM_CODEC_DIR`; legal and codec-feature
review remains mandatory before distributing those binaries.

Run `scripts/package-macos.sh`, `scripts/package-linux.sh`, or
`scripts/package-windows.ps1` for a platform-native portable release archive.
Every package includes Studio, the CLI, branding, dependency metadata, and
third-party notices. CI builds and smoke-tests all three archive formats.
