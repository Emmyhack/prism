#!/bin/sh
set -eu

workspace=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$workspace"

cargo_bin=${CARGO:-cargo}
ffmpeg_bin=${PRISM_FFMPEG:-ffmpeg}
ffprobe_bin=${PRISM_FFPROBE:-ffprobe}
ffplay_bin=${PRISM_FFPLAY:-ffplay}

"$ffmpeg_bin" -version >/dev/null
"$ffprobe_bin" -version >/dev/null
"$ffplay_bin" -version >/dev/null

run_playback_test() {
    test_filter=$1
    echo "playback-acceptance: $test_filter"
    PRISM_FFMPEG="$ffmpeg_bin" \
    PRISM_FFPROBE="$ffprobe_bin" \
    PRISM_FFPLAY="$ffplay_bin" \
    PRISM_TEST_FFMPEG=1 \
        "$cargo_bin" test -p prism-studio --all-targets --all-features --locked \
        "$test_filter" -- --ignored
}

run_playback_test continuous_decode_reuses_process_and_stops_it
run_playback_test continuous_audio_renders_exact_silence_between_trimmed_clips
run_playback_test real_player_reports_monotonic_audio_master_with_dummy_device
run_playback_test real_video_frames_complete_under_repeated_playback_ticks

echo "playback-acceptance: all playback qualification gates passed"
