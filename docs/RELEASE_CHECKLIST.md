# Release checklist

Prism is releasable only when every required gate below has evidence for the
target platform. A green build on one development machine is not cross-platform
release evidence.

## Automated qualification

- Run `scripts/release-check.sh` with `PRISM_FFMPEG`, `PRISM_FFPROBE`, and
  `PRISM_FFPLAY` set to the audited runtime binaries. The gate verifies the
  required libx264, AAC, and MP4 capabilities before running real-media tests.
- Require formatting, strict Clippy, all workspace tests, real-media tests, and
  optimized Studio/CLI builds to pass from the locked dependency graph.
- Require the macOS, Windows, and Linux CI matrix to pass for a platform before
  listing that platform as supported.
- Archive the CI run URL, source revision, dependency metadata, and compiler
  version with the release record.
- Require the embedded `RELEASE_BUILD.txt` to identify the exact revision,
  compiler and target and to report `source_dirty=false`. Local preview packages
  may report a dirty tree, but must never be promoted as release artifacts.

## Distribution qualification

- Build the macOS bundle with `scripts/package-macos.sh`. Set
  `PRISM_CODEC_DIR` only to a reviewed directory containing executable
  `ffmpeg`, `ffprobe`, and `ffplay` binaries; otherwise the app resolves the
  explicit `PRISM_FFMPEG`, `PRISM_FFPROBE`, and `PRISM_FFPLAY` environment
  variables before falling back to `PATH`.
- Preserve the generated versioned ZIP as the distribution artifact. The build
  creates a complete ICNS icon, registers `.prism` documents, embeds dependency
  and third-party-license inventories, signs the app, and archives resource
  forks. Every platform package emits a SHA-256 manifest. CI validates the
  checksum, plist/signature where applicable, archive, and embedded CLI.
- Replace ad-hoc signing with `PRISM_CODESIGN_IDENTITY` for distribution builds;
  notarize and staple the final artifact outside this repository.
- Inventory every shipped encoder, muxer, native library, license, and build
  flag. Do not distribute an arbitrary local FFmpeg build.
- Complete codec patent and market-specific legal review before commercial
  distribution.
- Produce signed Windows and tested Linux packages before claiming support for
  those platforms.
- CI builds and smoke-tests unsigned portable Windows and Linux archives with
  `scripts/package-windows.ps1` and `scripts/package-linux.sh`. Distribution
  signing and clean-machine qualification remain separate approval gates.

## Product smoke test

- Clean install and first launch.
- Create, save, Save As, close, reopen, autosave, and recover a project.
- Import representative video/audio, relink missing media, generate and switch
  proxies, and verify cache cleanup.
- Perform linked A/V split, move, trim, ripple delete, undo, and redo.
- Scrub and play through clips, cuts, gaps, and audio boundaries.
- Export video-only and synchronized A/V fixtures; independently probe duration,
  streams, geometry, and frame rate.
- Verify unsupported layers, mixes, or transitions are blocked explicitly rather
  than silently omitted.
- Exercise keyboard-only navigation, compact layout, and minimum window size.
- Pause/quit during decode, proxy, cache, autosave, and export work; verify no
  orphan worker or playback process remains.

## Known release blockers

- Signed/notarized macOS distribution and clean-machine installation evidence.
- Windows and Linux package/smoke-test evidence.
- Audited distributable codec runtime and completed legal review.
- Persistent low-latency A/V decode/device pipeline with measured synchronization.
- Preview/export agreement for real transitions, multiple visible video layers,
  and multitrack audio mixing.

These blockers must not be relabeled as production-ready merely because the
foundation APIs or interface controls exist.
