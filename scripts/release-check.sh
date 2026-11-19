#!/bin/sh
set -eu

workspace=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$workspace"

cargo_bin=${CARGO:-cargo}

"$cargo_bin" fmt --all -- --check
"$cargo_bin" clippy --workspace --all-targets --all-features --locked -- -D warnings
"$cargo_bin" test --workspace --all-targets --all-features --locked

if [ -n "${PRISM_FFMPEG:-}" ] && [ -n "${PRISM_FFPROBE:-}" ]; then
    media_check=$(mktemp -d "${TMPDIR:-/tmp}/prism-media-check.XXXXXX")
    trap 'rm -rf "$media_check"' EXIT HUP INT TERM
    "$PRISM_FFMPEG" -hide_banner -encoders > "$media_check/encoders.txt"
    "$PRISM_FFMPEG" -hide_banner -muxers > "$media_check/muxers.txt"
    grep -q "libx264" "$media_check/encoders.txt"
    grep -q " aac " "$media_check/encoders.txt"
    grep -Eq " mp4 |,mp4" "$media_check/muxers.txt"
    "$PRISM_FFPROBE" -version >/dev/null
    "${PRISM_FFPLAY:-ffplay}" -version >/dev/null
    ./scripts/playback-acceptance.sh
    PRISM_TEST_FFMPEG=1 "$cargo_bin" test --workspace --all-targets --all-features --locked -- --ignored
else
    echo "release-check: real-media tests skipped; set PRISM_FFMPEG and PRISM_FFPROBE" >&2
fi

"$cargo_bin" build --release --locked -p prism-studio -p prism-cli
git diff --check
git diff --cached --check

echo "release-check: all available gates passed"
