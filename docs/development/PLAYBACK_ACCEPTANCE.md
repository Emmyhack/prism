# Playback acceptance evidence

This is Stage 1 evidence for the current Studio backend, not a production-readiness
certificate. Automated results below cover synthetic media and controlled processes;
they do not measure a user's display, speakers, driver or physical output latency.

| Requirement | Current evidence | Remaining qualification |
|---|---|---|
| Pause and shutdown stop audio | Native transport tests terminate and reap both decoder and player child processes | Installed-platform/device matrix |
| Forward video reuses decoding | Real fixture produces changing frames from one persistent FFmpeg process; stop reaps it | Long recordings, representative codecs and resource budgets |
| Cuts and gaps preserve audio duration | A real AAC fixture is decoded into a continuous PCM session; exact sample count, audible regions and silence are checked | Broader codecs, rate/channel combinations and hardware output |
| Audio drives transport | Native UI tests hold before the first clock report and advance from the reported audio time rather than elapsed wall time | Sample-accurate native device clock and measured physical A/V sync |
| Clock parsing is bounded and session scoped | Tests cover invalid timestamps, oversized lines, monotonic positions and a real SDL dummy-device session | Packaged FFplay version compatibility |
| Frozen clocks stop playback | Clock deadline measures forward progress; repeated or regressing reports cannot refresh it | Real-device underrun/recovery behavior |
| Missing video results cannot leave audio running indefinitely | Native deadline test simulates a lost result and verifies transport stop plus termination of both audio processes; completed replies are drained before the deadline check | Representative slow/corrupt/network-mounted media |
| Waveform project/worker teardown | A controlled decoder is killed/reaped after worker drop; queued work is not restarted. Native resets use a separate worker and result receiver | Long-source decoding, cache-write races and OS-blocked reads |
| Scrubs supersede old results | Generation tests reject stale replies and distinguish explicit seeks from paced playback requests | Repeated long-session seek stress |
| Preview subprocess cancellation | Controlled processes verify deadline/cancellation kill-and-reap; probe and RGBA APIs accept per-request cancellation; old tokens stay cancelled after restart | Broader platform/process-tree behavior and repeated seek stress |
| Natural audio completion | Transport handles player exit near the planned end rather than waiting forever for another clock report | A/V end-to-end device tests across supported backends |

## Preview supervisor limits

Metadata inspection and reference RGBA decoding now capture output through a child
supervisor. Existing entry points have a 30-second default deadline. Studio preview
requests use five seconds per probe/frame operation and distinct cancellation tokens;
a new seek, project change or worker drop cancels the old request. Continuous video
metadata probing uses the same token and deadline. Probe stdout is capped at 4 MiB,
frame stdout at the expected RGBA byte count, and diagnostic stderr at 64 KiB.

Cancellation/deadline/output-limit errors kill and reap the tool's direct child.
This is not a general process-tree sandbox. Packaging and platform qualification
must still address unexpected descendant processes and OS-level blocked I/O.

## Still open in Stage 1

- Replace coarse FFplay text telemetry with native audio-device timing.
- Persist and evaluate the sequence's native frame rate; preview currently uses 30 fps.
- Qualify supervised metadata, packet-timing, RGBA and reference-audio subprocesses
  on each platform and with slow or blocked media. Studio waveform extraction now
  cancels superseded fingerprint and decode stages, but blocked OS file reads and
  already-submitted cache writes remain cooperative/platform concerns.
- Measure drift, seek accuracy, presentation delay and dropped frames with long and
  variable-frame-rate recordings. The one-second fixture is insufficient evidence.
- Persist explicit link/unlink group identity rather than relying only on uniquely
  aligned imported clip pairs.
- Qualify audio devices, driver changes and packaged backend versions on each platform.

Until these are qualified, Stage 1 remains in development. Additional panels and a
green test suite do not close these requirements.

## Executable qualification

Run the focused acceptance gate from a checkout with FFmpeg, FFprobe and FFplay:

```sh
PRISM_FFMPEG=ffmpeg PRISM_FFPROBE=ffprobe PRISM_FFPLAY=ffplay \
  ./scripts/playback-acceptance.sh
```

The gate verifies four release-critical paths against the checked-in A/V fixture:

- one continuous video decoder is reused, cancelled and reaped within 10 seconds;
- trimmed audio plus a timeline gap produces the exact expected 48 kHz sample count
  and silence region within 15 seconds;
- FFplay's dummy output reports a monotonic audio-master position and exits within
  its five-second watchdog;
- repeated playback ticks produce video frames within the five-second decoder
  watchdog.

These tests are deliberately marked ignored so ordinary unit runs require no media
runtime. CI and `scripts/release-check.sh` opt into them with explicit tool paths.
The budgets are fixture watchdogs, not claims about physical device latency or
long-form playback quality.

Waveform fingerprint and audio decode stages each have a 30-second deadline and
check their request cancellation token. Hashing checks between 64 KiB reads; it
cannot interrupt an OS-blocked file read. Project resets replace the worker and
receiver, so late results cannot enter the new project. Cache writes already
submitted before cancellation may complete in the old content-addressed cache.
